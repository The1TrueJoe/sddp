//! Search the network and print what answers.
//!
//! ```text
//! cargo run --example discover -- [target] [seconds] [interface-ip]
//! cargo run --example discover -- '*' 4
//! cargo run --example discover -- acme:widget 2
//! ```

use std::net::Ipv4Addr;
use std::process::ExitCode;
use std::time::Duration;

use sddp::Discovery;

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let target = args.next().unwrap_or_else(|| "*".to_owned());
    let seconds: f64 = match args.next().map(|s| s.parse()) {
        None => 3.0,
        Some(Ok(s)) if s > 0.0 => s,
        Some(_) => {
            eprintln!(
                "usage: discover [target] [seconds] [interface-ip]  (seconds must be a positive number)"
            );
            return ExitCode::from(2);
        }
    };
    let interface: Option<Ipv4Addr> = match args.next().map(|s| s.parse()) {
        None => None,
        Some(Ok(ip)) => Some(ip),
        Some(Err(_)) => {
            eprintln!(
                "usage: discover [target] [seconds] [interface-ip]  (interface must be an IPv4 address)"
            );
            return ExitCode::from(2);
        }
    };

    let mut search = Discovery::new()
        .target(target.clone())
        .timeout(Duration::from_secs_f64(seconds));
    if let Some(ip) = interface {
        search = search.interface(ip);
    }
    let on = interface.map(Ok).unwrap_or_else(sddp::default_interface);
    match &on {
        Ok(ip) => eprintln!("searching for {target:?} on {ip} for {seconds}s..."),
        Err(_) => eprintln!("searching for {target:?} for {seconds}s..."),
    }

    let mut devices = match search.discover().await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    devices.sort_by(|a, b| a.address.cmp(&b.address).then(a.host.cmp(&b.host)));

    let rows: Vec<[String; 5]> = devices
        .iter()
        .map(|d| {
            [
                d.address.to_string(),
                d.device_type.clone(),
                d.manufacturer.clone().unwrap_or_default(),
                d.model.clone().unwrap_or_default(),
                d.host.clone(),
            ]
        })
        .collect();
    let header = ["ADDRESS", "TYPE", "MANUFACTURER", "MODEL", "HOST"].map(String::from);
    let mut widths = header.clone().map(|h| h.len());
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
    eprintln!("{} device(s)", devices.len());
    ExitCode::SUCCESS
}
