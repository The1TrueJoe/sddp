//! SDDP, the Simple Device Discovery Protocol.
//!
//! SDDP is a small, HTTP-like text protocol over UDP multicast that devices
//! on a local network use to announce themselves and that clients use to
//! find them. Messages go to group `239.255.255.250`, port `1902`.
//!
//! A device announces itself:
//!
//! ```text
//! NOTIFY ALIVE SDDP/1.0
//! From: "192.168.1.109:1902"
//! Host: "widget-70b3d50c1637"
//! Max-Age: 1800
//! Type: "acme:widget"
//! Primary-Proxy: "widget"
//! Proxies: "widget"
//! Manufacturer: "Acme"
//! Model: "W-100"
//! Driver: "acme_widget.drv"
//! Config-URL: "http://192.168.1.109"
//! ```
//!
//! A client searches with `SEARCH * SDDP/1.0` (or an exact type instead of
//! `*`), and each matching device answers with `SDDP/1.0 200 OK` and the same
//! headers. `NOTIFY OFFLINE` says a device is leaving.
//!
//! # What's here
//!
//! * [`Message`], [`Headers`]: parse and format messages. Always available,
//!   no networking, no async runtime.
//! * [`Device`], [`Event`]: a typed view of what a device said.
//! * With the `tokio` feature (on by default):
//!   [`Discovery`] searches and collects answers, [`Listener`] / [`watch`]
//!   follow announcements passively, and [`Announcer`] makes this process
//!   act as a device.
//!
//! ```no_run
//! # async fn demo() -> std::io::Result<()> {
//! use std::time::Duration;
//!
//! for device in sddp::discover(Duration::from_secs(3)).await? {
//!     println!("{:<15} {:<24} {}", device.address, device.device_type, device.host);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Network caveats
//!
//! * **Search targets.** Devices answer `*` and their exact type only.
//!   Prefix patterns such as `acme:*` are not wildcards; no device answers them.
//! * **Sharing port 1902.** Sockets are opened with `SO_REUSEADDR` (and
//!   `SO_REUSEPORT` on Unix) so several SDDP programs can run on one machine.
//!   Searches go out from a separate ephemeral port, because most devices
//!   reply to the source port; replies addressed to port 1902 are heard too.
//! * **Interfaces.** By default the interface is the one the OS routes the
//!   multicast group through (see [`default_interface`]). On a machine with
//!   several networks, pass the right interface's IPv4 address explicitly.
//! * **macOS 15+** requires Local Network permission. Command-line tools
//!   inherit it from the terminal app; app bundles must declare
//!   `NSLocalNetworkUsageDescription` in `Info.plist`. Without it, sends fail
//!   with "No route to host".
//! * **Windows** Firewall must allow UDP port 1902 in and out.

#![warn(missing_docs)]

mod device;
mod headers;
mod message;

#[cfg(feature = "tokio")]
mod announcer;
#[cfg(feature = "tokio")]
mod discovery;
#[cfg(feature = "tokio")]
mod listener;
#[cfg(feature = "tokio")]
mod net;

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

pub use device::{Device, DeviceError, Event};
pub use headers::{Header, Headers, should_quote};
pub use message::{Message, NotifyKind, ParseError, VERSION};

#[cfg(feature = "tokio")]
pub use announcer::{Announcer, AnnouncerBuilder};
#[cfg(feature = "tokio")]
pub use discovery::{Discovery, discover};
#[cfg(feature = "tokio")]
pub use listener::{Listener, watch};

/// The SDDP multicast group, `239.255.255.250`.
pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);

/// The SDDP UDP port, `1902`.
pub const PORT: u16 = 1902;

/// The IPv4 address of the interface the OS would use to reach the SDDP
/// multicast group, i.e. the address other devices will see this machine at.
///
/// No packet is sent: this "connects" a UDP socket, which only makes the
/// kernel choose a route and a source address.
///
/// Fails when there is no IPv4 route to `239.255.255.250` (no network, or
/// only loopback); pass an interface address explicitly in that case.
pub fn default_interface() -> io::Result<Ipv4Addr> {
    let fail = |detail: String| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!(
                "could not work out which network interface to use for SDDP ({detail}); \
                 check the machine is on a network, or pass the interface's IPv4 address explicitly"
            ),
        )
    };
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|e| fail(format!("could not open a UDP socket: {e}")))?;
    probe
        .connect((MULTICAST_ADDR, PORT))
        .map_err(|e| fail(format!("no route to {MULTICAST_ADDR}: {e}")))?;
    match probe.local_addr()? {
        SocketAddr::V4(a) if !a.ip().is_unspecified() => Ok(*a.ip()),
        other => Err(fail(format!("the OS picked {other}"))),
    }
}
