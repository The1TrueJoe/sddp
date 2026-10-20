//! Follow announcements passively and print devices as they appear and leave.
//!
//! ```text
//! cargo run --example watch -- [interface-ip]
//! ```
//!
//! Devices repeat `NOTIFY ALIVE` every few minutes, so a quiet network can
//! take a while to show everything. Run the `discover` example in another
//! terminal to make devices answer now: their replies to it are not seen
//! here, but devices that also announce on search will appear.

use std::collections::HashMap;
use std::process::ExitCode;

use sddp::Event;

#[tokio::main]
async fn main() -> ExitCode {
    let interface = match std::env::args().nth(1).map(|s| s.parse()) {
        None => None,
        Some(Ok(ip)) => Some(ip),
        Some(Err(_)) => {
            eprintln!("usage: watch [interface-ip]");
            return ExitCode::from(2);
        }
    };
    let mut events = match sddp::watch(interface).await {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("listening for SDDP announcements (Ctrl-C to stop)...");

    // Host -> number of announcements heard, to print only changes.
    let mut seen: HashMap<String, u32> = HashMap::new();
    loop {
        let event = tokio::select! {
            _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
            e = events.recv() => e,
        };
        match event {
            Some(Ok(Event::Alive(d))) => {
                let count = seen.entry(d.host.clone()).or_default();
                *count += 1;
                if *count == 1 {
                    println!(
                        "+ {:<15} {:<28} {} {}",
                        d.address,
                        d.device_type,
                        d.model.as_deref().unwrap_or("-"),
                        d.host
                    );
                }
            }
            Some(Ok(Event::Offline(d))) => {
                seen.remove(&d.host);
                println!("- {:<15} {:<28} {}", d.address, d.device_type, d.host);
            }
            Some(Err(e)) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
            None => return ExitCode::SUCCESS,
        }
    }
}
