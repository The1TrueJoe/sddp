//! The device side: announce yourself and answer searches.
//!
//! An [`Announcer`] makes this process look like an SDDP device: it multicasts
//! `NOTIFY ALIVE` on start and periodically after that, answers each matching
//! `SEARCH` with a unicast `SDDP/1.0 200 OK`, and multicasts `NOTIFY OFFLINE`
//! when shut down or dropped.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::device::target_matches;
use crate::headers::{Header, Headers};
use crate::message::{Message, NotifyKind};
use crate::net;
use crate::{MULTICAST_ADDR, PORT};

/// Configuration for an [`Announcer`]; create one with [`Announcer::builder`].
#[derive(Clone, Debug)]
pub struct AnnouncerBuilder {
    host: String,
    device_type: String,
    primary_proxy: Option<String>,
    proxies: Vec<String>,
    manufacturer: Option<String>,
    model: Option<String>,
    driver: Option<String>,
    config_url: Option<String>,
    extra: Vec<(String, String)>,
    max_age: u32,
    interval: Duration,
    interface: Option<Ipv4Addr>,
}

impl AnnouncerBuilder {
    /// `Primary-Proxy`: the main role the device plays.
    pub fn primary_proxy(mut self, value: impl Into<String>) -> Self {
        self.primary_proxy = Some(value.into());
        self
    }

    /// `Proxies`: every role the device plays (written comma-separated).
    pub fn proxies<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.proxies = values.into_iter().map(Into::into).collect();
        self
    }

    /// `Manufacturer`.
    pub fn manufacturer(mut self, value: impl Into<String>) -> Self {
        self.manufacturer = Some(value.into());
        self
    }

    /// `Model`.
    pub fn model(mut self, value: impl Into<String>) -> Self {
        self.model = Some(value.into());
        self
    }

    /// `Driver`: the name of the integration file controllers should use.
    pub fn driver(mut self, value: impl Into<String>) -> Self {
        self.driver = Some(value.into());
        self
    }

    /// `Config-URL`: the device's configuration page.
    pub fn config_url(mut self, value: impl Into<String>) -> Self {
        self.config_url = Some(value.into());
        self
    }

    /// Any other header, sent after the standard ones. Quoted unless the
    /// value is a plain integer.
    ///
    /// # Panics
    ///
    /// When the announcer starts, if `name` is empty or contains whitespace,
    /// a colon or a double quote.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra.push((name.into(), value.into()));
        self
    }

    /// `Max-Age` in seconds: how long listeners should remember the device
    /// without hearing from it. Default 1800 (30 minutes).
    pub fn max_age(mut self, seconds: u32) -> Self {
        self.max_age = seconds;
        self
    }

    /// How often to repeat `NOTIFY ALIVE`. Default five minutes; keep it well
    /// under the max age so a lost packet doesn't make the device expire.
    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval.max(Duration::from_millis(100));
        self
    }

    /// The IPv4 address of the interface to announce on. Defaults to the
    /// interface the OS routes multicast through.
    pub fn interface(mut self, interface: Ipv4Addr) -> Self {
        self.interface = Some(interface);
        self
    }

    /// The headers this device sends, with `Tran` (from a search) placed
    /// after `Host` as devices do.
    fn headers(&self, interface: Ipv4Addr, tran: Option<&Header>) -> Headers {
        let mut h = Headers::new();
        h.set("From", format!("{interface}:{PORT}"));
        h.set("Host", self.host.as_str());
        if let Some(tran) = tran {
            // Echo it the way the searcher wrote it.
            h.set_quoted("Tran", tran.value(), tran.is_quoted());
        }
        h.set("Max-Age", self.max_age.to_string());
        h.set("Type", self.device_type.as_str());
        let mut optional = |name: &str, value: &Option<String>| {
            if let Some(v) = value {
                h.set(name, v.as_str());
            }
        };
        optional("Primary-Proxy", &self.primary_proxy);
        let proxies = (!self.proxies.is_empty()).then(|| self.proxies.join(","));
        optional("Proxies", &proxies);
        optional("Manufacturer", &self.manufacturer);
        optional("Model", &self.model);
        optional("Driver", &self.driver);
        optional("Config-URL", &self.config_url);
        for (name, value) in &self.extra {
            h.append(name.as_str(), value.as_str());
        }
        h
    }

    /// Opens the socket, sends the first `NOTIFY ALIVE`, and starts answering
    /// searches in the background.
    ///
    /// Errors if the socket cannot be opened or the first announcement cannot
    /// be sent; the message says why and what to try. Later send failures in
    /// the background are ignored (the next interval retries).
    pub async fn start(self) -> io::Result<Announcer> {
        let interface = match self.interface {
            Some(i) => i,
            None => crate::default_interface()?,
        };
        let socket = Arc::new(net::multicast_socket(interface)?);
        let alive = Message::notify(NotifyKind::Alive, self.headers(interface, None)).to_bytes();
        let offline =
            Message::notify(NotifyKind::Offline, self.headers(interface, None)).to_bytes();

        socket
            .send_to(&alive, (MULTICAST_ADDR, PORT))
            .await
            .map_err(|e| {
                net::context(
                    e,
                    format!("sending the first SDDP announcement from {interface}"),
                )
            })?;

        let (stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run(socket.clone(), self, interface, alive, stop_rx));
        Ok(Announcer {
            socket,
            interface,
            offline,
            stop: Some(stop_tx),
            task: Some(task),
        })
    }
}

