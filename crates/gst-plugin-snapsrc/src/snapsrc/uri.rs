//! The `snapcast-tcp://` URI (`docs/dlna.md`, "URI" and "Naming"):
//!
//! ```text
//! snapcast-tcp://<host>[:<port>]/<stream>[?control=<port>]
//! ```
//!
//! - `host`: name or address, IPv6 in brackets; empty (`snapcast-tcp:///radio`):
//!   find a server via mDNS.
//! - `port`: the stream port (TCP, Snapcast's binary protocol), default 1704.
//! - `stream`: the stream to join (percent-encoded); empty: the server's choice.
//! - `control`: the JSON-RPC control port for joining, default the stream port + 1.
//!
//! "tcp" names the transport: a `snapcast-ws` variant (audio over WebSocket) may
//! follow.

/// The URI scheme `snapsrc` handles.
pub const SCHEME: &str = "snapcast-tcp";

/// The parts of a `snapcast-tcp://` URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapUri {
    pub host: String,
    /// `None`: the default port.
    pub port: Option<u16>,
    pub stream: String,
    /// `None`: the stream port + 1.
    pub control_port: Option<u16>,
}

fn parse_port(s: &str, what: &str) -> Result<u16, String> {
    s.parse::<u16>()
        .ok()
        .filter(|&p| p > 0)
        .ok_or_else(|| format!("invalid {what} '{s}'"))
}

fn percent_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| format!("invalid percent-encoding in '{s}'"))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("'{s}' is not UTF-8"))
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

impl SnapUri {
    /// Parses a `snapcast-tcp://` URI.
    pub fn parse(uri: &str) -> Result<Self, String> {
        let rest = uri
            .split_once("://")
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case(SCHEME))
            .map(|(_, rest)| rest)
            .ok_or_else(|| format!("not a {SCHEME}:// URI: '{uri}'"))?;
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));

        let (host, port_str) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6
                .split_once(']')
                .ok_or_else(|| format!("unclosed '[' in '{uri}'"))?;
            let port = match after {
                "" => None,
                p => Some(
                    p.strip_prefix(':')
                        .ok_or_else(|| format!("bad host in '{uri}'"))?,
                ),
            };
            (host, port)
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            }
        };
        let port = port_str.map(|p| parse_port(p, "port")).transpose()?;

        let mut control_port = None;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            // Unknown parameters are ignored, for later additions.
            if let Some(("control", p)) = pair.split_once('=') {
                control_port = Some(parse_port(p, "control port")?);
            }
        }

        Ok(Self {
            host: host.to_owned(),
            port,
            stream: percent_decode(path.trim_end_matches('/'))?,
            control_port,
        })
    }
}

impl std::fmt::Display for SnapUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{SCHEME}://")?;
        if self.host.contains(':') {
            write!(f, "[{}]", self.host)?;
        } else {
            f.write_str(&self.host)?;
        }
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        write!(f, "/{}", percent_encode(&self.stream))?;
        if let Some(control) = self.control_port {
            write!(f, "?control={control}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_full_form() {
        let u = SnapUri::parse("snapcast-tcp://192.168.1.2:1800/dms-3f9a?control=1801").unwrap();
        assert_eq!(
            u,
            SnapUri {
                host: "192.168.1.2".into(),
                port: Some(1800),
                stream: "dms-3f9a".into(),
                control_port: Some(1801),
            }
        );
        assert_eq!(
            u.to_string(),
            "snapcast-tcp://192.168.1.2:1800/dms-3f9a?control=1801"
        );
    }

    #[test]
    fn defaults_ipv6_and_encoding() {
        let u = SnapUri::parse("SNAPCAST-TCP://nas/").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.stream.as_str()),
            ("nas", None, "")
        );
        assert_eq!(u.control_port, None);

        let u = SnapUri::parse("snapcast-tcp://[fe80::1]:1704/My%20Radio").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.stream.as_str()),
            ("fe80::1", Some(1704), "My Radio")
        );
        assert_eq!(u.to_string(), "snapcast-tcp://[fe80::1]:1704/My%20Radio");

        // Empty host: mDNS.
        assert_eq!(SnapUri::parse("snapcast-tcp:///radio").unwrap().host, "");
        // Unknown parameters are ignored.
        assert_eq!(
            SnapUri::parse("snapcast-tcp://h/s?x=1&control=9")
                .unwrap()
                .control_port,
            Some(9)
        );
    }

    #[test]
    fn rejects_bad_uris() {
        for bad in [
            "snapcast://h/s",
            "http://h/s",
            "snapcast-tcp://h:0/s",
            "snapcast-tcp://h:x/s",
            "snapcast-tcp://[::1/s",
            "snapcast-tcp://h/s?control=70000",
            "snapcast-tcp://h/%zz",
        ] {
            assert!(SnapUri::parse(bad).is_err(), "{bad}");
        }
    }
}
