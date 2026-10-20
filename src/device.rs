//! A typed view of a device, built from an announcement or a search response.
//!
//! [`Device`] pulls the well-known headers out of a [`Message`] and keeps the
//! complete header list for anything else. It does no networking.

use std::net::{IpAddr, SocketAddr};

use crate::PORT;
use crate::headers::Headers;
use crate::message::{Message, NotifyKind};

/// A device, as described by a `NOTIFY` or a search response.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Device {
    /// The device's IP address, from the `From` header, or the datagram's
    /// source address when `From` is missing or unreadable.
    pub address: IpAddr,
    /// The device's SDDP port, from the `From` header (normally 1902).
    pub port: u16,
    /// `Host`: the device's unique name on the network (often includes its MAC).
    pub host: String,
    /// `Type`: the device type, e.g. `acme:widget`. Empty if the header is missing.
    pub device_type: String,
    /// `Primary-Proxy`: the main role the device plays.
    pub primary_proxy: Option<String>,
    /// `Proxies`: every role the device plays (comma-separated on the wire).
    pub proxies: Vec<String>,
    /// `Manufacturer`.
    pub manufacturer: Option<String>,
    /// `Model`.
    pub model: Option<String>,
    /// `Driver`: the name of the integration file that controls this device.
    pub driver: Option<String>,
    /// `Config-URL`: where the device's own configuration page lives.
    pub config_url: Option<String>,
    /// `Max-Age`: seconds this announcement stays valid without a refresh.
    pub max_age: Option<u32>,
    /// Every header of the message, including the ones above and unknown ones.
    pub headers: Headers,
}

/// Why a message could not be turned into a [`Device`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DeviceError {
    /// The message is a `SEARCH`, which describes the client, not a device.
    #[error("a SEARCH message describes a client looking for devices, not a device")]
    NotADevice,
    /// The message is a response with a non-2xx status.
    #[error("SDDP response with status {0} does not describe a device")]
    ErrorStatus(u16),
    /// The required `Host` header is missing or empty.
    #[error("SDDP message has no Host header, so the device cannot be identified")]
    MissingHost,
    /// No usable `From` header and no source address was supplied.
    #[error("SDDP message has no usable From header and no source address was given")]
    MissingAddress,
}

/// Something a device said about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The device is present: a `NOTIFY ALIVE` or a `200 OK` search response.
    /// Devices repeat these, so expect the same device more than once.
    Alive(Device),
    /// The device is leaving: a `NOTIFY OFFLINE`.
    Offline(Device),
}

impl Event {
    /// Interprets a message, returning `None` for anything that is not a
    /// usable alive/offline signal (searches, other notify kinds, error
    /// responses, messages without a `Host`).
    pub fn from_message(message: &Message, source: Option<SocketAddr>) -> Option<Event> {
        let device = Device::from_message(message, source).ok()?;
        match message {
            Message::Notify {
                kind: NotifyKind::Alive,
                ..
            }
            | Message::Response { .. } => Some(Event::Alive(device)),
            Message::Notify {
                kind: NotifyKind::Offline,
                ..
            } => Some(Event::Offline(device)),
            _ => None,
        }
    }

    /// The device the event is about.
    pub fn device(&self) -> &Device {
        match self {
            Event::Alive(d) | Event::Offline(d) => d,
        }
    }
}

