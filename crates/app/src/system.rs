//! Starting with the computer: an entry that opens Vivace when the user
//! signs in, the registry's Run key on Windows and an XDG autostart file
//! elsewhere. The entry itself is the setting, so it is never out of step
//! with what the system will do.

use std::io;

/// Passed by the login entry, so the app starts out of the way.
pub const BACKGROUND_FLAG: &str = "--background";

/// Whether Vivace opens when the user signs in.
pub fn starts_at_login() -> bool {
    platform::entry_exists()
}

/// Adds or removes the login entry.
pub fn set_start_at_login(on: bool) -> io::Result<()> {
    if on {
        let program = std::env::current_exe()?;
        platform::write_entry(&program.display().to_string())
    } else {
        platform::remove_entry()
    }
}

/// Points the login entry at this copy of the app when the program it
/// names is gone, e.g. after the app was moved. An entry naming another
/// copy that still exists is left alone, so running a second copy never
/// changes which one starts at login.
pub fn refresh_login_entry() {
    let Some(command) = platform::entry_command() else {
        return;
    };
    if program_of(&command).is_some_and(|program| std::path::Path::new(program).exists()) {
        return;
    }
    if let Err(error) = set_start_at_login(true) {
        log::warn!("couldn't update the login entry: {error}");
    }
}

/// The program a login command runs: the quoted path at its start.
fn program_of(command: &str) -> Option<&str> {
    let quoted = command.trim_start().strip_prefix('"')?;
    quoted.split_once('"').map(|(program, _)| program)
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    use super::BACKGROUND_FLAG;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Vivace";

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The command the entry runs, if there is one.
    pub fn entry_command() -> Option<String> {
        let (key, value) = (wide(RUN_KEY), wide(VALUE));
        let mut buffer = vec![0u16; 1024];
        let mut size = (buffer.len() * 2) as u32;
        // SAFETY: the strings are NUL-terminated and `size` is the
        // buffer's length in bytes.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let length = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..length]))
    }

    pub fn entry_exists() -> bool {
        let (key, value) = (wide(RUN_KEY), wide(VALUE));
        let mut size = 0;
        // SAFETY: the strings are NUL-terminated and outlive the call; with
        // no buffer the call only reports the value's size.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                null_mut(),
                &mut size,
            )
        };
        status == ERROR_SUCCESS
    }

    pub fn write_entry(program: &str) -> io::Result<()> {
        let (key, value) = (wide(RUN_KEY), wide(VALUE));
        let command = wide(&format!("\"{program}\" {BACKGROUND_FLAG}"));
        let bytes = u32::try_from(command.len() * 2).map_err(io::Error::other)?;
        // SAFETY: the strings are NUL-terminated and `bytes` is the size of
        // `command`, NUL included.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                command.as_ptr().cast(),
                bytes,
            )
        };
        check(status)
    }

    pub fn remove_entry() -> io::Result<()> {
        let (key, value) = (wide(RUN_KEY), wide(VALUE));
        // SAFETY: the strings are NUL-terminated and outlive the call.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(status)
    }

    fn check(status: u32) -> io::Result<()> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::io;
    use std::path::PathBuf;

    use super::BACKGROUND_FLAG;

    fn entry() -> io::Result<PathBuf> {
        let config = dirs::config_dir().ok_or_else(|| io::Error::other("no config folder"))?;
        Ok(config.join("autostart").join("vivace.desktop"))
    }

    pub fn entry_exists() -> bool {
        entry().is_ok_and(|path| path.exists())
    }

    /// The command the entry runs, if there is one.
    pub fn entry_command() -> Option<String> {
        let contents = std::fs::read_to_string(entry().ok()?).ok()?;
        contents
            .lines()
            .find_map(|line| line.strip_prefix("Exec="))
            .map(str::to_owned)
    }

    pub fn write_entry(program: &str) -> io::Result<()> {
        let path = entry()?;
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        std::fs::write(
            path,
            format!(
                "[Desktop Entry]\nType=Application\nName=Vivace\nComment=Spotify player\n\
                 Exec=\"{program}\" {BACKGROUND_FLAG}\nTerminal=false\n\
                 X-GNOME-Autostart-enabled=true\n"
            ),
        )
    }

    pub fn remove_entry() -> io::Result<()> {
        match std::fs::remove_file(entry()?) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_the_quoted_start_of_the_command() {
        assert_eq!(
            program_of(r#""C:\Program Files\Vivace\vivace.exe" --background"#),
            Some(r"C:\Program Files\Vivace\vivace.exe")
        );
        assert_eq!(program_of("vivace --background"), None);
    }
}