async fn run(
    socket: Arc<UdpSocket>,
    config: AnnouncerBuilder,
    interface: Ipv4Addr,
    alive: Vec<u8>,
    mut stop: oneshot::Receiver<()>,
) {
    let group = SocketAddr::from((MULTICAST_ADDR, PORT));
    let mut ticker = tokio::time::interval_at(Instant::now() + config.interval, config.interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut buf = vec![0u8; net::RECV_BUFFER];
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = ticker.tick() => {
                let _ = socket.send_to(&alive, group).await;
            }
            r = socket.recv_from(&mut buf) => {
                let (n, from) = match r {
                    Ok(r) => r,
                    Err(e) if net::is_transient(&e) => continue,
                    Err(_) => {
                        // Don't spin on a persistent error; try again shortly.
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                };
                let Ok(Message::Search { target, headers }) = Message::parse(&buf[..n]) else {
                    continue;
                };
                if target_matches(&target, &config.device_type) {
                    let reply = Message::ok(config.headers(interface, headers.get_header("Tran")));
                    // Reply to where the search came from: that is where
                    // searchers listen for answers.
                    let _ = socket.send_to(&reply.to_bytes(), from).await;
                }
            }
        }
    }
}

/// A running SDDP device announcement. See [`Announcer::builder`].
///
/// Dropping it stops the background task and makes a best-effort attempt to
/// send `NOTIFY OFFLINE`; call [`shutdown`](Self::shutdown) to know whether
/// that was sent.
#[derive(Debug)]
pub struct Announcer {
    socket: Arc<UdpSocket>,
    interface: Ipv4Addr,
    offline: Vec<u8>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Announcer {
    /// Starts configuring a device with its unique `host` name (put a MAC
    /// address or serial number in it) and its `device_type`, e.g. `acme:widget`.
    ///
    /// ```no_run
    /// # async fn demo() -> std::io::Result<()> {
    /// let announcer = sddp::Announcer::builder("widget-0a1b2c3d4e5f", "acme:widget")
    ///     .manufacturer("Acme")
    ///     .model("W-100")
    ///     .primary_proxy("widget")
    ///     .proxies(["widget"])
    ///     .config_url("http://192.168.1.50/")
    ///     .start()
    ///     .await?;
    /// // ... run the device ...
    /// announcer.shutdown().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder(host: impl Into<String>, device_type: impl Into<String>) -> AnnouncerBuilder {
        AnnouncerBuilder {
            host: host.into(),
            device_type: device_type.into(),
            primary_proxy: None,
            proxies: Vec::new(),
            manufacturer: None,
            model: None,
            driver: None,
            config_url: None,
            extra: Vec::new(),
            max_age: 1800,
            interval: Duration::from_secs(300),
            interface: None,
        }
    }

    /// The interface address the device is announced on (and in its `From` header).
    pub fn interface(&self) -> Ipv4Addr {
        self.interface
    }

    /// Stops answering searches and multicasts `NOTIFY OFFLINE`.
    pub async fn shutdown(mut self) -> io::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        self.socket
            .send_to(&self.offline, (MULTICAST_ADDR, PORT))
            .await
            .map(drop)
            .map_err(|e| net::context(e, "sending SDDP NOTIFY OFFLINE"))
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            // Not shut down explicitly: say goodbye without blocking.
            let _ = self
                .socket
                .try_send_to(&self.offline, SocketAddr::from((MULTICAST_ADDR, PORT)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_headers_in_device_order() {
        let b = Announcer::builder("widget-01", "acme:widget")
            .manufacturer("Acme")
            .model("W-100")
            .driver("acme_widget.drv")
            .primary_proxy("widget")
            .proxies(["widget", "amplifier"])
            .config_url("http://10.0.0.5")
            .header("X-Firmware", "2.1")
            .max_age(900);
        let msg = Message::notify(
            NotifyKind::Alive,
            b.headers(Ipv4Addr::new(10, 0, 0, 5), None),
        );
        assert_eq!(
            msg.to_string(),
            "NOTIFY ALIVE SDDP/1.0\r\n\
             From: \"10.0.0.5:1902\"\r\n\
             Host: \"widget-01\"\r\n\
             Max-Age: 900\r\n\
             Type: \"acme:widget\"\r\n\
             Primary-Proxy: \"widget\"\r\n\
             Proxies: \"widget,amplifier\"\r\n\
             Manufacturer: \"Acme\"\r\n\
             Model: \"W-100\"\r\n\
             Driver: \"acme_widget.drv\"\r\n\
             Config-URL: \"http://10.0.0.5\"\r\n\
             X-Firmware: \"2.1\"\r\n\r\n"
        );
    }

    #[test]
    fn response_echoes_tran_as_written() {
        let b = Announcer::builder("rx-1", "example:receiver");
        let search: Message = "SEARCH * SDDP/1.0\r\nTran: \"77\"\r\n".parse().unwrap();
        let tran = search.headers().get_header("Tran");
        let reply = Message::ok(b.headers(Ipv4Addr::new(10, 0, 0, 6), tran));
        assert_eq!(
            reply.to_string(),
            "SDDP/1.0 200 OK\r\nFrom: \"10.0.0.6:1902\"\r\nHost: \"rx-1\"\r\nTran: \"77\"\r\n\
             Max-Age: 1800\r\nType: \"example:receiver\"\r\n\r\n"
        );
        let device = crate::Device::from_message(&reply, None).unwrap();
        assert_eq!(device.host, "rx-1");
        assert_eq!(device.address.to_string(), "10.0.0.6");
    }
}
