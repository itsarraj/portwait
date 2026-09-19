//! `portwait` — block until a dependency is actually ready.
//!
//! The library half is deliberately separated from the CLI so the parts that
//! are worth testing without a process boundary (duration parsing, status
//! specs, target parsing, the retry/backoff schedule) can be, while the parts
//! that are only meaningful against a real socket (the TCP and HTTP probes,
//! the exec handoff) are exercised by the integration tests in `tests/`
//! against genuinely bound local ports.

pub mod duration;
pub mod exec;
pub mod probe;
pub mod status;
pub mod target;
pub mod wait;
