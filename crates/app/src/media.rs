//! Media keys and the operating system's "now playing" display.

use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use tokio::sync::mpsc::UnboundedSender;
use vivace_core::model::Track;

use crate::backend::Update;

// Jump made by a media key that seeks without saying how far.
const SEEK_STEP: Duration = Duration::from_secs(10);

/// What the system asked the player to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MediaCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    /// Seek to an absolute position.
    SeekTo(Duration),
    /// Seek relative to the current position, in milliseconds.
    SeekBy(i64),
}

pub struct MediaKeys(MediaControls);

/// Outcome of trying to register with the system.
pub enum Attach {
    Ready(MediaKeys),
    /// The native window does not exist yet; try again shortly.
    WindowNotReady,
    /// The platform offers no such service, which only costs the media keys.
    Unavailable,
}

impl MediaKeys {
    /// Registers with the system; commands arrive as [`Update::Media`].
    pub fn attach(window: &slint::Window, updates: UnboundedSender<Update>) -> Attach {
        let handle = window.window_handle();
        let hwnd = match handle.window_handle().map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => Some(handle.hwnd.get() as *mut std::ffi::c_void),
            _ => None,
        };
        // Windows ties media controls to a window and cannot start without one.
        if cfg!(windows) && hwnd.is_none() {
            return Attach::WindowNotReady;
        }
        let config = PlatformConfig {
            display_name: "Vivace",
            dbus_name: "vivace",
            hwnd,
        };

        let attach = || -> Result<MediaControls, souvlaki::Error> {
            let mut controls = MediaControls::new(config)?;
            controls.attach(move |event| {
                if let Some(command) = command(event) {
                    // The receiver is gone only while the app shuts down.
                    _ = updates.send(Update::Media(command));
                }
            })?;
            Ok(controls)
        };
        match attach() {
            Ok(controls) => Attach::Ready(Self(controls)),
            Err(error) => {
                log::warn!("media keys are unavailable: {error:?}");
                Attach::Unavailable
            }
        }
    }

    pub fn set_track(&mut self, track: &Track, artists: &str) {
        let metadata = MediaMetadata {
            title: Some(&track.name),
            artist: Some(artists),
            album: Some(&track.album.name),
            cover_url: track.cover.as_ref().map(|cover| cover.url()),
            duration: Some(Duration::from_millis(track.duration_ms.into())),
        };
        if let Err(error) = self.0.set_metadata(metadata) {
            log::warn!("couldn't publish the playing track: {error:?}");
        }
    }

    pub fn set_playback(&mut self, playing: bool, position_ms: u32) {
        let progress = Some(MediaPosition(Duration::from_millis(position_ms.into())));
        let playback = if playing {
            MediaPlayback::Playing { progress }
        } else {
            MediaPlayback::Paused { progress }
        };
        if let Err(error) = self.0.set_playback(playback) {
            log::warn!("couldn't publish the playback state: {error:?}");
        }
    }
}

fn command(event: MediaControlEvent) -> Option<MediaCommand> {
    let signed = |direction, amount: Duration| {
        let amount = i64::try_from(amount.as_millis()).unwrap_or(i64::MAX);
        match direction {
            SeekDirection::Forward => amount,
            SeekDirection::Backward => -amount,
        }
    };
    Some(match event {
        MediaControlEvent::Play => MediaCommand::Play,
        MediaControlEvent::Pause | MediaControlEvent::Stop => MediaCommand::Pause,
        MediaControlEvent::Toggle => MediaCommand::Toggle,
        MediaControlEvent::Next => MediaCommand::Next,
        MediaControlEvent::Previous => MediaCommand::Previous,
        MediaControlEvent::SetPosition(MediaPosition(position)) => MediaCommand::SeekTo(position),
        MediaControlEvent::Seek(direction) => MediaCommand::SeekBy(signed(direction, SEEK_STEP)),
        MediaControlEvent::SeekBy(direction, amount) => {
            MediaCommand::SeekBy(signed(direction, amount))
        }
        MediaControlEvent::SetVolume(_)
        | MediaControlEvent::OpenUri(_)
        | MediaControlEvent::Raise
        | MediaControlEvent::Quit => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_transport_and_seek_events() {
        assert_eq!(
            command(MediaControlEvent::Toggle),
            Some(MediaCommand::Toggle)
        );
        assert_eq!(command(MediaControlEvent::Stop), Some(MediaCommand::Pause));
        assert_eq!(
            command(MediaControlEvent::Seek(SeekDirection::Backward)),
            Some(MediaCommand::SeekBy(-10_000))
        );
        assert_eq!(
            command(MediaControlEvent::SeekBy(
                SeekDirection::Forward,
                Duration::from_secs(3)
            )),
            Some(MediaCommand::SeekBy(3000))
        );
        assert_eq!(command(MediaControlEvent::Raise), None);
    }
}
