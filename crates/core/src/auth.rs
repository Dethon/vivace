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

/// The directory holding credentials, settings and the log.
pub fn data_dir() -> crate::Result<PathBuf> {
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

const KEYRING_SERVICE: &str = "vivace";
const KEYRING_ACCOUNT: &str = "spotify";
const CREDENTIALS_FILE: &str = "credentials.json";

/// The credentials saved by an earlier run: from the system keyring, or
/// from librespot's file where there is no keyring.
pub(crate) fn stored_credentials(cache: &Cache) -> Option<Credentials> {
    keyring_entry()
        .and_then(|entry| entry.get_password().ok())
        .and_then(|json| serde_json::from_str(&json).ok())
        .or_else(|| cache.credentials())
}

/// Moves the credentials librespot just wrote to its file into the system
/// keyring. Without a keyring the file stays, so signing in still sticks.
pub(crate) fn protect_credentials() {
    let Ok(path) = data_dir().map(|directory| directory.join(CREDENTIALS_FILE)) else {
        return;
    };
    let Ok(json) = std::fs::read_to_string(&path) else {
        return;
    };
    let stored = keyring_entry()
        .ok_or_else(|| "no keyring".to_owned())
        .and_then(|entry| entry.set_password(&json).map_err(|error| error.to_string()));
    match stored {
        Ok(()) => {
            if let Err(error) = std::fs::remove_file(&path) {
                log::warn!("couldn't remove {}: {error}", path.display());
            }
        }
        Err(error) => log::info!("keeping credentials in a file: {error}"),
    }
}

/// Forgets the saved login, so the next connection asks for it in the browser.
pub fn sign_out() {
    if let Some(entry) = keyring_entry() {
        // There is nothing to delete when the login was kept in the file.
        _ = entry.delete_credential();
    }
    if let Ok(path) = data_dir().map(|directory| directory.join(CREDENTIALS_FILE))
        && let Err(error) = std::fs::remove_file(&path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("couldn't remove {}: {error}", path.display());
    }
}

fn keyring_entry() -> Option<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).ok()
}

const DEVICE_ID_FILE: &str = "device-id";

/// The id this computer has in Spotify Connect, kept across runs so it
/// stays one device in Spotify's lists. The first run stores `fresh`.
/// Returns `None` when the data directory is unusable, in which case the
/// session keeps its fresh id.
pub(crate) fn device_id(fresh: &str) -> Option<String> {
    let path = data_dir().ok()?.join(DEVICE_ID_FILE);
    if let Ok(stored) = std::fs::read_to_string(&path) {
        let stored = stored.trim();
        if !stored.is_empty() {
            return Some(stored.to_owned());
        }
    }
    match std::fs::write(&path, fresh) {
        Ok(()) => Some(fresh.to_owned()),
        Err(error) => {
            log::warn!("couldn't keep the device id in {}: {error}", path.display());
            None
        }
    }
}
