# sddp

[![CI](https://github.com/The1TrueJoe/sddp/actions/workflows/ci.yml/badge.svg)](https://github.com/The1TrueJoe/sddp/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/sddp.svg)](https://crates.io/crates/sddp)
[![docs.rs](https://docs.rs/sddp/badge.svg)](https://docs.rs/sddp)

A Rust implementation of **SDDP, the Simple Device Discovery Protocol**: the
small HTTP-like text protocol that devices on a local network use to announce
themselves over UDP multicast, and that clients use to find them.

- Parse and format SDDP messages (no networking, no runtime needed).
- Discover devices: send a search and collect who answers.
- Watch announcements passively as devices come and go.
- Announce a device of your own and answer searches for it.

## The protocol

Messages are UDP datagrams sent to multicast group **`239.255.255.250`, port
`1902`**. Each is a start line plus `Name: value` headers, lines ending in
CRLF, finished by a blank line. Values are usually in double quotes; numbers
usually are not.

A device announces itself when it starts and every few minutes after:

```text
NOTIFY ALIVE SDDP/1.0
From: "192.168.1.109:1902"
Host: "widget-70b3d50c1637"
Max-Age: 1800
Type: "acme:widget"
Primary-Proxy: "widget"
Proxies: "widget"
Manufacturer: "Acme"
Model: "W-100"
Driver: "acme_widget.drv"
Config-URL: "http://192.168.1.109"
```

and says goodbye with `NOTIFY OFFLINE SDDP/1.0` (same headers, at least
`From`, `Host` and `Type`).

A client searches for every device (`*`) or one exact type:

```text
SEARCH * SDDP/1.0
From: "192.168.1.20:1902"
Tran: 41267
Timeout: 3
```

and each matching device replies by unicast:

```text
SDDP/1.0 200 OK
From: "192.168.1.109:1902"
Host: "widget-70b3d50c1637"
Tran: 41267
Max-Age: 1800
Type: "acme:widget"
...
```

| Header | Meaning |
| --- | --- |
| `From` | The device's `ip:port` (port is normally 1902). |
| `Host` | Unique name of the device, often containing its MAC address. |
| `Type` | Device type, e.g. `acme:widget`. What searches match against. |
| `Max-Age` | Seconds the announcement stays valid without a refresh. |
| `Primary-Proxy`, `Proxies` | The device's main role and all roles (comma-separated). |
| `Manufacturer`, `Model` | What the device is. |
| `Driver` | Name of the integration file a controller should use for it. |
| `Config-URL` | The device's own configuration page. |
| `Tran` | Transaction id of a search, often echoed in replies. |
| `Timeout` | How long the searcher will wait, in seconds. |

Unknown headers are preserved in order, and lookups are case-insensitive.

## Quick start

```toml
[dependencies]
sddp = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal"] }
```

### Discover

```rust
use std::time::Duration;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let devices = sddp::Discovery::new()
        .target("*")                       // or an exact type: "example:receiver"
        .timeout(Duration::from_secs(3))
        .discover()
        .await?;
    for d in devices {
        println!("{:<15} {:<24} {:?} {}", d.address, d.device_type, d.model, d.host);
    }
    Ok(())
}
```

`discover()` collects search replies **and** `NOTIFY ALIVE` announcements
heard during the window, filters them by the target, and de-duplicates by
`Host` (latest message wins).

### Watch

```rust
use sddp::Event;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut listener = sddp::Listener::bind(None).await?;
    loop {
        match listener.next().await? {
            Event::Alive(d) => println!("alive   {} {}", d.address, d.host),
            Event::Offline(d) => println!("offline {} {}", d.address, d.host),
        }
    }
}
```

Or `sddp::watch(None).await?` for a Tokio `mpsc::Receiver` of events fed by a
background task. Devices repeat announcements, so expect the same device
many times.

### Announce

```rust
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let announcer = sddp::Announcer::builder("widget-0a1b2c3d4e5f", "acme:widget")
        .manufacturer("Acme")
        .model("W-100")
        .primary_proxy("widget")
        .proxies(["widget"])
        .start()                 // sends NOTIFY ALIVE, starts answering searches
        .await?;
    tokio::signal::ctrl_c().await?;
    announcer.shutdown().await   // sends NOTIFY OFFLINE
}
```

The announcer repeats `NOTIFY ALIVE` every five minutes (configurable),
answers `SEARCH *` and `SEARCH <its exact type>` with a `200 OK` sent back to
the searcher, and sends `NOTIFY OFFLINE` on `shutdown()` (or, best effort,
when dropped).

### Parse and format only

```toml
sddp = { version = "0.1", default-features = false }
```

```rust
use sddp::{Device, Message};

let msg = Message::parse(b"NOTIFY ALIVE SDDP/1.0\r\nFrom: \"10.0.0.5:1902\"\r\n\
Host: \"rx-1\"\r\nType: \"example:receiver\"\r\n\r\n")?;
let device = Device::from_message(&msg, None)?;
assert_eq!(device.device_type, "example:receiver");

let mut search = Message::search("example:receiver");
search.headers_mut().set("Tran", "7");
assert_eq!(search.to_string(), "SEARCH example:receiver SDDP/1.0\r\nTran: 7\r\n\r\n");
```

The parser is lenient where real devices are sloppy (bare LF line endings,
no final blank line, a blank line in the middle of the headers, no space
after the colon, stray whitespace, invalid UTF-8) and returns a descriptive
`ParseError` for anything that is not SDDP.

## Examples

```sh
cargo run --example discover -- '*' 4          # [target] [seconds] [interface-ip]
cargo run --example watch                      # [interface-ip]
cargo run --example announce -- widget-demo-01 acme:widget
```

## Things to know

- **Search targets are `*` or an exact type.** Devices do not implement
  prefix wildcards: `SEARCH acme:* SDDP/1.0` gets no answers. Search `*` and
  filter locally instead.
- **Replies arrive two ways.** Most devices reply to the search's source
  address and port; some reply to the port in its `From` header (1902)
  instead. `Discovery` sends from a private ephemeral port and also listens
  on 1902, so it hears both. Some devices answer a search with
  `NOTIFY ALIVE` instead of a reply; those are collected too.
- **Port sharing.** Sockets on 1902 use `SO_REUSEADDR` (plus `SO_REUSEPORT`
  on Unix), so this crate coexists with other SDDP software on the same
  machine.
- **Choosing an interface.** By default the interface is the one the OS
  routes `239.255.255.250` through, found without sending anything
  (`sddp::default_interface()`). On machines with several networks, pass the
  right interface's IPv4 address to `.interface(...)` / `Listener::bind`.
- **macOS 15 and later** require Local Network permission for multicast and
  LAN traffic. Command-line programs inherit it from the terminal app (you
  may be prompted once); app bundles must include
  `NSLocalNetworkUsageDescription` in their `Info.plist`. Without
  permission, sends fail with "No route to host" and this crate's error says
  so.
- **Windows**: the firewall must allow UDP port 1902 inbound and outbound
  (Windows asks the first time a program listens).
- **IPv4 only.** SDDP is used on IPv4 networks; IPv6 is not implemented.
- **Multicast TTL** is 2 and multicast loopback is on, so programs on the
  same machine see each other.

## Minimum supported Rust version

Rust **1.85** (edition 2024). Raising it is a minor-version change.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
