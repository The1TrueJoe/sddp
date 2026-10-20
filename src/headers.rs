//! Ordered, case-insensitive SDDP headers.
//!
//! SDDP headers look like HTTP headers, but most values are wrapped in double
//! quotes (`Host: "widget-01"`) while numbers usually are not (`Max-Age: 1800`).
//! [`Headers`] hides the quotes when you read a value and remembers whether a
//! value was quoted so a parsed message is written back the way it arrived.

use std::fmt;

/// One header line: a name, its value without surrounding quotes, and whether
/// the value is written in quotes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    name: String,
    value: String,
    quoted: bool,
}

impl Header {
    /// The header name, exactly as received or inserted (case preserved).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The value with any surrounding double quotes removed.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Whether the value is written wrapped in double quotes.
    pub fn is_quoted(&self) -> bool {
        self.quoted
    }

    /// Parses one `Name: value` line. Returns `None` when there is no colon or
    /// the name is empty.
    pub(crate) fn parse_line(line: &str) -> Option<Header> {
        let (name, value) = line.split_once(':')?;
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        // Devices are sloppy about whitespace (`Max-Age:1800 ` has been seen).
        let value = value.trim();
        let (value, quoted) = match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            Some(inner) => (inner, true),
            None => (value, false),
        };
        Some(Header {
            name: name.to_owned(),
            value: value.to_owned(),
            quoted,
        })
    }
}

impl fmt::Display for Header {
    /// Writes `Name: value` (no line ending). CR and LF inside the value are
    /// replaced with spaces so a value can never break the message framing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.value.replace(['\r', '\n'], " ");
        if self.quoted {
            write!(f, "{}: \"{}\"", self.name, value)
        } else {
            write!(f, "{}: {}", self.name, value)
        }
    }
}

/// The default quoting rule used by [`Headers::set`] and [`Headers::append`]:
/// plain non-negative integers are written bare (`Max-Age: 1800`), everything
/// else is quoted (`Type: "acme:widget"`). This matches what devices send.
pub fn should_quote(value: &str) -> bool {
    value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit())
}

fn check_name(name: &str) {
    assert!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_graphic() && b != b':' && b != b'"'),
        "invalid SDDP header name {name:?}: names must be non-empty printable ASCII without ':' or spaces"
    );
}

/// An ordered list of headers with case-insensitive lookup.
///
/// Order is preserved, duplicate names are allowed (lookups return the first),
/// and unknown headers are kept as-is.
///
/// ```
/// use sddp::Headers;
///
/// let mut h = Headers::new();
/// h.set("Type", "acme:widget");
/// h.set("Max-Age", "1800");
/// assert_eq!(h.get("type"), Some("acme:widget"));
/// assert_eq!(h.to_string(), "Type: \"acme:widget\"\r\nMax-Age: 1800\r\n");
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<Header>,
}

impl Headers {
    /// An empty header list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of header lines.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no headers.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The first value for `name` (case-insensitive), without quotes.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.get_header(name).map(Header::value)
    }

    /// The first header line named `name` (case-insensitive).
    pub fn get_header(&self, name: &str) -> Option<&Header> {
        self.entries
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
    }

    /// Every value for `name` (case-insensitive), in order.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.entries
            .iter()
            .filter(move |h| h.name.eq_ignore_ascii_case(name))
            .map(Header::value)
    }

    /// Whether a header named `name` (case-insensitive) is present.
    pub fn contains(&self, name: &str) -> bool {
        self.get_header(name).is_some()
    }

    /// Sets `name` to `value`, quoting it according to [`should_quote`].
    ///
    /// If the header already exists its first occurrence is replaced in place
    /// (keeping its position and the spelling of its name) and any further
    /// occurrences are removed; otherwise it is appended.
    ///
    /// # Panics
    ///
    /// If `name` is empty or contains whitespace, a colon or a double quote.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let value = value.into();
        let quoted = should_quote(&value);
        self.set_quoted(name, value, quoted);
    }

    /// Like [`set`](Self::set), but you choose whether the value is quoted.
    ///
    /// # Panics
    ///
    /// If `name` is empty or contains whitespace, a colon or a double quote.
    pub fn set_quoted(&mut self, name: impl Into<String>, value: impl Into<String>, quoted: bool) {
        let name = name.into();
        check_name(&name);
        let value = value.into();
        let mut matches = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, h)| h.name.eq_ignore_ascii_case(&name))
            .map(|(i, _)| i);
        match matches.next() {
            Some(first) => {
                let entry = &mut self.entries[first];
                entry.value = value;
                entry.quoted = quoted;
                let mut i = 0;
                self.entries.retain(|h| {
                    let keep = i <= first || !h.name.eq_ignore_ascii_case(&name);
                    i += 1;
                    keep
                });
            }
            None => self.entries.push(Header {
                name,
                value,
                quoted,
            }),
        }
    }

    /// Appends a header line, even if one with the same name exists, quoting
    /// the value according to [`should_quote`].
    ///
    /// # Panics
    ///
    /// If `name` is empty or contains whitespace, a colon or a double quote.
    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let value = value.into();
        let quoted = should_quote(&value);
        self.append_quoted(name, value, quoted);
    }

    /// Like [`append`](Self::append), but you choose whether the value is quoted.
    ///
    /// # Panics
    ///
    /// If `name` is empty or contains whitespace, a colon or a double quote.
    pub fn append_quoted(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
        quoted: bool,
    ) {
        let name = name.into();
        check_name(&name);
        self.entries.push(Header {
            name,
            value: value.into(),
            quoted,
        });
    }

    /// Removes every header named `name` (case-insensitive) and returns the
    /// first removed value.
    pub fn remove(&mut self, name: &str) -> Option<String> {
        let mut first = None;
        self.entries.retain_mut(|h| {
            if h.name.eq_ignore_ascii_case(name) {
                if first.is_none() {
                    first = Some(std::mem::take(&mut h.value));
                }
                false
            } else {
                true
            }
        });
        first
    }

    /// The header lines in order.
    pub fn iter(&self) -> std::slice::Iter<'_, Header> {
        self.entries.iter()
    }

    pub(crate) fn push(&mut self, header: Header) {
        self.entries.push(header);
    }
}

