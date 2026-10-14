//! Shows the notifications Vivace shows for two songs in a row, where the
//! system has them. With `--serve` it stands in for the desktop's
//! notification service and checks what arrives: the second notification
//! must replace the first.
//!
//!   cargo run -p vivace --example notify [-- --serve]

use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    let server = if std::env::args().any(|argument| argument == "--serve") {
        Some(server::start()?)
    } else {
        None
    };

    vivace::notify::announce("Get Lucky".into(), "Daft Punk".into());
    // Each notification is sent from a thread of its own.
    std::thread::sleep(Duration::from_millis(500));
    vivace::notify::announce("Instant Crush".into(), "Daft Punk".into());
    std::thread::sleep(Duration::from_millis(500));

    #[cfg(target_os = "linux")]
    if let Some(server) = server {
        let received = server.received();
        for call in &received {
            println!(
                "#{} (replacing {}): {} / {} for {} ms",
                call.id, call.replaces, call.summary, call.body, call.timeout
            );
        }
        let replaced = received.len() == 2 && received[1].replaces == received[0].id;
        println!("two notifications, the second replacing the first: {replaced}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod server {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use zbus::zvariant::OwnedValue;

    #[derive(Clone)]
    pub struct Call {
        pub id: u32,
        pub replaces: u32,
        pub summary: String,
        pub body: String,
        pub timeout: i32,
    }

    pub struct Server {
        _connection: zbus::blocking::Connection,
        calls: Arc<Mutex<Vec<Call>>>,
    }

    impl Server {
        pub fn received(&self) -> Vec<Call> {
            self.calls
                .lock()
                .map(|calls| calls.clone())
                .unwrap_or_default()
        }
    }

    struct Notifications {
        calls: Arc<Mutex<Vec<Call>>>,
    }

    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl Notifications {
        #[allow(clippy::too_many_arguments)]
        fn notify(
            &self,
            _app: String,
            replaces: u32,
            _icon: String,
            summary: String,
            body: String,
            _actions: Vec<String>,
            _hints: HashMap<String, OwnedValue>,
            timeout: i32,
        ) -> u32 {
            let mut calls = self.calls.lock().expect("not poisoned");
            // A replaced notification keeps its id, as real services do.
            let id = if replaces != 0 {
                replaces
            } else {
                calls.len() as u32 + 1
            };
            calls.push(Call {
                id,
                replaces,
                summary,
                body,
                timeout,
            });
            id
        }
    }

    pub fn start() -> zbus::Result<Server> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let connection = zbus::blocking::connection::Builder::session()?
            .name("org.freedesktop.Notifications")?
            .serve_at(
                "/org/freedesktop/Notifications",
                Notifications {
                    calls: calls.clone(),
                },
            )?
            .build()?;
        Ok(Server {
            _connection: connection,
            calls,
        })
    }
}
