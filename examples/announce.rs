//! Pretend to be a device: announce it, answer searches, say goodbye on Ctrl-C.
//!
//! ```text
//! cargo run --example announce -- [host] [type]
//! cargo run --example announce -- widget-demo-01 acme:widget
//! ```
//!
//! Then, in another terminal: `cargo run --example discover -- acme:widget`.

use std::process::ExitCode;
use std::time::Duration;

use sddp::Announcer;

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let host = args
        .next()
        .unwrap_or_else(|| format!("widget-demo-{}", std::process::id()));
    let device_type = args.next().unwrap_or_else(|| "acme:widget".to_owned());

    let announcer = Announcer::builder(host.clone(), device_type.clone())
        .manufacturer("Acme")
        .model("W-100")
        .primary_proxy("widget")
        .proxies(["widget"])
        .interval(Duration::from_secs(60))
        .start()
        .await;
    let announcer = match announcer {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "announcing {host:?} ({device_type}) from {} — Ctrl-C to stop",
        announcer.interface()
    );

    if let Err(e) = tokio::signal::ctrl_c().await {
        eprintln!("error waiting for Ctrl-C: {e}");
    }
    match announcer.shutdown().await {
        Ok(()) => {
            eprintln!("sent NOTIFY OFFLINE");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
