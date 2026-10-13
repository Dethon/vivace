use bytes::Bytes;
use http::Method;
use serde::Deserialize;

use super::{Api, transport::decode};
use crate::{
    Error, Result,
    model::{Id, LyricLine, Lyrics},
};

const JSON: &str = "application/json";

impl Api {
    /// A track's lyrics, or `None` when Spotify has none for it.
    pub async fn lyrics(&self, track: Id) -> Result<Option<Lyrics>> {
        let endpoint = format!("/color-lyrics/v2/track/{track}?format=json&market=from_token");
        match self
            .0
            .spclient(Method::GET, &endpoint, Some(JSON), Bytes::new())
            .await
        {
            Ok(response) => {
                let response: Response = serde_json::from_slice(&response).map_err(decode)?;
                Ok(Some(response.lyrics.into()))
            }
            Err(Error::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[derive(Deserialize)]
struct Response {
    lyrics: Body,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Body {
    sync_type: String,
    lines: Vec<Line>,
    #[serde(default)]
    provider_display_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    // Milliseconds, sent as a string.
    start_time_ms: String,
    words: String,
}

impl From<Body> for Lyrics {
    fn from(body: Body) -> Self {
        Self {
            synced: body.sync_type == "LINE_SYNCED",
            provider: body.provider_display_name.into(),
            lines: body
                .lines
                .into_iter()
                .map(|line| LyricLine {
                    start_ms: line.start_time_ms.parse().unwrap_or_default(),
                    text: line.words.into(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synced_lines() {
        let response: Response = serde_json::from_str(
            r#"{"lyrics": {"syncType": "LINE_SYNCED", "providerDisplayName": "Musixmatch",
                "lines": [
                    {"startTimeMs": "960", "words": "We're no strangers to love", "syllables": [], "endTimeMs": "0"},
                    {"startTimeMs": "5120", "words": "♪", "syllables": [], "endTimeMs": "0"}
                ], "language": "en"},
                "colors": {"background": -1, "text": -1, "highlightText": -1}, "hasVocalRemoval": false}"#,
        )
        .unwrap();
        let lyrics = Lyrics::from(response.lyrics);

        assert!(lyrics.synced);
        assert_eq!(&*lyrics.provider, "Musixmatch");
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].start_ms, 960);
        assert_eq!(&*lyrics.lines[0].text, "We're no strangers to love");
    }

    #[test]
    fn unsynced_lyrics_are_flagged() {
        let response: Response =
            serde_json::from_str(r#"{"lyrics": {"syncType": "UNSYNCED", "lines": []}}"#).unwrap();
        assert!(!Lyrics::from(response.lyrics).synced);
    }
}
