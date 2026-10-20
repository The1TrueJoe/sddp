//! SDDP messages: parsing a datagram into a [`Message`] and formatting one back.
//!
//! An SDDP message is one UDP datagram of HTTP-like text: a start line, then
//! `Name: value` header lines, each ending in CRLF, then a blank line. The
//! parser is deliberately lenient about what real devices get wrong (bare LF,
//! a missing final blank line, stray blank lines between headers, extra
//! whitespace) and strict only where guessing would produce nonsense.

use std::fmt;
use std::str::FromStr;

use crate::headers::{Header, Headers};

/// The protocol version written on every message this crate formats.
pub const VERSION: &str = "SDDP/1.0";

/// The kind of a `NOTIFY` message.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NotifyKind {
    /// `NOTIFY ALIVE`: the device is present (sent periodically and on startup).
    Alive,
    /// `NOTIFY OFFLINE`: the device is leaving the network.
    Offline,
    /// Any other notification kind, kept verbatim.
    Other(String),
}

impl NotifyKind {
    /// The kind as written on the wire (`ALIVE`, `OFFLINE`, or the original text).
    pub fn as_str(&self) -> &str {
        match self {
            NotifyKind::Alive => "ALIVE",
            NotifyKind::Offline => "OFFLINE",
            NotifyKind::Other(s) => s,
        }
    }

    fn from_wire(s: &str) -> Self {
        if s.eq_ignore_ascii_case("ALIVE") {
            NotifyKind::Alive
        } else if s.eq_ignore_ascii_case("OFFLINE") {
            NotifyKind::Offline
        } else {
            NotifyKind::Other(s.to_owned())
        }
    }
}

impl fmt::Display for NotifyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One SDDP message.
///
/// ```
/// use sddp::{Message, NotifyKind};
///
/// let msg = Message::parse(
///     b"NOTIFY ALIVE SDDP/1.0\r\n\
///       From: \"192.168.1.20:1902\"\r\n\
///       Host: \"widget-01\"\r\n\
///       Type: \"acme:widget\"\r\n\
///       Max-Age: 1800\r\n\r\n",
/// )
/// .unwrap();
///
/// assert!(matches!(msg, Message::Notify { kind: NotifyKind::Alive, .. }));
/// assert_eq!(msg.headers().get("type"), Some("acme:widget"));
/// assert_eq!(Message::parse(&msg.to_bytes()).unwrap(), msg);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// `NOTIFY <kind> SDDP/1.0`, multicast by a device.
    Notify {
        /// `ALIVE`, `OFFLINE` or something else.
        kind: NotifyKind,
        /// The message headers.
        headers: Headers,
    },
    /// `SEARCH <target> SDDP/1.0`, multicast by a client looking for devices.
    Search {
        /// What to look for: `*` for every device, or an exact device type.
        /// Devices do not implement prefix wildcards such as `acme:*`.
        target: String,
        /// The message headers.
        headers: Headers,
    },
    /// `SDDP/1.0 <status> <reason>`, a device's unicast answer to a search.
    Response {
        /// The status code, `200` for a normal answer.
        status: u16,
        /// The reason phrase, usually `OK`.
        reason: String,
        /// The message headers.
        headers: Headers,
    },
}

/// Why a datagram could not be parsed as an SDDP message.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// The datagram is empty or contains only whitespace.
    #[error("empty SDDP message: the datagram contained no start line")]
    Empty,
    /// The first line is not an SDDP start line.
    #[error(
        "not an SDDP message: start line {0:?} should look like `NOTIFY ALIVE SDDP/1.0`, \
         `SEARCH * SDDP/1.0` or `SDDP/1.0 200 OK`"
    )]
    NotSddp(String),
    /// A request line with an SDDP version but a method other than NOTIFY or SEARCH.
    #[error("unsupported SDDP method {0:?}: only NOTIFY and SEARCH are understood")]
    UnknownMethod(String),
    /// `NOTIFY` without a kind or `SEARCH` without a target.
    #[error("SDDP {method} line is missing its {what} (expected `{method} <{what}> SDDP/1.0`)")]
    MissingArgument {
        /// `NOTIFY` or `SEARCH`.
        method: &'static str,
        /// `kind` or `target`.
        what: &'static str,
    },
    /// A response line whose status is not a three-digit number.
    #[error("bad SDDP response status {0:?}: expected a three-digit code such as 200")]
    BadStatus(String),
    /// A header line without a `Name:` part.
    #[error("malformed SDDP header on line {line}: {text:?} has no `Name:` part")]
    BadHeader {
        /// 1-based line number within the datagram.
        line: usize,
        /// The offending line (truncated to 80 characters).
        text: String,
    },
}