fn non_empty(headers: &Headers, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Reads a `From` value: `ip:port`, `[v6]:port`, or a bare IP.
fn parse_from(value: &str) -> Option<(IpAddr, u16)> {
    let value = value.trim();
    if let Ok(sa) = value.parse::<SocketAddr>() {
        return Some((sa.ip(), sa.port()));
    }
    value.parse::<IpAddr>().ok().map(|ip| (ip, PORT))
}

impl Device {
    /// Builds a device from a `NOTIFY` or a 2xx response.
    ///
    /// `source` is where the datagram came from; it is used for the address
    /// only when the `From` header is missing or unreadable (responses often
    /// come from an ephemeral port while `From` names the real SDDP port).
    pub fn from_message(
        message: &Message,
        source: Option<SocketAddr>,
    ) -> Result<Device, DeviceError> {
        match message {
            Message::Search { .. } => return Err(DeviceError::NotADevice),
            Message::Response { status, .. } if !(200..300).contains(status) => {
                return Err(DeviceError::ErrorStatus(*status));
            }
            _ => {}
        }
        let headers = message.headers();
        let host = non_empty(headers, "Host").ok_or(DeviceError::MissingHost)?;
        let (address, port) = headers
            .get("From")
            .and_then(parse_from)
            .or_else(|| source.map(|s| (s.ip(), PORT)))
            .ok_or(DeviceError::MissingAddress)?;
        let proxies = headers
            .get("Proxies")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Device {
            address,
            port,
            host,
            device_type: non_empty(headers, "Type").unwrap_or_default(),
            primary_proxy: non_empty(headers, "Primary-Proxy"),
            proxies,
            manufacturer: non_empty(headers, "Manufacturer"),
            model: non_empty(headers, "Model"),
            driver: non_empty(headers, "Driver"),
            config_url: non_empty(headers, "Config-URL"),
            max_age: headers.get("Max-Age").and_then(|v| v.trim().parse().ok()),
            headers: headers.clone(),
        })
    }

    /// Whether this device answers a search for `target`: `*` matches every
    /// device, anything else must equal the device type exactly.
    ///
    /// This is how devices behave on the wire; in particular a prefix such as
    /// `acme:*` is **not** a wildcard and matches nothing.
    pub fn matches(&self, target: &str) -> bool {
        target_matches(target, &self.device_type)
    }
}

