//! The retry loop: ask, sleep, ask again, give up at the deadline.

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::probe::{Probed, Prober};

/// How long to wait and how often to re-ask.
#[derive(Debug, Clone)]
pub struct WaitConfig {
    /// Total budget before giving up.
    pub timeout: Duration,
    /// Gap between the first two attempts.
    pub interval: Duration,
    /// Multiplier applied to the gap after each failed attempt. `1.0` means a
    /// flat poll, which is the default: for a local dependency that comes up
    /// in a second, backing off actively *hurts* — it can leave you sleeping
    /// through the moment it became ready.
    pub backoff: f64,
    /// Ceiling for the backed-off gap, so a long `--timeout` doesn't turn
    /// into one attempt every two minutes.
    pub max_interval: Duration,
}

impl Default for WaitConfig {
    fn default() -> Self {
        WaitConfig {
            timeout: Duration::from_secs(30),
            interval: Duration::from_millis(250),
            backoff: 1.0,
            max_interval: Duration::from_secs(5),
        }
    }
}

/// It came up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub elapsed: Duration,
    pub attempts: u32,
}

/// It didn't, and here's the last reason why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedOut {
    pub elapsed: Duration,
    pub attempts: u32,
    pub last_error: String,
}

/// The gap to use after a failed attempt, given the gap that was just used.
/// Split out as a pure function because "does the backoff actually back off,
/// and does it actually stop at the ceiling" is worth testing without burning
/// the wall-clock time it would take to observe it in a real loop.
pub fn next_interval(current: Duration, cfg: &WaitConfig) -> Duration {
    if cfg.backoff <= 1.0 {
        return current.min(cfg.max_interval);
    }
    let grown = current.as_secs_f64() * cfg.backoff;
    Duration::from_secs_f64(grown).min(cfg.max_interval)
}

