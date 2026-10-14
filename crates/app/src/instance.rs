//! Keeps Vivace to one window per user: opening it again brings the running
//! one forward instead of starting a second player with the same device.
//!
//! The marker is an object the system drops with the process, so a crash
//! never leaves a stale lock behind.

use slint::ComponentHandle;

use crate::MainWindow;

// What the marker is called, per user.
const NAME: &str = "Vivace.Running";

/// This process's hold on being the running instance.
pub struct Instance(platform::Marker);

/// Claims the running instance for this process. When another one already
/// holds it, that one is asked to show itself unless `quietly`, and `None`
/// comes back.
pub fn claim(quietly: bool) -> Option<Instance> {
    match platform::Marker::create(NAME) {
        Ok(Some(marker)) => Some(Instance(marker)),
        Ok(None) => {
            if !quietly && let Err(error) = platform::signal(NAME) {
                log::warn!("couldn't reach the running Vivace: {error}");
            }
            None
        }
        // Better two windows than none.
        Err(error) => {
            log::warn!("couldn't check for a running Vivace: {error}");
            Some(Instance(platform::Marker::none()))
        }
    }
}

impl Instance {
    /// Brings the window forward whenever Vivace is opened again.
    pub fn show_when_reopened(self, ui: &MainWindow) {
        let ui = ui.as_weak();
        let started = std::thread::Builder::new()
            .name("reopen".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                self.0.wait(|| {
                    _ = ui.upgrade_in_event_loop(|ui| {
                        crate::tray::show_window(&ui);
                        platform::bring_forward(&ui);
                    });
                });
            });
        if let Err(error) = started {
            log::warn!("couldn't watch for Vivace being reopened: {error}");
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0},
        System::Threading::{
            CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
        },
        UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow, SetForegroundWindow},
    };

    use crate::MainWindow;

    // `Local\` scopes it to the user's session.
    fn wide(name: &str) -> Vec<u16> {
        format!("Local\\{name}").encode_utf16().chain([0]).collect()
    }

    /// A named event: whoever creates it is running, and later instances
    /// set it to ask for the window.
    pub struct Marker(HANDLE);

    // SAFETY: an event handle may be used from any thread.
    unsafe impl Send for Marker {}

    impl Marker {
        pub fn create(name: &str) -> io::Result<Option<Self>> {
            // SAFETY: the name is NUL-terminated; an auto-reset event, unset.
            let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, wide(name).as_ptr()) };
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: reads the calling thread's last error.
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                // SAFETY: the handle was just opened and is not used again.
                unsafe { CloseHandle(event) };
                return Ok(None);
            }
            Ok(Some(Self(event)))
        }

        pub fn none() -> Self {
            Self(std::ptr::null_mut())
        }

        pub fn wait(self, reopened: impl Fn()) {
            if self.0.is_null() {
                return;
            }
            // SAFETY: the handle stays open for the whole process.
            while unsafe { WaitForSingleObject(self.0, INFINITE) } == WAIT_OBJECT_0 {
                reopened();
            }
        }
    }

    pub fn signal(name: &str) -> io::Result<()> {
        // SAFETY: the name is NUL-terminated.
        let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, wide(name).as_ptr()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // Windows only lets a process take the foreground when the one that
        // has it allows so: that is this one, as the user just opened it.
        // SAFETY: plain calls on a handle opened above.
        let set = unsafe {
            AllowSetForegroundWindow(ASFW_ANY);
            let set = SetEvent(event);
            CloseHandle(event);
            set
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn bring_forward(ui: &MainWindow) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use slint::ComponentHandle;

        let window = ui.window().window_handle();
        if let Ok(handle) = window.window_handle()
            && let RawWindowHandle::Win32(handle) = handle.as_raw()
        {
            // SAFETY: the handle belongs to the window shown on this thread.
            unsafe { SetForegroundWindow(handle.hwnd.get() as _) };
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::{
        io::{self, ErrorKind},
        os::unix::net::{SocketAddr, UnixListener, UnixStream},
    };

    use crate::MainWindow;

    /// A listening socket: whoever binds it is running, and later instances
    /// connect to it to ask for the window.
    pub struct Marker(Option<UnixListener>);

    // An abstract socket vanishes with its process, unlike a file. The user
    // id keeps other people's sessions on the same machine apart.
    fn address(name: &str) -> io::Result<SocketAddr> {
        #[cfg(target_os = "linux")]
        {
            use std::os::{linux::net::SocketAddrExt, unix::fs::MetadataExt};

            let user = std::fs::metadata("/proc/self")?.uid();
            SocketAddr::from_abstract_name(format!("{name}.{user}"))
        }
        #[cfg(not(target_os = "linux"))]
        {
            _ = name;
            Err(io::Error::from(ErrorKind::Unsupported))
        }
    }

    impl Marker {
        pub fn create(name: &str) -> io::Result<Option<Self>> {
            match UnixListener::bind_addr(&address(name)?) {
                Ok(listener) => Ok(Some(Self(Some(listener)))),
                Err(error) if error.kind() == ErrorKind::AddrInUse => Ok(None),
                Err(error) => Err(error),
            }
        }

        pub fn none() -> Self {
            Self(None)
        }

        pub fn wait(self, reopened: impl Fn()) {
            let Some(listener) = self.0 else { return };
            for connection in listener.incoming() {
                match connection {
                    // Connecting is the whole message.
                    Ok(_) => reopened(),
                    Err(error) => log::warn!("couldn't take a reopen request: {error}"),
                }
            }
        }
    }

    pub fn signal(name: &str) -> io::Result<()> {
        UnixStream::connect_addr(&address(name)?).map(drop)
    }

    // The window manager decides about focus; showing the window is the ask.
    pub fn bring_forward(_: &MainWindow) {}
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::platform::{Marker, signal};

    #[test]
    fn a_second_claim_signals_the_first() {
        // Apart from a real Vivace, and from other test runs.
        let name = format!("Vivace.Test.{}", std::process::id());
        let first = Marker::create(&name).unwrap().expect("first claim");
        assert!(Marker::create(&name).unwrap().is_none());

        let (reopened, received) = mpsc::channel();
        std::thread::spawn(move || first.wait(move || _ = reopened.send(())));
        signal(&name).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        signal(&name).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}