/// The search matching rule shared by [`Device::matches`] and the announcer.
pub(crate) fn target_matches(target: &str, device_type: &str) -> bool {
    let target = target.trim();
    target == "*" || target == device_type
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive() -> Message {
        "NOTIFY ALIVE SDDP/1.0\r\n\
From: \"192.168.1.109:1902\"\r\n\
Host: \"widget-70b3d50c1637\"\r\n\
Max-Age: 1800\r\n\
Type: \"acme:widget\"\r\n\
Primary-Proxy: \"avswitch\"\r\n\
Proxies: \"avswitch, amplifier,,\"\r\n\
Manufacturer: \"Acme\"\r\n\
Model: \"W-100\"\r\n\
Driver: \"acme_widget_ip.drv\"\r\n\
Config-URL: \"http://192.168.1.109\"\r\n\
X-Firmware: \"2.1\"\r\n\r\n"
            .parse()
            .unwrap()
    }

    #[test]
    fn reads_every_known_header() {
        let source: SocketAddr = "192.168.1.109:38177".parse().unwrap();
        let d = Device::from_message(&alive(), Some(source)).unwrap();
        assert_eq!(d.address, "192.168.1.109".parse::<IpAddr>().unwrap());
        assert_eq!(d.port, 1902);
        assert_eq!(d.host, "widget-70b3d50c1637");
        assert_eq!(d.device_type, "acme:widget");
        assert_eq!(d.primary_proxy.as_deref(), Some("avswitch"));
        assert_eq!(d.proxies, ["avswitch", "amplifier"]);
        assert_eq!(d.manufacturer.as_deref(), Some("Acme"));
        assert_eq!(d.model.as_deref(), Some("W-100"));
        assert_eq!(d.driver.as_deref(), Some("acme_widget_ip.drv"));
        assert_eq!(d.config_url.as_deref(), Some("http://192.168.1.109"));
        assert_eq!(d.max_age, Some(1800));
        assert_eq!(d.headers.get("x-firmware"), Some("2.1"));
    }

    #[test]
    fn falls_back_to_source_address() {
        let mut msg = alive();
        msg.headers_mut().remove("From");
        let source: SocketAddr = "10.1.2.3:40000".parse().unwrap();
        let d = Device::from_message(&msg, Some(source)).unwrap();
        assert_eq!(d.address, source.ip());
        assert_eq!(d.port, PORT);

        msg.headers_mut().set("From", "not an address");
        let d = Device::from_message(&msg, Some(source)).unwrap();
        assert_eq!(d.address, source.ip());

        assert_eq!(
            Device::from_message(&msg, None),
            Err(DeviceError::MissingAddress)
        );
    }

    #[test]
    fn from_header_variants() {
        assert_eq!(
            parse_from("10.0.0.1"),
            Some(("10.0.0.1".parse().unwrap(), PORT))
        );
        assert_eq!(
            parse_from(" 10.0.0.1:5000 "),
            Some(("10.0.0.1".parse().unwrap(), 5000))
        );
        assert_eq!(
            parse_from("[fe80::1]:1902"),
            Some(("fe80::1".parse().unwrap(), 1902))
        );
        assert_eq!(parse_from("host.local:1902"), None);
    }

    #[test]
    fn minimal_and_invalid_messages() {
        let msg: Message = "SDDP/1.0 200 OK\r\nHost: \"x\"\r\nFrom: \"10.0.0.7\"\r\n"
            .parse()
            .unwrap();
        let d = Device::from_message(&msg, None).unwrap();
        assert_eq!(d.device_type, "");
        assert!(d.proxies.is_empty());
        assert_eq!(d.max_age, None);

        let msg: Message = "SDDP/1.0 200 OK\r\nHost: \"x\"\r\nMax-Age: soon\r\n"
            .parse()
            .unwrap();
        let src = Some("10.0.0.7:1902".parse().unwrap());
        assert_eq!(Device::from_message(&msg, src).unwrap().max_age, None);

        let msg: Message = "SDDP/1.0 200 OK\r\nHost: \"  \"\r\n".parse().unwrap();
        assert_eq!(
            Device::from_message(&msg, src),
            Err(DeviceError::MissingHost)
        );

        let msg: Message = "SDDP/1.0 500 Oops\r\nHost: \"x\"\r\n".parse().unwrap();
        assert_eq!(
            Device::from_message(&msg, src),
            Err(DeviceError::ErrorStatus(500))
        );

        assert_eq!(
            Device::from_message(&Message::search("*"), src),
            Err(DeviceError::NotADevice)
        );
    }

    #[test]
    fn events_from_messages() {
        let src = Some("192.168.1.109:1902".parse().unwrap());
        let ev = Event::from_message(&alive(), src).unwrap();
        assert!(matches!(ev, Event::Alive(_)));
        assert_eq!(ev.device().host, "widget-70b3d50c1637");

        let mut off = alive();
        if let Message::Notify { kind, .. } = &mut off {
            *kind = NotifyKind::Offline;
        }
        assert!(matches!(
            Event::from_message(&off, src),
            Some(Event::Offline(_))
        ));

        let ok = Message::ok(alive().headers().clone());
        assert!(matches!(
            Event::from_message(&ok, src),
            Some(Event::Alive(_))
        ));

        let other = Message::notify(
            NotifyKind::Other("UPDATE".into()),
            alive().headers().clone(),
        );
        assert_eq!(Event::from_message(&other, src), None);
        assert_eq!(Event::from_message(&Message::search("*"), src), None);
    }

    #[test]
    fn search_matching_rules() {
        let d = Device::from_message(&alive(), None).unwrap();
        assert!(d.matches("*"));
        assert!(d.matches(" * "));
        assert!(d.matches("acme:widget"));
        assert!(!d.matches("acme:*"));
        assert!(!d.matches("acme"));
        assert!(!d.matches("ACME:WIDGET"));
        assert!(!d.matches("example:receiver"));
    }
}
