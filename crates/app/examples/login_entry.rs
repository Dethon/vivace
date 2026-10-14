//! Turns the "Open Vivace when you sign in" entry on and back off, printing
//! what the system holds, and leaves it as it found it. `on` or `off` as an
//! argument just sets it, to inspect the entry by hand.
//!
//!   cargo run -p vivace --example login_entry [on|off]

use vivace::system;

fn main() -> std::io::Result<()> {
    if let Some(state) = std::env::args().nth(1) {
        system::set_start_at_login(state == "on")?;
        println!("now: {}", system::starts_at_login());
        return Ok(());
    }
    let before = system::starts_at_login();
    println!("before: {before}");
    system::set_start_at_login(true)?;
    println!("after turning on: {}", system::starts_at_login());
    system::set_start_at_login(false)?;
    println!("after turning off: {}", system::starts_at_login());
    system::set_start_at_login(before)?;
    println!("restored: {}", system::starts_at_login());
    Ok(())
}
