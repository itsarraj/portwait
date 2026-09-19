//! The two readiness checks.
//!
//! Both are deliberately one-shot and stateless: a probe answers "is it ready
//! *right now*", and [`crate::wait`] owns everything about when to ask again.
//! That split is what lets the retry/backoff schedule be unit-tested against a
//! fake prober while the real ones are tested against real sockets.

use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::status::StatusSpec;
use crate::target::Target;

/// The answer to a single readiness check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probed {
    Ready,
    /// Not ready, with the reason to quote if we eventually time out.
    NotReady(String),
}

/// One readiness check, asked repeatedly.
pub trait Prober {
    /// `budget` is how much of the overall `--timeout` is left; a probe must
    /// not block past it, so a slow connect can't overshoot the deadline the
    /// user asked for.
    fn probe(&self, budget: Duration) -> Probed;
}

/// Builds the right prober for a target. `expect` is ignored for TCP targets
/// (there's no status code to check when all you did was open a socket) — a
/// fact the CLI points out rather than silently swallowing.
pub fn prober_for(
    target: &Target,
    expect: StatusSpec,
    connect_timeout: Duration,
) -> Box<dyn Prober> {
    match target {
        Target::Tcp { .. } => Box::new(TcpProber {
            authority: target.authority().unwrap_or_default(),
            connect_timeout,
        }),
        Target::Http { url } => Box::new(HttpProber {
            url: url.clone(),
            expect,
            connect_timeout,
        }),
    }
}

/// "Can I open a TCP connection to this?" — the `wait-for-it.sh` check.
pub struct TcpProber {
    authority: String,
    connect_timeout: Duration,
}

impl Prober for TcpProber {
    fn probe(&self, budget: Duration) -> Probed {
        // Never block longer than what's left of the overall timeout, and
        // never pass a zero timeout to `connect_timeout` (which rejects it
        // outright as an invalid argument rather than meaning "instant").
        let per_attempt = self
            .connect_timeout
            .min(budget)
            .max(Duration::from_millis(1));

        // Resolved on every attempt on purpose. In a compose network the
        // dependency's DNS name frequently doesn't resolve at all for the
        // first second or two, and that is a "not ready yet" condition, not a
        // fatal error — resolving once up front would make `portwait` fail
        // instantly in exactly the situation it exists for.
        let addrs = match self.authority.to_socket_addrs() {
            Ok(addrs) => addrs.collect::<Vec<_>>(),
            Err(e) => return Probed::NotReady(format!("cannot resolve {}: {e}", self.authority)),
        };
        if addrs.is_empty() {
            return Probed::NotReady(format!("{} resolved to no addresses", self.authority));
        }

        let mut last = String::new();
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, per_attempt) {
                Ok(stream) => {
                    // Close it cleanly. We only ever wanted to know the
                    // handshake completes; leaving a half-open connection
                    // behind would show up in the dependency's logs as a
                    // spurious client that connected and said nothing.
                    let _ = stream.shutdown(Shutdown::Both);
                    return Probed::Ready;
                }
                Err(e) => last = format!("{addr}: {e}"),
            }
        }
        Probed::NotReady(last)
    }
}

/// "Does this URL answer with a status I accept?" — the check that a socket
/// being open cannot make for you.
pub struct HttpProber {
    url: String,
    expect: StatusSpec,
    connect_timeout: Duration,
}

impl Prober for HttpProber {
    fn probe(&self, budget: Duration) -> Probed {
        let per_attempt = self
            .connect_timeout
            .min(budget)
            .max(Duration::from_millis(1));

        // A fresh agent per attempt: no connection pooling across polls. The
        // thing being polled is, by definition, in the middle of starting up
        // and may well close or restart between probes, and a pooled socket
        // to a process that has since restarted is a confusing failure to
        // attribute. `redirects(0)` matters too — following redirects would
        // turn a 302 into whatever it points at, which defeats the point of
        // letting the user say `--expect-status 302`.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(per_attempt)
            .timeout(per_attempt)
            .redirects(0)
            .user_agent(concat!("portwait/", env!("CARGO_PKG_VERSION")))
            .build();

        match agent.get(&self.url).call() {
            Ok(resp) => self.judge(resp.status()),
            // ureq reports any status >= 400 as an error. For a health check
            // that is emphatically not an error — a 503 from a still-warming
            // app is the *normal* case here, and the whole reason this mode
            // exists — so it gets judged against the spec like any other.
            Err(ureq::Error::Status(code, _)) => self.judge(code),
            Err(ureq::Error::Transport(t)) => Probed::NotReady(t.to_string()),
        }
    }
}

impl HttpProber {
    fn judge(&self, code: u16) -> Probed {
        if self.expect.matches(code) {
            Probed::Ready
        } else {
            Probed::NotReady(format!("got status {code}, want {}", self.expect))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn tcp_probe_is_ready_against_a_real_bound_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let target = Target::parse(&addr.to_string()).unwrap();
        let prober = prober_for(
            &target,
            StatusSpec::parse("200").unwrap(),
            Duration::from_secs(1),
        );
        assert_eq!(prober.probe(Duration::from_secs(1)), Probed::Ready);
    }

    #[test]
    fn tcp_probe_is_not_ready_against_a_refused_port() {
        // Port 1 needs root to bind, so nothing in this sandbox (or any
        // parallel agent on this machine) can be listening on it.
        let target = Target::parse("127.0.0.1:1").unwrap();
        let prober = prober_for(
            &target,
            StatusSpec::parse("200").unwrap(),
            Duration::from_secs(1),
        );
        match prober.probe(Duration::from_secs(1)) {
            Probed::NotReady(reason) => assert!(!reason.is_empty(), "reason must be quotable"),
            Probed::Ready => panic!("nothing should be listening on 127.0.0.1:1"),
        }
    }

    #[test]
    fn an_unresolvable_host_is_not_ready_rather_than_a_hard_error() {
        let target = Target::parse("portwait.invalid.:5432").unwrap();
        let prober = prober_for(
            &target,
            StatusSpec::parse("200").unwrap(),
            Duration::from_millis(200),
        );
        match prober.probe(Duration::from_millis(200)) {
            Probed::NotReady(reason) => assert!(
                reason.contains("portwait.invalid"),
                "reason should name the host, got: {reason}"
            ),
            Probed::Ready => panic!("an unresolvable host is never ready"),
        }
    }
}
