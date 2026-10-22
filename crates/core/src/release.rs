//! New versions of Vivace, as published on its GitHub releases page.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use bytes::Bytes;
use http::{
    Request, StatusCode, Uri,
    header::{ACCEPT, LOCATION},
};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use librespot_core::http_client::HttpClient;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const LATEST: &str = "https://api.github.com/repos/Dethon/vivace/releases/latest";
/// Every release with its notes, newest first: Vivace's changelog.
pub const CHANGELOG: &str = "https://github.com/Dethon/vivace/releases";
const GITHUB_JSON: &str = "application/vnd.github+json";
const CHECKSUMS: &str = "SHA256SUMS";
// Asset links redirect to a download host; a few hops are plenty.
const MAX_REDIRECTS: usize = 5;
// What is kept of an unfinished download's name.
const PARTIAL: &str = "part";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(String),
    #[error("GitHub answered with HTTP {0}")]
    Status(u16),
    #[error("unexpected answer from GitHub: {0}")]
    Decode(String),
    #[error("the release has no {0}")]
    MissingAsset(String),
    #[error("the download doesn't match its checksum")]
    Checksum,
    #[error("couldn't save the download: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A release number such as 0.4.1, compared part by part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// The version of this build.
    pub fn current() -> Self {
        env!("CARGO_PKG_VERSION")
            .parse()
            .expect("the package version is a plain release number")
    }
}

impl FromStr for Version {
    type Err = ();

    /// Reads `1.2.3` or `v1.2.3`; anything else, such as a pre-release, is refused.
    fn from_str(text: &str) -> std::result::Result<Self, ()> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text
            .split('.')
            .map(|part| part.parse::<u32>().map_err(drop));
        let version = Self {
            major: parts.next().ok_or(())??,
            minor: parts.next().ok_or(())??,
            patch: parts.next().ok_or(())??,
        };
        match parts.next() {
            Some(_) => Err(()),
            None => Ok(version),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The newest published release.
#[derive(Debug, Clone)]
pub struct Release {
    pub version: Version,
    /// The release's web page, with its notes and downloads.
    pub page: String,
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct Published {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

impl Release {
    fn from_json(json: &[u8]) -> Result<Self> {
        let published: Published = serde_json::from_slice(json).map_err(decode)?;
        let version = published
            .tag_name
            .parse()
            .map_err(|()| Error::Decode(format!("tag {}", published.tag_name)))?;
        Ok(Self {
            version,
            page: published.html_url,
            assets: published.assets,
        })
    }

    /// The name of this release's Windows installer.
    pub fn installer_name(&self) -> String {
        installer_name(self.version)
    }

    fn asset(&self, name: &str) -> Result<&Asset> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| Error::MissingAsset(name.to_owned()))
    }

    /// Downloads the Windows installer into `directory` and checks it
    /// against the release's checksums. The file only takes its final name
    /// once it has passed, so one already there needs no second download.
    pub async fn download_installer(&self, directory: &Path) -> Result<PathBuf> {
        let name = self.installer_name();
        let path = directory.join(&name);
        if path.is_file() {
            return Ok(path);
        }
        let installer = self.asset(&name)?;
        let client = HttpClient::new(None);
        let sums = get(&client, &self.asset(CHECKSUMS)?.browser_download_url, None).await?;
        let sums = std::str::from_utf8(&sums).map_err(decode)?;
        let expected = checksum_of(sums, &name).ok_or_else(|| Error::MissingAsset(name.clone()))?;

        fs::create_dir_all(directory)?;
        let partial = path.with_extension(PARTIAL);
        let result = save(&client, &installer.browser_download_url, &partial).await;
        let verified = match result {
            Ok(digest) if digest.eq_ignore_ascii_case(expected) => Ok(()),
            Ok(_) => Err(Error::Checksum),
            Err(error) => Err(error),
        };
        if let Err(error) = verified {
            _ = fs::remove_file(&partial);
            return Err(error);
        }
        fs::rename(&partial, &path)?;
        Ok(path)
    }
}

/// The newest release on GitHub, drafts and pre-releases aside.
pub async fn latest() -> Result<Release> {
    let client = HttpClient::new(None);
    Release::from_json(&get(&client, LATEST, Some(GITHUB_JSON)).await?)
}

/// What the Windows installer of `version` is called.
pub fn installer_name(version: Version) -> String {
    format!("Vivace-{version}-setup.exe")
}

/// The version of an installer this module saved, from its file name.
pub fn installer_version(file_name: &str) -> Option<Version> {
    file_name
        .strip_prefix("Vivace-")?
        .strip_suffix("-setup.exe")?
        .parse()
        .ok()
}

/// The checksum `sha256sum` listed for `name`.
fn checksum_of<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (digest, file) = line.split_once(char::is_whitespace)?;
        // Binary mode marks the name with an asterisk.
        let file = file.trim_start();
        let file = file.strip_prefix('*').unwrap_or(file);
        (file.trim_end() == name).then_some(digest)
    })
}