/// Polls `prober` until it reports ready or `cfg.timeout` elapses.
///
/// `on_attempt` is called with the attempt number and the failure reason after
/// every unsuccessful probe, which is how the CLI prints progress without this
/// module knowing anything about terminals.
pub fn wait_for(
    prober: &dyn Prober,
    cfg: &WaitConfig,
    mut on_attempt: impl FnMut(u32, &str),
) -> Result<Ready, TimedOut> {
    let start = Instant::now();
    let mut attempts: u32 = 0;
    let mut interval = cfg.interval;
    // Definitely assigned before it can ever be read: the only path to the
    // timeout return runs through the `NotReady` arm below.
    let mut last_error: String;

    loop {
        // Always make at least one real attempt, even with `--timeout 0`:
        // "check once, don't wait" is a legitimate use (a CI step asserting
        // something is *already* up), and it should do a genuine check rather
        // than fail on a technicality before touching the network.
        let remaining = cfg.timeout.saturating_sub(start.elapsed());
        let budget = if remaining.is_zero() {
            Duration::from_millis(1)
        } else {
            remaining
        };

        attempts += 1;
        match prober.probe(budget) {
            Probed::Ready => {
                return Ok(Ready {
                    elapsed: start.elapsed(),
                    attempts,
                })
            }
            Probed::NotReady(reason) => {
                on_attempt(attempts, &reason);
                last_error = reason;
            }
        }

        let elapsed = start.elapsed();
        if elapsed >= cfg.timeout {
            return Err(TimedOut {
                elapsed,
                attempts,
                last_error,
            });
        }

        // Don't sleep past the deadline: without this clamp a `--timeout 5s`
        // with a `--max-interval 30s` would return at 30s, which is the kind
        // of thing that quietly blows a CI job's own time limit.
        sleep(interval.min(cfg.timeout - elapsed));
        interval = next_interval(interval, cfg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A prober that reports not-ready a fixed number of times, then ready.
    struct ReadyAfter {
        failures: u32,
        seen: Cell<u32>,
    }

    impl Prober for ReadyAfter {
        fn probe(&self, _budget: Duration) -> Probed {
            let n = self.seen.get();
            self.seen.set(n + 1);
            if n < self.failures {
                Probed::NotReady(format!("not yet (attempt {})", n + 1))
            } else {
                Probed::Ready
            }
        }
    }

    /// A prober that is never ready, and records the budget it was handed.
    struct NeverReady {
        smallest_budget: Cell<Duration>,
    }

    impl Prober for NeverReady {
        fn probe(&self, budget: Duration) -> Probed {
            if budget < self.smallest_budget.get() {
                self.smallest_budget.set(budget);
            }
            Probed::NotReady("nope".to_string())
        }
    }

    fn fast_config(timeout: Duration) -> WaitConfig {
        WaitConfig {
            timeout,
            interval: Duration::from_millis(10),
            backoff: 1.0,
            max_interval: Duration::from_millis(10),
        }
    }

    #[test]
    fn returns_on_the_first_attempt_when_already_ready() {
        let prober = ReadyAfter {
            failures: 0,
            seen: Cell::new(0),
        };
        let ready = wait_for(&prober, &fast_config(Duration::from_secs(5)), |_, _| {}).unwrap();
        assert_eq!(ready.attempts, 1);
    }

    #[test]
    fn keeps_retrying_until_ready_and_counts_every_attempt() {
        let prober = ReadyAfter {
            failures: 4,
            seen: Cell::new(0),
        };
        let mut reported = Vec::new();
        let ready = wait_for(&prober, &fast_config(Duration::from_secs(5)), |n, why| {
            reported.push((n, why.to_string()))
        })
        .unwrap();
        assert_eq!(ready.attempts, 5);
        assert_eq!(reported.len(), 4, "every failed attempt is reported once");
        assert_eq!(reported[0].0, 1);
        assert!(reported[3].1.contains("attempt 4"));
    }

    #[test]
    fn times_out_carrying_the_last_real_reason() {
        let prober = NeverReady {
            smallest_budget: Cell::new(Duration::MAX),
        };
        let err =
            wait_for(&prober, &fast_config(Duration::from_millis(120)), |_, _| {}).unwrap_err();
        assert_eq!(err.last_error, "nope");
        assert!(
            err.attempts >= 2,
            "should have retried, got {}",
            err.attempts
        );
        assert!(
            err.elapsed >= Duration::from_millis(120),
            "must not give up early, gave up at {:?}",
            err.elapsed
        );
        assert!(
            err.elapsed < Duration::from_millis(600),
            "must not overrun the timeout, ran {:?}",
            err.elapsed
        );
    }

    #[test]
    fn a_zero_timeout_still_makes_one_real_attempt() {
        let prober = NeverReady {
            smallest_budget: Cell::new(Duration::MAX),
        };
        let err = wait_for(&prober, &fast_config(Duration::ZERO), |_, _| {}).unwrap_err();
        assert_eq!(err.attempts, 1);
        assert_eq!(err.last_error, "nope");
    }

    #[test]
    fn the_budget_handed_to_a_probe_shrinks_toward_the_deadline() {
        let prober = NeverReady {
            smallest_budget: Cell::new(Duration::MAX),
        };
        let _ = wait_for(&prober, &fast_config(Duration::from_millis(100)), |_, _| {});
        let smallest = prober.smallest_budget.get();
        assert!(
            smallest < Duration::from_millis(30),
            "the last probes must be handed only what's left of the timeout, got {smallest:?}"
        );
    }

    #[test]
    fn backoff_of_one_keeps_a_flat_interval() {
        let cfg = WaitConfig {
            backoff: 1.0,
            max_interval: Duration::from_secs(5),
            ..WaitConfig::default()
        };
        let mut interval = Duration::from_millis(250);
        for _ in 0..10 {
            interval = next_interval(interval, &cfg);
        }
        assert_eq!(interval, Duration::from_millis(250));
    }

    #[test]
    fn backoff_grows_the_interval_and_stops_at_max_interval() {
        let cfg = WaitConfig {
            backoff: 2.0,
            max_interval: Duration::from_secs(2),
            ..WaitConfig::default()
        };
        let a = next_interval(Duration::from_millis(250), &cfg);
        assert_eq!(a, Duration::from_millis(500));
        let b = next_interval(a, &cfg);
        assert_eq!(b, Duration::from_secs(1));
        let c = next_interval(b, &cfg);
        assert_eq!(c, Duration::from_secs(2));
        // ...and then it stays there rather than growing without bound.
        assert_eq!(next_interval(c, &cfg), Duration::from_secs(2));
    }

    #[test]
    fn an_interval_larger_than_max_interval_is_still_capped() {
        let cfg = WaitConfig {
            backoff: 1.0,
            max_interval: Duration::from_millis(100),
            ..WaitConfig::default()
        };
        assert_eq!(
            next_interval(Duration::from_secs(10), &cfg),
            Duration::from_millis(100)
        );
    }
}
