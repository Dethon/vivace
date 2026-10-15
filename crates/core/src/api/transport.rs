use std::time::Duration;

use bytes::Bytes;
use http::{
    Method, Request, StatusCode,
    header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue},
};
use http_body_util::BodyExt;
use librespot_core::{Session, error::ErrorKind, http_client::HttpClient};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::Semaphore;

use crate::{Error, Result};

// Spotify's own clients keep a handful of requests in flight; staying at that
// level keeps the app responsive without looking like a scraper.
const MAX_CONCURRENT: usize = 4;
const ATTEMPTS: usize = 2;
const PATHFINDER: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const JSON: &str = "application/json";
// How much of a failed or undecodable response is logged for diagnosis.
const RESPONSE_LOG_BYTES: usize = 400;

/// A persisted GraphQL query of Spotify's web player, addressed by its hash.
pub(crate) struct Query {
    pub name: &'static str,
    pub hash: &'static str,
}

#[derive(Serialize)]
struct QueryBody<'a, V> {
    #[serde(rename = "operationName")]
    name: &'a str,
    variables: &'a V,
    extensions: Extensions<'a>,
}

#[derive(Serialize)]
struct Extensions<'a> {
    #[serde(rename = "persistedQuery")]
    persisted: Persisted<'a>,
}

#[derive(Serialize)]
struct Persisted<'a> {
    version: u8,
    #[serde(rename = "sha256Hash")]
    hash: &'a str,
}

#[derive(serde::Deserialize)]
struct Envelope<T> {
    data: T,
}

pub(crate) struct Transport {
    session: Session,
    permits: Semaphore,
}

impl Transport {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            permits: Semaphore::new(MAX_CONCURRENT),
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Calls an spclient endpoint, e.g. `/playlist/v2/playlist/<id>`.
    pub async fn spclient(
        &self,
        method: Method,
        endpoint: &str,
        content_type: Option<&'static str>,
        body: Bytes,
    ) -> Result<Bytes> {
        let mut url = self.session.spclient().base_url().await?;
        url.push_str(endpoint);
        self.send(true, &method, &url, content_type, body).await
    }

    pub async fn pathfinder<V, T>(&self, query: &Query, variables: &V) -> Result<T>
    where
        V: Serialize,
        T: DeserializeOwned,
    {
        let body = serde_json::to_vec(&QueryBody {
            name: query.name,
            variables,
            extensions: Extensions {
                persisted: Persisted {
                    version: 1,
                    hash: query.hash,
                },
            },
        })
        .map_err(decode)?;

        let response = self
            .send(true, &Method::POST, PATHFINDER, Some(JSON), body.into())
            .await
            .map_err(|error| match error {
                // 412 is an unknown hash, 400 a changed variable set.
                Error::Status(412 | 400) => Error::QueryChanged,
                other => other,
            })?;
        match serde_json::from_slice::<Envelope<T>>(&response) {
            Ok(envelope) => Ok(envelope.data),
            Err(error) => {
                log::debug!("{} answered {}", query.name, shown(&response));
                Err(decode(error))
            }
        }
    }

    /// Plain GET without credentials, for the image CDN.
    pub async fn get(&self, url: &str) -> Result<Bytes> {
        self.send(false, &Method::GET, url, None, Bytes::new())
            .await
    }

    async fn send(
        &self,
        authorized: bool,
        method: &Method,
        url: &str,
        content_type: Option<&'static str>,
        body: Bytes,
    ) -> Result<Bytes> {
        let _permit = self.permits.acquire().await.map_err(network)?;

        let mut attempt = 1;
        loop {
            let mut request = Request::builder()
                .method(method)
                .uri(url)
                .body(body.clone())
                .map_err(network)?;
            let headers = request.headers_mut();
            if let Some(content_type) = content_type {
                headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
                headers.insert(ACCEPT, HeaderValue::from_static(content_type));
            }
            // Over HTTP/1.1, which the Windows build speaks, an empty body goes
            // out without a length, and Spotify refuses that with 411.
            if body.is_empty() && *method != Method::GET {
                headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
            }
            if authorized {
                // Both tokens are cached by the session and refreshed on expiry.
                let token = self.session.login5().auth_token().await?;
                let client_token = self.session.spclient().client_token().await?;
                headers.insert(
                    AUTHORIZATION,
                    header(&format!("Bearer {}", token.access_token))?,
                );
                headers.insert("client-token", header(&client_token)?);
            }

            let response = match self.session.http_client().request_fut(request) {
                Ok(response) => response.await,
                // The session's own limiter refused the request.
                Err(error) if error.kind == ErrorKind::ResourceExhausted => {
                    return Err(Error::RateLimited { retry_after: None });
                }
                Err(error) => return Err(error.into()),
            };

            match response {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        let body = response.into_body().collect().await.map_err(network)?;
                        return Ok(body.to_bytes());
                    }
                    let error = match status {
                        StatusCode::TOO_MANY_REQUESTS => Error::RateLimited {
                            retry_after: HttpClient::get_retry_after(response.headers()),
                        },
                        StatusCode::NOT_FOUND => Error::NotFound,
                        status => Error::Status(status.as_u16()),
                    };
                    let body = response.into_body().collect().await;
                    let body = body.map(|body| body.to_bytes()).unwrap_or_default();
                    log_failure(method, url, status, &body);
                    return Err(error);
                }
                // Connection-level failures are usually a stale pooled socket.
                Err(_) if attempt < ATTEMPTS => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error) => return Err(network(error)),
            }
        }
    }
}

/// Logs what Spotify answered to a request that failed.
fn log_failure(method: &Method, url: &str, status: StatusCode, body: &[u8]) {
    // Missing lyrics, covers and the like answer 404 all the time.
    let level = if status == StatusCode::NOT_FOUND {
        log::Level::Debug
    } else {
        log::Level::Warn
    };
    let path = url.split('?').next().unwrap_or(url);
    log::log!(level, "{method} {path} answered {status}: {}", shown(body));
}

/// The start of a response, for the log.
fn shown(body: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(&body[..body.len().min(RESPONSE_LOG_BYTES)])
}

fn header(value: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(value).map_err(network)
}

fn network(error: impl std::fmt::Display) -> Error {
    Error::Network(error.to_string())
}

pub(crate) fn decode(error: impl std::fmt::Display) -> Error {
    Error::Decode(error.to_string())
}