fn is_version(token: &str) -> bool {
    token.len() > 5
        && token
            .get(..5)
            .is_some_and(|p| p.eq_ignore_ascii_case("SDDP/"))
}

fn truncate(s: &str) -> String {
    match s.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

impl Message {
    /// A `SEARCH` for `target` with no headers yet.
    ///
    /// Use `"*"` for every device or an exact type such as `"acme:widget"`.
    pub fn search(target: impl Into<String>) -> Self {
        Message::Search {
            target: target.into(),
            headers: Headers::new(),
        }
    }

    /// A `NOTIFY` of the given kind with the given headers.
    pub fn notify(kind: NotifyKind, headers: Headers) -> Self {
        Message::Notify { kind, headers }
    }

    /// A `SDDP/1.0 200 OK` response with the given headers.
    pub fn ok(headers: Headers) -> Self {
        Message::Response {
            status: 200,
            reason: "OK".to_owned(),
            headers,
        }
    }

    /// Parses one datagram.
    ///
    /// Bytes that are not valid UTF-8 are replaced with U+FFFD rather than
    /// rejected, so one odd character in a model name does not hide a device.
    pub fn parse(bytes: &[u8]) -> Result<Message, ParseError> {
        let text = String::from_utf8_lossy(bytes);
        text.parse()
    }

    /// The headers of any kind of message.
    pub fn headers(&self) -> &Headers {
        match self {
            Message::Notify { headers, .. }
            | Message::Search { headers, .. }
            | Message::Response { headers, .. } => headers,
        }
    }

    /// Mutable access to the headers of any kind of message.
    pub fn headers_mut(&mut self) -> &mut Headers {
        match self {
            Message::Notify { headers, .. }
            | Message::Search { headers, .. }
            | Message::Response { headers, .. } => headers,
        }
    }

    /// The message as it goes on the wire (CRLF line endings, trailing blank line).
    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }
}

impl FromStr for Message {
    type Err = ParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        // Some stacks pad datagrams with NULs.
        let text = text.trim_end_matches('\0');
        let mut lines = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty());

        let (_, start) = lines.next().ok_or(ParseError::Empty)?;
        let start = start.trim();
        let mut tokens = start.split_whitespace();
        let first = tokens.next().ok_or(ParseError::Empty)?;

        let mut message = if is_version(first) {
            let status_text = tokens.next().unwrap_or("");
            let status = match status_text.parse::<u16>() {
                Ok(code) if status_text.len() == 3 => code,
                _ => return Err(ParseError::BadStatus(status_text.to_owned())),
            };
            // Everything after the status, internal spacing preserved.
            let reason = start[first.len()..].trim_start()[status_text.len()..].trim();
            Message::Response {
                status,
                reason: reason.to_owned(),
                headers: Headers::new(),
            }
        } else {
            let last = start.rsplit(char::is_whitespace).next().unwrap_or("");
            if !is_version(last) || last.len() == start.len() {
                return Err(ParseError::NotSddp(truncate(start)));
            }
            // The argument may contain spaces (device types sometimes do), so
            // take everything between the method and the version.
            let argument = start[first.len()..start.len() - last.len()].trim();
            if first.eq_ignore_ascii_case("NOTIFY") {
                if argument.is_empty() {
                    return Err(ParseError::MissingArgument {
                        method: "NOTIFY",
                        what: "kind",
                    });
                }
                Message::Notify {
                    kind: NotifyKind::from_wire(argument),
                    headers: Headers::new(),
                }
            } else if first.eq_ignore_ascii_case("SEARCH") {
                if argument.is_empty() {
                    return Err(ParseError::MissingArgument {
                        method: "SEARCH",
                        what: "target",
                    });
                }
                Message::Search {
                    target: argument.to_owned(),
                    headers: Headers::new(),
                }
            } else {
                return Err(ParseError::UnknownMethod(truncate(first)));
            }
        };

        // Blank lines are skipped rather than treated as the end of the
        // message: a datagram is already a complete frame, and at least one
        // device in the wild puts a blank line in the middle of its headers.
        let headers = message.headers_mut();
        for (index, line) in lines {
            let header = Header::parse_line(line).ok_or_else(|| ParseError::BadHeader {
                line: index + 1,
                text: truncate(line),
            })?;
            headers.push(header);
        }
        Ok(message)
    }
}

