//! Passive monitoring: hear devices come and go without searching.
//!
//! A [`Listener`] only receives; it never sends. Devices announce themselves
//! periodically (every few minutes is typical) and when they start, so a
//! listener on its own can take a while to see everything. Run a
//! [`Discovery`](crate::Discovery) first if you need a complete list now.

use std::io;
use std::net::Ipv4Addr;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::device::Event;
use crate::message::Message;
use crate::net;

/// Receives SDDP announcements on UDP port 1902.
///
/// ```no_run
/// # async fn demo() -> std::io::Result<()> {
/// use sddp::{Event, Listener};
///
/// let mut listener = Listener::bind(None).await?;
/// loop {
///     match listener.next().await? {
///         Event::Alive(d) => println!("alive   {} {}", d.address, d.host),
///         Event::Offline(d) => println!("offline {} {}", d.address, d.host),
///     }
/// }
/// # }
/// ```
#[derive(Debug)]
pub struct Listener {
    socket: UdpSocket,
    interface: Ipv4Addr,
    buf: Vec<u8>,
}

impl Listener {
    /// Joins the SDDP multicast group on `interface` (or the default
    /// interface when `None`) and starts receiving. The port is shared, so
    /// this works alongside other SDDP software on the same machine.
    pub async fn bind(interface: Option<Ipv4Addr>) -> io::Result<Listener> {
        let interface = match interface {
            Some(i) => i,
            None => crate::default_interface()?,
        };
        Ok(Listener {
            socket: net::multicast_socket(interface)?,
            interface,
            buf: vec![0u8; net::RECV_BUFFER],
        })
    }

    /// The interface address the listener joined the group on.
    pub fn interface(&self) -> Ipv4Addr {
        self.interface
    }

    /// Waits for the next alive or offline event.
    ///
    /// `NOTIFY ALIVE`, `NOTIFY OFFLINE`, and `200 OK` replies to anyone's
    /// search that arrive on port 1902 are reported; searches, unparseable
    /// datagrams and messages without a `Host` are skipped. Devices repeat
    /// announcements, so the same device shows up many times.
    pub async fn next(&mut self) -> io::Result<Event> {
        loop {
            let (n, from) = match self.socket.recv_from(&mut self.buf).await {
                Ok(r) => r,
                Err(e) if net::is_transient(&e) => continue,
                Err(e) => return Err(net::context(e, "receiving SDDP announcements")),
            };
            let Ok(message) = Message::parse(&self.buf[..n]) else {
                continue;
            };
            if let Some(event) = Event::from_message(&message, Some(from)) {
                return Ok(event);
            }
        }
    }

    /// Moves the listener onto a background task and returns a channel of its
    /// events. The task stops when the receiver is dropped, or after sending
    /// the first error.
    ///
    /// Must be called from within a Tokio runtime.
    pub fn into_channel(mut self, capacity: usize) -> mpsc::Receiver<io::Result<Event>> {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tx.closed() => break,
                    r = self.next() => {
                        let failed = r.is_err();
                        if tx.send(r).await.is_err() || failed {
                            break;
                        }
                    }
                }
            }
        });
        rx
    }
}

/// Starts a [`Listener`] on `interface` (or the default) in the background and
/// returns a channel of its events.
///
/// ```no_run
/// # async fn demo() -> std::io::Result<()> {
/// let mut events = sddp::watch(None).await?;
/// while let Some(event) = events.recv().await {
///     println!("{:?}", event?);
/// }
/// # Ok(())
/// # }
/// ```
pub async fn watch(interface: Option<Ipv4Addr>) -> io::Result<mpsc::Receiver<io::Result<Event>>> {
    Ok(Listener::bind(interface).await?.into_channel(64))
}
