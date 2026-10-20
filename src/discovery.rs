//! Active discovery: send a `SEARCH`, collect who answers for a while.
//!
//! Not a long-running monitor; for that use [`Listener`](crate::Listener).

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::net::UdpSocket;
use tokio::time::{Instant, sleep_until};

use crate::device::{Device, Event};
use crate::message::Message;
use crate::net;
use crate::{MULTICAST_ADDR, PORT};

/// How long after the first search to send a second copy. UDP multicast is
/// lossy (especially on Wi-Fi) and devices answer every copy; duplicates are
/// merged.
const RESEND_AFTER: Duration = Duration::from_millis(500);

/// Searches the local network for SDDP devices.
///
/// ```no_run
/// # async fn demo() -> std::io::Result<()> {
/// use std::time::Duration;
///
/// let devices = sddp::Discovery::new()
///     .target("*")
///     .timeout(Duration::from_secs(3))
///     .discover()
///     .await?;
/// for d in devices {
///     println!("{} {} {}", d.address, d.device_type, d.host);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Discovery {
    target: String,
    timeout: Duration,
    interface: Option<Ipv4Addr>,
}

impl Default for Discovery {
    fn default() -> Self {
        Discovery {
            target: "*".to_owned(),
            timeout: Duration::from_secs(3),
            interface: None,
        }
    }
}

impl Discovery {
    /// A search for every device (`*`) lasting three seconds on the default interface.
    pub fn new() -> Self {
        Self::default()
    }

    /// What to search for: `*` (the default) or an exact device type such as
    /// `acme:widget`.
    ///
    /// Devices only answer `*` and their exact type. Prefix patterns such as
    /// `acme:*` are sent as-is but no device answers them, and only an exact
    /// type match is collected from announcements.
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = target.into();
        self
    }

    /// How long to collect answers (default three seconds). Devices on busy
    /// or wireless networks can take a second or two to reply.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The IPv4 address of the interface to search on. Defaults to the
    /// interface the OS routes multicast through (see [`crate::default_interface`]).
    pub fn interface(mut self, interface: Ipv4Addr) -> Self {
        self.interface = Some(interface);
        self
    }

    /// Sends the search and returns every device heard before the timeout:
    /// replies to this search, replies to other clients' searches, and
    /// `NOTIFY ALIVE` announcements, filtered by the target and
    /// de-duplicated by `Host` (the latest message wins; order is first
    /// heard). A device that sends `NOTIFY OFFLINE` during the window is
    /// dropped.
    ///
    /// Errors if the sockets cannot be opened or the search cannot be sent;
    /// the message says why and what to try.
    pub async fn discover(&self) -> io::Result<Vec<Device>> {
        let interface = match self.interface {
            Some(i) => i,
            None => crate::default_interface()?,
        };
        // Replies go either to the search's source port (most devices) or to
        // the port in its `From` header (some devices), so listen on both: the
        // shared multicast socket on 1902, and a private ephemeral socket.
        let group = net::multicast_socket(interface)?;
        let search_socket = net::search_socket(interface)?;

        let search = self.search_message(interface);
        let start = Instant::now();
        let deadline = start + self.timeout;
        let resend_at = start + RESEND_AFTER.min(self.timeout / 2);
        send_search(&search_socket, &search, interface).await?;
        let mut resent = false;

        let mut found = DeviceSet::default();
        let mut buf_group = vec![0u8; net::RECV_BUFFER];
        let mut buf_search = vec![0u8; net::RECV_BUFFER];
        loop {
            let wake = if resent { deadline } else { resend_at };
            tokio::select! {
                r = group.recv_from(&mut buf_group) => {
                    let (n, from) = received(r)?;
                    found.observe(&buf_group[..n], from, &self.target);
                }
                r = search_socket.recv_from(&mut buf_search) => {
                    let (n, from) = received(r)?;
                    found.observe(&buf_search[..n], from, &self.target);
                }
                _ = sleep_until(wake) => {
                    if resent {
                        break;
                    }
                    send_search(&search_socket, &search, interface).await?;
                    resent = true;
                }
            }
        }
        Ok(found.into_vec())
    }

    fn search_message(&self, interface: Ipv4Addr) -> Vec<u8> {
        let mut msg = Message::search(self.target.clone());
        let h = msg.headers_mut();
        h.set("From", format!("{interface}:{PORT}"));
        h.set("Tran", transaction_id().to_string());
        // Whole seconds, rounded up, at least one.
        let secs = self.timeout.as_secs() + u64::from(self.timeout.subsec_nanos() > 0);
        h.set("Timeout", secs.max(1).to_string());
        msg.to_bytes()
    }
}