impl fmt::Display for Message {
    /// The full wire text, including CRLFs and the final blank line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::Notify { kind, .. } => write!(f, "NOTIFY {kind} {VERSION}\r\n")?,
            Message::Search { target, .. } => write!(f, "SEARCH {target} {VERSION}\r\n")?,
            Message::Response { status, reason, .. } => {
                write!(f, "{VERSION} {status:03} {reason}\r\n")?
            }
        }
        write!(f, "{}\r\n", self.headers())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALIVE: &[u8] = b"NOTIFY ALIVE SDDP/1.0\r\n\
From: \"192.168.1.109:1902\"\r\n\
Host: \"widget-70b3d50c1637\"\r\n\
Max-Age: 1800\r\n\
Type: \"acme:widget\"\r\n\
Primary-Proxy: \"avswitch\"\r\n\
Proxies: \"avswitch\"\r\n\
Manufacturer: \"Acme\"\r\n\
Model: \"W-100\"\r\n\
Driver: \"acme_widget_ip.drv\"\r\n\
Config-URL: \"http://192.168.1.109\"\r\n\
\r\n";

    #[test]
    fn parses_notify_alive() {
        let msg = Message::parse(ALIVE).unwrap();
        let Message::Notify { kind, headers } = &msg else {
            panic!("expected NOTIFY, got {msg:?}");
        };
        assert_eq!(*kind, NotifyKind::Alive);
        assert_eq!(headers.len(), 10);
        assert_eq!(headers.get("From"), Some("192.168.1.109:1902"));
        assert_eq!(headers.get("max-age"), Some("1800"));
        assert_eq!(headers.get("CONFIG-URL"), Some("http://192.168.1.109"));
        let names: Vec<_> = headers.iter().map(|h| h.name()).collect();
        assert_eq!(names[0], "From");
        assert_eq!(names[9], "Config-URL");
    }

    #[test]
    fn notify_round_trips_byte_for_byte() {
        let msg = Message::parse(ALIVE).unwrap();
        assert_eq!(msg.to_bytes(), ALIVE);
    }

    #[test]
    fn parses_notify_offline_and_other() {
        let msg: Message = "NOTIFY OFFLINE SDDP/1.0\r\nHost: \"a\"\r\n\r\n"
            .parse()
            .unwrap();
        assert!(matches!(
            msg,
            Message::Notify {
                kind: NotifyKind::Offline,
                ..
            }
        ));
        let msg: Message = "notify alive sddp/1.0\r\n\r\n".parse().unwrap();
        assert!(matches!(
            msg,
            Message::Notify {
                kind: NotifyKind::Alive,
                ..
            }
        ));
        let msg: Message = "NOTIFY BYEBYE SDDP/1.0\r\n\r\n".parse().unwrap();
        let Message::Notify { kind, .. } = &msg else {
            panic!()
        };
        assert_eq!(*kind, NotifyKind::Other("BYEBYE".into()));
        assert_eq!(msg.to_string(), "NOTIFY BYEBYE SDDP/1.0\r\n\r\n");
    }

    #[test]
    fn parses_search() {
        let msg: Message =
            "SEARCH * SDDP/1.0\r\nFrom: \"10.0.0.2:1902\"\r\nTran: 17\r\nTimeout: 3\r\n\r\n"
                .parse()
                .unwrap();
        let Message::Search { target, headers } = &msg else {
            panic!()
        };
        assert_eq!(target, "*");
        assert_eq!(headers.get("tran"), Some("17"));
        assert!(!headers.get_header("Tran").unwrap().is_quoted());
        assert_eq!(headers.get("Timeout"), Some("3"));
    }

    #[test]
    fn search_target_may_contain_spaces() {
        let msg: Message = "SEARCH Example Corp:receiver:big SDDP/1.0\r\n\r\n"
            .parse()
            .unwrap();
        let Message::Search { target, .. } = &msg else {
            panic!()
        };
        assert_eq!(target, "Example Corp:receiver:big");
        assert_eq!(
            msg.to_string(),
            "SEARCH Example Corp:receiver:big SDDP/1.0\r\n\r\n"
        );
    }

    #[test]
    fn parses_response() {
        let msg: Message = "SDDP/1.0 200 OK\r\nTran: 1\r\nHost: \"r\"\r\n"
            .parse()
            .unwrap();
        let Message::Response {
            status,
            reason,
            headers,
        } = &msg
        else {
            panic!()
        };
        assert_eq!((*status, reason.as_str()), (200, "OK"));
        assert_eq!(headers.get("host"), Some("r"));

        let msg: Message = "SDDP/1.0 404 Not  Found\r\n\r\n".parse().unwrap();
        let Message::Response { status, reason, .. } = &msg else {
            panic!()
        };
        assert_eq!((*status, reason.as_str()), (404, "Not  Found"));

        let msg: Message = "SDDP/1.0 200\r\n\r\n".parse().unwrap();
        let Message::Response { reason, .. } = &msg else {
            panic!()
        };
        assert_eq!(reason, "");
    }

    #[test]
    fn tolerates_real_world_sloppiness() {
        // Bare LF, a blank line in the middle of the headers, no space after
        // the colon, trailing whitespace, no final blank line, NUL padding.
        let raw = b"SDDP/1.0 200 OK\nTran: \"1\"\r\nTimeout: 3\r\n\r\nHost: \"rx-1\"\r\n\
Type: \"example:receiver\"\r\nMax-Age:1800 \r\n\0\0";
        let msg = Message::parse(raw).unwrap();
        let h = msg.headers();
        assert_eq!(h.len(), 5);
        assert_eq!(h.get("Host"), Some("rx-1"));
        assert_eq!(h.get("Max-Age"), Some("1800"));
        assert!(h.get_header("Tran").unwrap().is_quoted());
    }

    #[test]
    fn leading_blank_lines_are_skipped() {
        let msg: Message = "\r\n\r\nSEARCH * SDDP/1.0\r\n\r\n".parse().unwrap();
        assert_eq!(msg, Message::search("*"));
    }

    #[test]
    fn non_utf8_is_replaced_not_rejected() {
        let msg = Message::parse(b"NOTIFY ALIVE SDDP/1.0\r\nModel: \"W\xff1\"\r\n\r\n").unwrap();
        assert_eq!(msg.headers().get("model"), Some("W\u{fffd}1"));
    }

    #[test]
    fn rejects_malformed_input() {
        // Multi-byte characters where the version prefix would be must not panic.
        assert!(Message::parse("é€SDDP/1.0 200".as_bytes()).is_err());
        assert!(matches!(
            Message::parse(b"\xff\xfe"),
            Err(ParseError::NotSddp(_))
        ));
        assert!(Message::parse("NOTIFY x 日本語/1.0".as_bytes()).is_err());
        assert_eq!(Message::parse(b""), Err(ParseError::Empty));
        assert_eq!(Message::parse(b"\r\n \r\n"), Err(ParseError::Empty));
        assert_eq!(Message::parse(b"\0\0\0"), Err(ParseError::Empty));
        assert!(matches!(
            Message::parse(b"GET / HTTP/1.1\r\n\r\n"),
            Err(ParseError::NotSddp(_))
        ));
        assert!(matches!(
            Message::parse(b"M-SEARCH * HTTP/1.1\r\n\r\n"),
            Err(ParseError::NotSddp(_))
        ));
        assert!(matches!(
            Message::parse(b"SDDP/1.0\r\n"),
            Err(ParseError::BadStatus(_))
        ));
        assert!(matches!(
            Message::parse(b"hello"),
            Err(ParseError::NotSddp(_))
        ));
        assert_eq!(
            Message::parse(b"SUBSCRIBE x SDDP/1.0\r\n"),
            Err(ParseError::UnknownMethod("SUBSCRIBE".into()))
        );
        assert_eq!(
            Message::parse(b"NOTIFY SDDP/1.0\r\n"),
            Err(ParseError::MissingArgument {
                method: "NOTIFY",
                what: "kind"
            })
        );
        assert_eq!(
            Message::parse(b"SEARCH SDDP/1.0\r\n"),
            Err(ParseError::MissingArgument {
                method: "SEARCH",
                what: "target"
            })
        );
        assert_eq!(
            Message::parse(b"SDDP/1.0 2000 OK\r\n"),
            Err(ParseError::BadStatus("2000".into()))
        );
        assert_eq!(
            Message::parse(b"SDDP/1.0 abc OK\r\n"),
            Err(ParseError::BadStatus("abc".into()))
        );
        assert_eq!(
            Message::parse(b"NOTIFY ALIVE SDDP/1.0\r\nHost: \"a\"\r\nthis is not a header\r\n"),
            Err(ParseError::BadHeader {
                line: 3,
                text: "this is not a header".into()
            })
        );
    }

    #[test]
    fn error_messages_explain_the_problem() {
        let err = Message::parse(b"GET / HTTP/1.1").unwrap_err();
        assert!(err.to_string().contains("SEARCH * SDDP/1.0"), "{err}");
        let long = format!("NOTIFY ALIVE SDDP/1.0\r\n{}\r\n", "x".repeat(200));
        let ParseError::BadHeader { text, .. } = Message::parse(long.as_bytes()).unwrap_err()
        else {
            panic!()
        };
        assert_eq!(text.chars().count(), 81);
    }

    #[test]
    fn formats_constructed_messages() {
        let mut search = Message::search("acme:widget");
        let h = search.headers_mut();
        h.set("From", "10.0.0.2:1902");
        h.set("Tran", "9");
        h.set("Timeout", "3");
        assert_eq!(
            search.to_string(),
            "SEARCH acme:widget SDDP/1.0\r\nFrom: \"10.0.0.2:1902\"\r\nTran: 9\r\nTimeout: 3\r\n\r\n"
        );

        let ok = Message::ok([("Host", "w"), ("Max-Age", "1800")].into_iter().collect());
        assert_eq!(
            ok.to_string(),
            "SDDP/1.0 200 OK\r\nHost: \"w\"\r\nMax-Age: 1800\r\n\r\n"
        );

        let bye = Message::notify(NotifyKind::Offline, Headers::new());
        assert_eq!(bye.to_string(), "NOTIFY OFFLINE SDDP/1.0\r\n\r\n");
    }

    #[test]
    fn round_trips_every_variant() {
        let headers: Headers = [
            ("From", "10.0.0.9:1902"),
            ("Tran", "5"),
            ("X-Custom", "keep me"),
        ]
        .into_iter()
        .collect();
        for msg in [
            Message::notify(NotifyKind::Alive, headers.clone()),
            Message::notify(NotifyKind::Offline, headers.clone()),
            Message::notify(NotifyKind::Other("UPDATE".into()), headers.clone()),
            Message::Search {
                target: "*".into(),
                headers: headers.clone(),
            },
            Message::Response {
                status: 200,
                reason: "OK".into(),
                headers: headers.clone(),
            },
            Message::Response {
                status: 503,
                reason: "Busy now".into(),
                headers,
            },
        ] {
            let parsed = Message::parse(&msg.to_bytes()).unwrap();
            assert_eq!(parsed, msg);
            assert_eq!(parsed.to_bytes(), msg.to_bytes());
        }
    }
}
