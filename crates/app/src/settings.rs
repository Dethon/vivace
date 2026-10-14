//! Small pieces of state kept between runs.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vivace_core::{Config, Quality};

const FILE_NAME: &str = "settings.json";
// Earlier versions stored the default name; every computer shared it.
const OLD_DEFAULT_NAME: &str = "Vivace";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub window: Option<WindowState>,
    pub quality: Quality,
    pub normalise: bool,
    /// Name this computer has in Spotify's device list, when the user chose
    /// one; see [`Settings::device_name`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

/// The window's size in logical pixels. Its position is not kept: a saved
/// one can point at a monitor that is no longer there.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
}

impl Settings {
    /// Reads the settings; a missing or unreadable file yields the defaults.
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        let mut settings: Self = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if settings.device_name.as_deref() == Some(OLD_DEFAULT_NAME) {
            settings.device_name = None;
        }
        settings
    }

    /// The name this computer has in Spotify's device list.
    pub fn device_name(&self) -> String {
        self.device_name
            .clone()
            .unwrap_or_else(Config::default_device_name)
    }

    /// Names the device, or goes back to the default name for an empty one.
    pub fn set_device_name(&mut self, name: &str) {
        let name = name.trim();
        self.device_name =
            (!name.is_empty() && name != Config::default_device_name()).then(|| name.to_owned());
    }

    /// Changes the stored settings. They are re-read first because the
    /// window and the settings page each write their own part.
    pub fn update(change: impl FnOnce(&mut Self)) -> Self {
        let mut settings = Self::load();
        change(&mut settings);
        settings.save();
        settings
    }

    /// Whether the two differ in something the engine has to restart for.
    pub fn config_differs(&self, other: &Self) -> bool {
        (self.quality, self.normalise, self.device_name())
            != (other.quality, other.normalise, other.device_name())
    }

    /// The engine configuration these settings ask for.
    pub fn config(&self) -> Config {
        Config {
            device_name: self.device_name(),
            quality: self.quality,
            normalise: self.normalise,
            persistent_device: true,
            sink: None,
        }
    }

    fn save(&self) {
        let Some(path) = path() else { return };
        let result = serde_json::to_vec_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|bytes| fs::write(path, bytes));
        if let Err(error) = result {
            log::warn!("couldn't save settings: {error}");
        }
    }
}

fn path() -> Option<PathBuf> {
    let directory = vivace_core::data_dir().ok()?;
    fs::create_dir_all(&directory).ok()?;
    Some(directory.join(FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_round_trip() {
        let settings = Settings {
            window: Some(WindowState {
                width: 1280.0,
                height: 800.0,
                maximized: true,
            }),
            quality: Quality::Normal,
            normalise: true,
            device_name: Some("Study".into()),
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), settings);
    }

    #[test]
    fn an_empty_or_default_name_means_the_default() {
        let mut settings = Settings::default();
        settings.set_device_name("  Study  ");
        assert_eq!(settings.device_name.as_deref(), Some("Study"));
        settings.set_device_name("");
        assert_eq!(settings.device_name, None);
        settings.set_device_name(&Config::default_device_name());
        assert_eq!(settings.device_name, None);
        assert_eq!(settings.device_name(), Config::default_device_name());
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let settings: Settings =
            serde_json::from_str(r#"{"window": {"width": 900, "height": 600}, "future": 1}"#)
                .unwrap();
        assert_eq!(
            settings.window,
            Some(WindowState {
                width: 900.0,
                height: 600.0,
                maximized: false,
            })
        );
        assert_eq!(settings.quality, Quality::VeryHigh);
        assert_eq!(settings.device_name, None);
        assert_eq!(
            serde_json::from_str::<Settings>("{}").unwrap(),
            Settings::default()
        );
    }
}