impl fmt::Display for Headers {
    /// Each header as `Name: value\r\n`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for h in &self.entries {
            write!(f, "{h}\r\n")?;
        }
        Ok(())
    }
}

impl<'a> IntoIterator for &'a Headers {
    type Item = &'a Header;
    type IntoIter = std::slice::Iter<'a, Header>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl<K: Into<String>, V: Into<String>> Extend<(K, V)> for Headers {
    /// Appends each pair with [`append`](Headers::append).
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.append(k, v);
        }
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Headers {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut h = Headers::new();
        h.extend(iter);
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_strips_quotes_and_whitespace() {
        let h = Header::parse_line("Host: \"widget-01\"").unwrap();
        assert_eq!(
            (h.name(), h.value(), h.is_quoted()),
            ("Host", "widget-01", true)
        );

        let h = Header::parse_line("Max-Age:1800 ").unwrap();
        assert_eq!(
            (h.name(), h.value(), h.is_quoted()),
            ("Max-Age", "1800", false)
        );

        let h = Header::parse_line("  Tran :  \"7\"\t").unwrap();
        assert_eq!((h.name(), h.value(), h.is_quoted()), ("Tran", "7", true));
    }

    #[test]
    fn parse_line_keeps_colons_in_values() {
        let h = Header::parse_line("From: \"192.168.1.20:1902\"").unwrap();
        assert_eq!(h.value(), "192.168.1.20:1902");
        let h = Header::parse_line("Config-URL: http://10.0.0.5:8080/x").unwrap();
        assert_eq!(h.value(), "http://10.0.0.5:8080/x");
        assert!(!h.is_quoted());
    }

    #[test]
    fn parse_line_edge_cases() {
        assert!(Header::parse_line("no colon here").is_none());
        assert!(Header::parse_line(": value").is_none());
        // A lone quote is not a quoted value.
        let h = Header::parse_line("X: \"").unwrap();
        assert_eq!((h.value(), h.is_quoted()), ("\"", false));
        // Empty quoted value.
        let h = Header::parse_line("X: \"\"").unwrap();
        assert_eq!((h.value(), h.is_quoted()), ("", true));
        // Empty bare value.
        let h = Header::parse_line("X:").unwrap();
        assert_eq!((h.value(), h.is_quoted()), ("", false));
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let mut h = Headers::new();
        h.set("Primary-Proxy", "receiver");
        assert_eq!(h.get("primary-proxy"), Some("receiver"));
        assert_eq!(h.get("PRIMARY-PROXY"), Some("receiver"));
        assert!(h.contains("pRiMaRy-PrOxY"));
        assert_eq!(h.get("Proxies"), None);
    }

    #[test]
    fn set_replaces_in_place_and_drops_duplicates() {
        let mut h = Headers::new();
        h.append("A", "1");
        h.append("Tran", "x");
        h.append("B", "2");
        h.append("tran", "y");
        h.set("TRAN", "z");
        let names: Vec<_> = h.iter().map(Header::name).collect();
        assert_eq!(names, ["A", "Tran", "B"]);
        assert_eq!(h.get("tran"), Some("z"));
    }

    #[test]
    fn append_keeps_duplicates_and_order() {
        let mut h = Headers::new();
        h.append("X-Thing", "one");
        h.append("Other", "o");
        h.append("x-thing", "two");
        assert_eq!(h.get("X-THING"), Some("one"));
        assert_eq!(h.get_all("x-thing").collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn remove_removes_all_and_returns_first() {
        let mut h: Headers = [("A", "1"), ("a", "2"), ("B", "3")].into_iter().collect();
        assert_eq!(h.remove("A").as_deref(), Some("1"));
        assert_eq!(h.len(), 1);
        assert_eq!(h.remove("A"), None);
    }

    #[test]
    fn automatic_quoting() {
        assert!(!should_quote("1800"));
        assert!(!should_quote("0"));
        assert!(should_quote(""));
        assert!(should_quote("-1"));
        assert!(should_quote("acme:widget"));
        assert!(should_quote("1.5"));

        let mut h = Headers::new();
        h.set("Max-Age", "1800");
        h.set("Type", "acme:widget");
        h.set("Empty", "");
        h.set_quoted("Tran", "42", true);
        h.append_quoted("Raw", "a b", false);
        assert_eq!(
            h.to_string(),
            "Max-Age: 1800\r\nType: \"acme:widget\"\r\nEmpty: \"\"\r\nTran: \"42\"\r\nRaw: a b\r\n"
        );
    }

    #[test]
    fn formatting_neutralises_line_breaks() {
        let mut h = Headers::new();
        h.set("Model", "W-100\r\nInjected: yes");
        assert_eq!(h.to_string(), "Model: \"W-100  Injected: yes\"\r\n");
    }

    #[test]
    #[should_panic(expected = "invalid SDDP header name")]
    fn rejects_name_with_colon() {
        Headers::new().set("Bad:Name", "x");
    }

    #[test]
    #[should_panic(expected = "invalid SDDP header name")]
    fn rejects_name_with_space() {
        Headers::new().append("Bad Name", "x");
    }

    #[test]
    #[should_panic(expected = "invalid SDDP header name")]
    fn rejects_empty_name() {
        Headers::new().set("", "x");
    }
}
