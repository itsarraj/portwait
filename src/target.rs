//! What you're waiting for.
//!
//! The mode is inferred from the target's shape rather than needing a flag:
//! anything with an `http://`/`https://` scheme is an HTTP health check,
//! everything else is a raw TCP connect. That keeps the common compose case
//! down to `portwait db:5432` while leaving `portwait http://app:8080/healthz`
//! equally short, and there's no ambiguity to resolve — a `host:port` pair is
//! never a valid URL and vice versa.

use std::fmt;

use anyhow::{bail, Result};

/// A single thing to wait for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Wait until a TCP connection to `host:port` succeeds.
    Tcp { host: String, port: u16 },
    /// Wait until a GET to this URL answers with an accepted status.
    Http { url: String },
}

impl Target {
    /// Parses `host:port`, `tcp://host:port`, `[::1]:port`, or an
    /// `http(s)://` URL.
    pub fn parse(input: &str) -> Result<Self> {
        let text = input.trim();
        if text.is_empty() {
            bail!("empty target: expected `host:port` or an http(s):// URL");
        }

        let lower = text.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            // Deliberately not fully validating the URL here: ureq is the
            // thing that has to understand it, and it reports a far better
            // error than a second, redundant parser would.
            return Ok(Target::Http {
                url: text.to_string(),
            });
        }

        let host_port = text.strip_prefix("tcp://").unwrap_or(text);
        let (host, port) = split_host_port(host_port, input)?;
        Ok(Target::Tcp { host, port })
    }

    /// The `host:port` form for a TCP target, ready for `to_socket_addrs`.
    pub fn authority(&self) -> Option<String> {
        match self {
            Target::Tcp { host, port } => Some(if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            }),
            Target::Http { .. } => None,
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Tcp { .. } => write!(f, "tcp {}", self.authority().unwrap_or_default()),
            Target::Http { url } => write!(f, "http {url}"),
        }
    }
}

fn split_host_port(text: &str, whole: &str) -> Result<(String, u16)> {
    // Bracketed IPv6 first (`[::1]:5432`), because a bare rsplit on ':' would
    // otherwise tear a literal IPv6 address in half at its last colon.
    let (host, port_text) = if let Some(rest) = text.strip_prefix('[') {
        let Some(close) = rest.find(']') else {
            bail!("`{whole}` is missing the closing `]` of its IPv6 address");
        };
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let Some(port_text) = after.strip_prefix(':') else {
            bail!("`{whole}` has no port: expected `[{host}]:PORT`");
        };
        (host, port_text)
    } else {
        let Some((host, port_text)) = text.rsplit_once(':') else {
            bail!("`{whole}` has no port: expected `host:port`, e.g. `db:5432`");
        };
        (host, port_text)
    };

    if host.is_empty() {
        bail!("`{whole}` has no host: expected `host:port`, e.g. `db:5432`");
    }
    let port: u16 = port_text
        .parse()
        .map_err(|_| anyhow::anyhow!("`{whole}` has an invalid port `{port_text}` (1-65535)"))?;
    if port == 0 {
        bail!("`{whole}` has an invalid port 0 (1-65535)");
    }

    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(host: &str, port: u16) -> Target {
        Target::Tcp {
            host: host.to_string(),
            port,
        }
    }

    #[test]
    fn a_bare_host_port_is_tcp_mode() {
        assert_eq!(Target::parse("db:5432").unwrap(), tcp("db", 5432));
        assert_eq!(
            Target::parse("127.0.0.1:6379").unwrap(),
            tcp("127.0.0.1", 6379)
        );
    }

    #[test]
    fn an_explicit_tcp_scheme_is_accepted_too() {
        assert_eq!(Target::parse("tcp://db:5432").unwrap(), tcp("db", 5432));
    }

    #[test]
    fn a_bracketed_ipv6_literal_is_not_split_at_its_own_colons() {
        assert_eq!(Target::parse("[::1]:5432").unwrap(), tcp("::1", 5432));
        assert_eq!(
            Target::parse("[fe80::1%eth0]:80").unwrap(),
            tcp("fe80::1%eth0", 80)
        );
        // ...and it round-trips back into bracketed form for the resolver.
        assert_eq!(
            Target::parse("[::1]:5432").unwrap().authority().unwrap(),
            "[::1]:5432"
        );
    }

    #[test]
    fn an_http_url_selects_http_mode_by_its_scheme_alone() {
        let parsed = Target::parse("http://app:8080/healthz").unwrap();
        assert_eq!(
            parsed,
            Target::Http {
                url: "http://app:8080/healthz".to_string()
            }
        );
        assert!(matches!(
            Target::parse("https://api.example.com/health").unwrap(),
            Target::Http { .. }
        ));
        assert!(matches!(
            Target::parse("HTTP://APP:8080/x").unwrap(),
            Target::Http { .. }
        ));
    }

    #[test]
    fn a_url_is_never_mistaken_for_a_host_port_pair() {
        // `app:8080` appears inside this string; the scheme has to win.
        assert!(Target::parse("http://app:8080/healthz")
            .unwrap()
            .authority()
            .is_none());
    }

    #[test]
    fn missing_or_nonsense_ports_are_rejected_with_a_specific_reason() {
        for bad in ["db", "db:", "db:notaport", "db:70000", "db:0", ":5432", ""] {
            assert!(Target::parse(bad).is_err(), "`{bad}` should be rejected");
        }
        let err = Target::parse("db").unwrap_err().to_string();
        assert!(err.contains("has no port"), "got: {err}");
    }

    #[test]
    fn display_says_which_mode_was_inferred() {
        assert_eq!(Target::parse("db:5432").unwrap().to_string(), "tcp db:5432");
        assert_eq!(
            Target::parse("http://a/healthz").unwrap().to_string(),
            "http http://a/healthz"
        );
    }
}
