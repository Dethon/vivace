//! A desktop notification naming each new song, as Spotify's Linux app
//! shows. Windows already names it in its media overlay, which the media
//! keys feed, so there is nothing to add there.

/// Whether this system gets song notifications.
pub const SUPPORTED: bool = cfg!(target_os = "linux");

#[cfg(target_os = "linux")]
pub use linux::announce;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    use zbus::zvariant::Value;

    const SERVICE: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    // How long the notification stays, in milliseconds.
    const SHOWN_FOR: i32 = 4_000;

    /// The notification shown last, which the next one replaces instead of
    /// stacking up.
    static SHOWN: AtomicU32 = AtomicU32::new(0);

    /// Shows `title` and `subtitle` in a notification, off the calling
    /// thread: the session bus answers in its own time.
    pub fn announce(title: String, subtitle: String) {
        let spawned = std::thread::Builder::new()
            .name("notify".into())
            .spawn(move || {
                if let Err(error) = notify(&title, &subtitle) {
                    log::debug!("couldn't show a notification: {error}");
                }
            });
        if let Err(error) = spawned {
            log::warn!("couldn't start the notification thread: {error}");
        }
    }

    fn notify(title: &str, subtitle: &str) -> zbus::Result<()> {
        let connection = zbus::blocking::Connection::session()?;
        let hints: HashMap<&str, Value<'_>> =
            HashMap::from([("desktop-entry", Value::from("vivace"))]);
        let reply = connection.call_method(
            Some(SERVICE),
            PATH,
            Some(SERVICE),
            "Notify",
            &(
                "Vivace",
                SHOWN.load(Ordering::Relaxed),
                "",
                title,
                subtitle,
                Vec::<&str>::new(),
                hints,
                SHOWN_FOR,
            ),
        )?;
        let id: u32 = reply.body().deserialize()?;
        SHOWN.store(id, Ordering::Relaxed);
        Ok(())
    }
}

/// Stands in where there are no song notifications.
#[cfg(not(target_os = "linux"))]
pub fn announce(_: String, _: String) {}
