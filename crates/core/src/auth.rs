use std::path::PathBuf;

use librespot_core::{authentication::Credentials, cache::Cache};
use librespot_oauth::OAuthClientBuilder;

// Spotify's own desktop client id; third-party ids are not accepted for streaming sessions.
const CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
// The scope set librespot requests; Spotify rejects the whole login if any name is unknown.
const SCOPES: &[&str] = &[
    "app-remote-control",
    "playlist-modify",
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read",
    "playlist-read-collaborative",
    "playlist-read-private",
    "streaming",
    "ugc-image-upload",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify",
    "user-modify-playback-state",
    "user-modify-private",
    "user-personalized",
    "user-read-birthdate",
    "user-read-currently-playing",
    "user-read-email",
    "user-read-play-history",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];

const LOGIN_DONE_PAGE: &str =
    "<!doctype html><html><body><h1>Signed in. You can close this tab.</h1></body></html>";

fn data_dir() -> crate::Result<PathBuf> {
    Ok(dirs::config_dir()
        .ok_or(crate::Error::NoConfigDir)?
        .join("vivace"))
}

/// Opens the on-disk store for credentials and the last volume.
pub(crate) fn cache() -> crate::Result<Cache> {
    let dir = data_dir()?;
    Ok(Cache::new(Some(dir.clone()), Some(dir), None, None)?)
}

/// Runs the OAuth flow in the user's browser.
pub(crate) async fn browser_login() -> crate::Result<Credentials> {
    let client = OAuthClientBuilder::new(CLIENT_ID, REDIRECT_URI, SCOPES.to_vec())
        .open_in_browser()
        .with_custom_message(LOGIN_DONE_PAGE)
        .build()?;
    let token = client.get_access_token_async().await?;
    Ok(Credentials::with_access_token(token.access_token))
}
