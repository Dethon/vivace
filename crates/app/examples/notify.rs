//! Shows the notification Vivace shows for a new song, where the system
//! has them.
//!
//!   cargo run -p vivace --example notify

fn main() {
    vivace::notify::announce("Get Lucky".into(), "Daft Punk".into());
    // The notification is sent from a thread of its own.
    std::thread::sleep(std::time::Duration::from_secs(1));
}
