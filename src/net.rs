//! Socket setup shared by discovery, listening and announcing.
//!
//! Everything here exists so several SDDP programs can run on one machine:
//! the listening socket uses `SO_REUSEADDR` (and `SO_REUSEPORT` on Unix) so it
//! can share UDP port 1902, and searches are sent from a separate socket on an
//! ephemeral port so that unicast replies reach this process rather than
//! whichever other process the kernel picks among those sharing port 1902.

use std::fmt::Display;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};

use crate::{MULTICAST_ADDR, PORT};

/// Multicast TTL for everything we send. SDDP is link-local in practice; 2
/// (the UPnP default) allows for one router hop on networks that forward it.
const MULTICAST_TTL: u32 = 2;

/// Adds a short, actionable explanation to an I/O error.
pub(crate) fn context(err: io::Error, doing: impl Display) -> io::Error {
    let hint = hint_for(&err);
    io::Error::new(err.kind(), format!("{doing}: {err}{hint}"))
}

fn hint_for(err: &io::Error) -> &'static str {
    // EHOSTUNREACH is what macOS returns for multicast/LAN sends when the
    // app has not been granted Local Network access.
    #[cfg(target_os = "macos")]
    if err.raw_os_error() == Some(65) {
        return ". On macOS 15 and later this usually means Local Network access is denied: \
                allow the app (or the terminal running it) under System Settings > Privacy & \
                Security > Local Network; app bundles must declare NSLocalNetworkUsageDescription";
    }
    match err.kind() {
        io::ErrorKind::AddrInUse => {
            ". Another program holds UDP port 1902 without allowing it to be shared; stop it and try again"
        }
        io::ErrorKind::PermissionDenied if cfg!(windows) => {
            ". Windows Firewall may be blocking SDDP; allow inbound and outbound UDP port 1902"
        }
        io::ErrorKind::PermissionDenied => ". The OS or a firewall refused access to UDP port 1902",
        _ => "",
    }
}

fn udp_socket() -> io::Result<socket2::Socket> {
    socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
}

fn set_multicast_send(sock: &socket2::Socket, interface: Ipv4Addr) -> io::Result<()> {
    sock.set_multicast_if_v4(&interface)?;
    sock.set_multicast_ttl_v4(MULTICAST_TTL)?;
    // Loopback stays on so programs on the same machine (including an
    // announcer and a discovery in one process) can see each other. Our own
    // packets are filtered out by message type, not by turning this off.
    sock.set_multicast_loop_v4(true)
}

fn into_tokio(sock: socket2::Socket) -> io::Result<tokio::net::UdpSocket> {
    sock.set_nonblocking(true)?;
    tokio::net::UdpSocket::from_std(sock.into())
}

/// Lets other sockets bind the same port. `SO_REUSEADDR` is enough on
/// Windows; Unix also needs `SO_REUSEPORT` (where socket2 supports it).
// The cfg mirrors socket2's own for `set_reuse_port`. Older compilers (the
// MSRV) don't know every one of these target names, hence the allow.
#[allow(unexpected_cfgs)]
fn share_port(sock: &socket2::Socket) -> io::Result<()> {
    sock.set_reuse_address(true)?;
    #[cfg(all(
        unix,
        not(any(
            target_os = "solaris",
            target_os = "illumos",
            target_os = "cygwin",
            target_os = "nuttx"
        ))
    ))]
    sock.set_reuse_port(true)?;
    Ok(())
}

/// A socket bound to `0.0.0.0:1902` that has joined the SDDP group on
/// `interface` and multicasts out of it. Shares the port with other programs.
pub(crate) fn multicast_socket(interface: Ipv4Addr) -> io::Result<tokio::net::UdpSocket> {
    let doing = || format!("opening the SDDP socket on UDP port {PORT} (interface {interface})");
    let sock = udp_socket().map_err(|e| context(e, doing()))?;
    share_port(&sock).map_err(|e| context(e, doing()))?;
    // Bind the wildcard address, not the interface address: on Linux a socket
    // bound to a unicast address never receives multicast.
    let bind = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT);
    sock.bind(&bind.into()).map_err(|e| context(e, doing()))?;
    sock.join_multicast_v4(&MULTICAST_ADDR, &interface)
        .map_err(|e| {
            context(
                e,
                format!("joining the SDDP multicast group {MULTICAST_ADDR} on {interface}"),
            )
        })?;
    set_multicast_send(&sock, interface).map_err(|e| context(e, doing()))?;
    into_tokio(sock).map_err(|e| context(e, doing()))
}

/// A socket on an ephemeral port of `interface`, used to send searches and
/// receive the unicast replies that devices send back to the source port.
pub(crate) fn search_socket(interface: Ipv4Addr) -> io::Result<tokio::net::UdpSocket> {
    let doing = || format!("opening a socket to send SDDP searches from {interface}");
    let sock = udp_socket().map_err(|e| context(e, doing()))?;
    sock.bind(&SocketAddrV4::new(interface, 0).into())
        .map_err(|e| context(e, doing()))?;
    set_multicast_send(&sock, interface).map_err(|e| context(e, doing()))?;
    into_tokio(sock).map_err(|e| context(e, doing()))
}

/// Whether a receive error is a per-packet hiccup to skip rather than a
/// reason to stop. Windows reports an earlier ICMP "port unreachable" as
/// `ConnectionReset` on the next receive; truncated or interrupted reads are
/// harmless too.
pub(crate) fn is_transient(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
    ) || cfg!(windows) && err.raw_os_error() == Some(10040) // WSAEMSGSIZE
}

/// The receive buffer size. SDDP datagrams are a few hundred bytes; anything
/// longer than this is truncated and parsed as far as it goes.
pub(crate) const RECV_BUFFER: usize = 8 * 1024;