async fn send_search(socket: &UdpSocket, search: &[u8], interface: Ipv4Addr) -> io::Result<()> {
    socket
        .send_to(search, (MULTICAST_ADDR, PORT))
        .await
        .map(drop)
        .map_err(|e| net::context(e, format!("sending an SDDP search from {interface}")))
}

/// Shorthand for `Discovery::new().timeout(timeout).discover()`.
pub async fn discover(timeout: Duration) -> io::Result<Vec<Device>> {
    Discovery::new().timeout(timeout).discover().await
}

/// Turns a transient receive error into "nothing received" (an empty read).
fn received(r: io::Result<(usize, SocketAddr)>) -> io::Result<(usize, SocketAddr)> {
    match r {
        Ok(ok) => Ok(ok),
        Err(e) if net::is_transient(&e) => Ok((0, SocketAddr::from(([0, 0, 0, 0], 0)))),
        Err(e) => Err(net::context(e, "receiving SDDP replies")),
    }
}

/// A transaction id that is unlikely to repeat across runs or calls.
fn transaction_id() -> u32 {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ d.as_secs() as u32)
        .unwrap_or(0);
    let mixed =
        nanos ^ std::process::id().rotate_left(16) ^ COUNTER.fetch_add(1, Ordering::Relaxed);
    // Keep it positive and readable; some devices echo it back as a decimal.
    mixed % 1_000_000_000
}

/// Devices collected during a search, de-duplicated by `Host`.
#[derive(Debug, Default)]
pub(crate) struct DeviceSet {
    devices: Vec<Device>,
}

impl DeviceSet {
    /// Feeds one datagram in. Garbage, searches (including our own, looped
    /// back) and devices that don't match the target are ignored.
    pub(crate) fn observe(&mut self, datagram: &[u8], from: SocketAddr, target: &str) {
        if datagram.is_empty() {
            return;
        }
        let Ok(message) = Message::parse(datagram) else {
            return;
        };
        match Event::from_message(&message, Some(from)) {
            Some(Event::Alive(d)) if d.matches(target) => self.insert(d),
            Some(Event::Offline(d)) => self.remove(&d.host),
            _ => {}
        }
    }

    /// Adds a device or replaces the one with the same `Host`, keeping its position.
    pub(crate) fn insert(&mut self, device: Device) {
        match self.devices.iter_mut().find(|d| d.host == device.host) {
            Some(existing) => *existing = device,
            None => self.devices.push(device),
        }
    }

    pub(crate) fn remove(&mut self, host: &str) {
        self.devices.retain(|d| d.host != host);
    }

    pub(crate) fn into_vec(self) -> Vec<Device> {
        self.devices
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn alive(host: &str, ty: &str, ip: &str, model: &str) -> Vec<u8> {
        format!(
            "NOTIFY ALIVE SDDP/1.0\r\nFrom: \"{ip}:1902\"\r\nHost: \"{host}\"\r\n\
             Type: \"{ty}\"\r\nModel: \"{model}\"\r\nMax-Age: 1800\r\n\r\n"
        )
        .into_bytes()
    }

    fn ok(host: &str, ty: &str, ip: &str, model: &str) -> Vec<u8> {
        format!(
            "SDDP/1.0 200 OK\r\nFrom: \"{ip}:1902\"\r\nHost: \"{host}\"\r\nTran: 5\r\n\
             Type: \"{ty}\"\r\nModel: \"{model}\"\r\n\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn dedups_by_host_latest_wins_first_order_kept() {
        let mut set = DeviceSet::default();
        let s = src("10.0.0.5:40000");
        set.observe(&ok("a", "acme:widget", "10.0.0.5", "v1"), s, "*");
        set.observe(&alive("b", "example:receiver", "10.0.0.6", "r1"), s, "*");
        set.observe(&alive("a", "acme:widget", "10.0.0.9", "v2"), s, "*");
        set.observe(&ok("a", "acme:widget", "10.0.0.9", "v3"), s, "*");
        let found = set.into_vec();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].host, "a");
        assert_eq!(found[0].model.as_deref(), Some("v3"));
        assert_eq!(found[0].address.to_string(), "10.0.0.9");
        assert_eq!(found[1].host, "b");
    }

