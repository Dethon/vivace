//! Checks for a newer Vivace on GitHub as the app does, and downloads and
//! verifies its Windows installer. Touches nothing but `<directory>`.
//!
//!   release                          compares the latest release with this build
//!   release <version> [directory]    pretends to be <version>; with a
//!                                    directory, downloads a newer installer there

use std::path::Path;

use vivace_core::release::{self, Version};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut arguments = std::env::args().skip(1);
    let current: Version = match arguments.next() {
        Some(version) => version.parse().expect("a version such as 0.4.0"),
        None => Version::current(),
    };
    let directory = arguments.next();

    let latest = match release::latest().await {
        Ok(latest) => latest,
        Err(error) => {
            println!("FAIL couldn't check: {error}");
            std::process::exit(1);
        }
    };
    println!("latest {} at {}", latest.version, latest.page);
    if latest.version <= current {
        println!("PASS {current} is up to date");
        return;
    }
    println!("{current} is behind");
    let Some(directory) = directory else { return };
    match latest.download_installer(Path::new(&directory)).await {
        Ok(path) => {
            let size = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
            println!("PASS verified {} ({size} bytes)", path.display());
        }
        Err(error) => {
            println!("FAIL {error}");
            std::process::exit(1);
        }
    }
}