/// Answers a GET, following redirects.
async fn respond(client: &HttpClient, url: &str, accept: Option<&str>) -> Result<Incoming> {
    let mut uri: Uri = url.parse().map_err(network)?;
    for _ in 0..=MAX_REDIRECTS {
        let mut request = Request::get(uri.clone());
        if let Some(accept) = accept {
            request = request.header(ACCEPT, accept);
        }
        let request = request.body(Bytes::new()).map_err(network)?;
        let response = client
            .request_fut(request)
            .map_err(network)?
            .await
            .map_err(network)?;
        let status = response.status();
        if status.is_success() {
            return Ok(response.into_body());
        }
        if !status.is_redirection() || status == StatusCode::NOT_MODIFIED {
            return Err(Error::Status(status.as_u16()));
        }
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|location| location.to_str().ok())
            .ok_or(Error::Status(status.as_u16()))?;
        uri = location.parse().map_err(network)?;
    }
    Err(Error::Network("too many redirects".into()))
}

async fn get(client: &HttpClient, url: &str, accept: Option<&str>) -> Result<Bytes> {
    let body = respond(client, url, accept).await?;
    Ok(body.collect().await.map_err(network)?.to_bytes())
}

/// Writes a download to `path` as it arrives and returns its SHA-256 in hex.
async fn save(client: &HttpClient, url: &str, path: &Path) -> Result<String> {
    let mut body = respond(client, url, None).await?;
    let mut file = fs::File::create(path)?;
    let mut hasher = Sha256::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.map_err(network)?.into_data() {
            hasher.update(&data);
            file.write_all(&data)?;
        }
    }
    file.sync_all()?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn network(error: impl fmt::Display) -> Error {
    Error::Network(error.to_string())
}

fn decode(error: impl fmt::Display) -> Error {
    Error::Decode(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        text.parse().unwrap()
    }

    #[test]
    fn versions_compare_part_by_part() {
        assert!(version("0.10.0") > version("0.9.9"));
        assert!(version("v0.4.2") > version("0.4.1"));
        assert!(version("1.0.0") > version("0.99.99"));
        assert_eq!(version("v0.4.1"), version("0.4.1"));
        assert_eq!(version("0.4.1").to_string(), "0.4.1");
    }

    #[test]
    fn only_plain_release_numbers_are_versions() {
        for text in ["", "0.4", "0.4.1.2", "0.5.0-beta", "x.4.1", "v"] {
            assert!(text.parse::<Version>().is_err(), "{text}");
        }
        assert!(Version::current() >= version("0.4.1"));
    }

    #[test]
    fn installers_are_named_by_version() {
        assert_eq!(installer_name(version("0.5.0")), "Vivace-0.5.0-setup.exe");
        assert_eq!(
            installer_version("Vivace-0.5.0-setup.exe"),
            Some(version("0.5.0"))
        );
        assert_eq!(installer_version("Vivace-0.5.0-setup.part"), None);
        assert_eq!(installer_version("vivace-0.5.0-windows-x64.zip"), None);
    }

    #[test]
    fn checksums_are_found_by_file_name() {
        let sums = "aa11  Vivace-0.4.1-setup.exe\n\
                    bb22 *vivace-0.4.1-windows-x64.zip\r\n\
                    cc33  vivace_0.4.1_amd64.deb\n";
        assert_eq!(checksum_of(sums, "Vivace-0.4.1-setup.exe"), Some("aa11"));
        assert_eq!(
            checksum_of(sums, "vivace-0.4.1-windows-x64.zip"),
            Some("bb22")
        );
        assert_eq!(checksum_of(sums, "vivace_0.4.1_amd64.deb"), Some("cc33"));
        assert_eq!(checksum_of(sums, "Vivace-0.4.0-setup.exe"), None);
    }

    #[test]
    fn a_release_is_read_from_github_json() {
        let json = br#"{
            "tag_name": "v0.4.1",
            "html_url": "https://github.com/Dethon/vivace/releases/tag/v0.4.1",
            "draft": false,
            "assets": [
                {"name": "SHA256SUMS", "browser_download_url": "https://example.com/sums", "size": 1},
                {"name": "Vivace-0.4.1-setup.exe", "browser_download_url": "https://example.com/setup", "size": 2}
            ]
        }"#;
        let release = Release::from_json(json).unwrap();
        assert_eq!(release.version, version("0.4.1"));
        assert_eq!(
            release.page,
            "https://github.com/Dethon/vivace/releases/tag/v0.4.1"
        );
        assert_eq!(
            release
                .asset("Vivace-0.4.1-setup.exe")
                .unwrap()
                .browser_download_url,
            "https://example.com/setup"
        );
        assert!(release.asset("Vivace-0.4.0-setup.exe").is_err());
        assert!(Release::from_json(br#"{"tag_name": "nightly", "html_url": ""}"#).is_err());
    }
}