    #[test]
    fn filters_by_target() {
        let mut set = DeviceSet::default();
        let s = src("10.0.0.5:1902");
        set.observe(
            &alive("a", "acme:widget", "10.0.0.5", "m"),
            s,
            "acme:widget",
        );
        set.observe(
            &alive("b", "example:receiver", "10.0.0.6", "m"),
            s,
            "acme:widget",
        );
        set.observe(
            &ok("c", "acme:widget-pro", "10.0.0.7", "m"),
            s,
            "acme:widget",
        );
        let hosts: Vec<_> = set.into_vec().into_iter().map(|d| d.host).collect();
        assert_eq!(hosts, ["a"]);

        // A prefix "wildcard" is literal, as on real devices.
        let mut set = DeviceSet::default();
        set.observe(&alive("a", "acme:widget", "10.0.0.5", "m"), s, "acme:*");
        assert!(set.into_vec().is_empty());
    }

    #[test]
    fn ignores_searches_garbage_and_errors() {
        let mut set = DeviceSet::default();
        let s = src("10.0.0.5:1902");
        set.observe(
            b"SEARCH * SDDP/1.0\r\nFrom: \"10.0.0.5:1902\"\r\nHost: \"me\"\r\n",
            s,
            "*",
        );
        set.observe(
            b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n\r\n",
            s,
            "*",
        );
        set.observe(b"", s, "*");
        set.observe(b"\xff\xfe", s, "*");
        set.observe(b"SDDP/1.0 500 Error\r\nHost: \"x\"\r\n", s, "*");
        set.observe(
            b"NOTIFY ALIVE SDDP/1.0\r\nType: \"acme:widget\"\r\n",
            s,
            "*",
        );
        assert!(set.into_vec().is_empty());
    }

    #[test]
    fn offline_removes_device() {
        let mut set = DeviceSet::default();
        let s = src("10.0.0.5:1902");
        set.observe(&alive("a", "acme:widget", "10.0.0.5", "m"), s, "*");
        set.observe(&alive("b", "acme:widget", "10.0.0.6", "m"), s, "*");
        set.observe(
            b"NOTIFY OFFLINE SDDP/1.0\r\nFrom: \"10.0.0.5:1902\"\r\nHost: \"a\"\r\n",
            s,
            "*",
        );
        let hosts: Vec<_> = set.into_vec().into_iter().map(|d| d.host).collect();
        assert_eq!(hosts, ["b"]);
    }

    #[test]
    fn search_message_format() {
        let d = Discovery::new()
            .target("example:receiver")
            .timeout(Duration::from_millis(2500));
        let msg = Message::parse(&d.search_message(Ipv4Addr::new(10, 0, 0, 2))).unwrap();
        let Message::Search { target, headers } = &msg else {
            panic!()
        };
        assert_eq!(target, "example:receiver");
        assert_eq!(headers.get("From"), Some("10.0.0.2:1902"));
        assert_eq!(headers.get("Timeout"), Some("3"));
        assert!(headers.get("Tran").unwrap().parse::<u32>().is_ok());
        assert!(!headers.get_header("Tran").unwrap().is_quoted());

        let d = Discovery::new().timeout(Duration::from_millis(10));
        let msg = Message::parse(&d.search_message(Ipv4Addr::LOCALHOST)).unwrap();
        assert_eq!(msg.headers().get("Timeout"), Some("1"));
    }
}
