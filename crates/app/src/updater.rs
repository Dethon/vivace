//! Looks for new versions of Vivace. Where Vivace was installed with its
//! Windows installer, the new installer is downloaded ahead and runs when
//! the user asks; elsewhere the user is pointed to the release page.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc::UnboundedSender;
use vivace_core::release::{self, Version};

use crate::backend::Update;

/// Set while a check runs, so a click on "Check now" doesn't start a second.
static CHECKING: AtomicBool = AtomicBool::new(false);

/// What a check found.
pub enum Found {
    UpToDate,
    Newer(Available),
    Failed(String),
}

/// A release newer than this copy of Vivace.
#[derive(Clone)]
pub struct Available {
    pub version: String,
    /// The release page.
    pub page: String,
    /// The verified installer, ready to run; none where Vivace doesn't
    /// update itself or the download failed.
    pub installer: Option<PathBuf>,
}

/// Checks in the background and reports through `updates`; `asked` says
/// the user started it. Does nothing while another check runs.
pub fn check(updates: UnboundedSender<Update>, asked: bool) {
    if CHECKING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("updates".into())
        .spawn(move || {
            // A runtime of its own, gone with the thread, keeps the check
            // independent of the Spotify session and costs nothing between checks.
            let found = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(look()),
                Err(error) => Found::Failed(error.to_string()),
            };
            CHECKING.store(false, Ordering::Release);
            _ = updates.send(Update::Release { found, asked });
        });
    if let Err(error) = spawned {
        CHECKING.store(false, Ordering::Release);
        log::warn!("couldn't start the update check: {error}");
    }
}

async fn look() -> Found {
    let latest = match release::latest().await {
        Ok(latest) => latest,
        Err(error) => {
            log::info!("couldn't check for updates: {error}");
            return Found::Failed(error.to_string());
        }
    };
    let current = Version::current();
    if latest.version <= current {
        log::debug!("up to date: the latest release is {}", latest.version);
        return Found::UpToDate;
    }
    log::info!("Vivace {} is available", latest.version);
    let installer = match downloads() {
        Some(directory) => match latest.download_installer(&directory).await {
            Ok(path) => Some(path),
            // The release page still has it.
            Err(error) => {
                log::warn!("couldn't download Vivace {}: {error}", latest.version);
                None
            }
        },
        None => None,
    };
    Found::Newer(Available {
        version: latest.version.to_string(),
        page: latest.page,
        installer,
    })
}

// Written by the installer next to vivace.exe.
#[cfg(windows)]
const UNINSTALLER: &str = "uninstall.exe";

/// Where installers are downloaded to, where Vivace updates itself: a copy
/// that its installer put there, which the installer can replace. A copy
/// unzipped elsewhere would gain a second, installed one instead.
#[cfg(windows)]
fn downloads() -> Option<PathBuf> {
    let program = std::env::current_exe().ok()?;
    if !program.with_file_name(UNINSTALLER).is_file() {
        return None;
    }
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("vivace").join("updates"))
}

/// Linux packages are updated by their package manager or by hand.
#[cfg(not(windows))]
fn downloads() -> Option<PathBuf> {
    None
}

/// Deletes downloaded installers of this version or older, which have
/// done their job, and unfinished downloads.
pub fn clean_up() {
    let Some(directory) = downloads() else { return };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let current = Version::current();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let installed = release::installer_version(&name).is_none_or(|version| version <= current);
        if installed && let Err(error) = std::fs::remove_file(entry.path()) {
            log::debug!("couldn't delete {name}: {error}");
        }
    }
}

/// Starts `installer` without its pages. It waits for this copy of Vivace
/// to quit, which the caller then does, and opens the new version.
pub fn install(installer: &Path) -> io::Result<()> {
    std::process::Command::new(installer)
        .args(["/S", "/relaunch"])
        .spawn()
        .map(drop)
}
