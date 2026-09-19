use std::process::exit;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;

use portwait::duration::{format_duration, parse_duration};
use portwait::exec::run_following_command;
use portwait::probe::prober_for;
use portwait::status::StatusSpec;
use portwait::target::Target;
use portwait::wait::{wait_for, WaitConfig};

/// Exit code used when the wait ran out of time. Distinct from clap's `2`
/// (usage error) so a script can tell "you typed the flag wrong" apart from
/// "the database genuinely never came up".
const EXIT_TIMEOUT: i32 = 1;

#[derive(Parser)]
#[command(
    name = "portwait",
    version,
    about = "Block until a TCP port or an HTTP health check is actually ready, then optionally exec a command",
    after_help = "\
EXAMPLES:
  portwait db:5432                                 wait up to 30s for a TCP connect
  portwait http://app:8080/healthz --expect-status 200
  portwait db:5432 redis:6379 -- npm start         wait for both, then exec
  portwait db:5432 -t 90s -i 500ms --backoff 1.5   slower poll, backing off

Targets are TCP unless they carry an http:// or https:// scheme.
Exit codes: 0 ready (or the exec'd command's own code), 1 timed out, 2 bad usage."
)]
struct Cli {
    /// What to wait for: `host:port`, `tcp://host:port`, `[::1]:port`, or an
    /// http(s):// URL. Repeat to wait for several — they share one overall
    /// timeout and are checked in the order given.
    #[arg(required = true, value_name = "TARGET")]
    targets: Vec<String>,

    /// Total time to wait before giving up, across all targets (30s, 250ms, 2m, 1h; a bare number means seconds)
    #[arg(short = 't', long, default_value = "30s", value_name = "DURATION")]
    timeout: String,

    /// Gap between attempts
    #[arg(short = 'i', long, default_value = "250ms", value_name = "DURATION")]
    interval: String,

    /// Multiply the gap by this after each failed attempt (1.0 = flat polling)
    #[arg(long, default_value = "1.0", value_name = "FACTOR")]
    backoff: f64,

    /// Ceiling for the backed-off gap
    #[arg(long, default_value = "5s", value_name = "DURATION")]
    max_interval: String,

    /// Per-attempt connect/read timeout, capped by whatever is left of --timeout
    #[arg(long, default_value = "5s", value_name = "DURATION")]
    connect_timeout: String,

    /// HTTP mode only: which response statuses count as ready (200, 2xx, 200-399, 200,204)
    #[arg(long, default_value = "200-399", value_name = "SPEC")]
    expect_status: String,

    /// Only report failures; say nothing while waiting or on success
    #[arg(short, long)]
    quiet: bool,

    /// Command to exec once every target is ready, e.g. `-- npm start`
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<String>,
}

fn main() {
    match run() {
        Ok(code) => exit(code),
        Err(e) => {
            eprintln!("portwait: {e:#}");
            exit(EXIT_TIMEOUT);
        }
    }
}

fn run() -> Result<i32> {
    let cli = Cli::parse();

    let timeout = parse_duration(&cli.timeout).context("--timeout")?;
    let interval = parse_duration(&cli.interval).context("--interval")?;
    let max_interval = parse_duration(&cli.max_interval).context("--max-interval")?;
    let connect_timeout = parse_duration(&cli.connect_timeout).context("--connect-timeout")?;
    let expect = StatusSpec::parse(&cli.expect_status).context("--expect-status")?;
    if !(cli.backoff.is_finite() && cli.backoff >= 1.0) {
        anyhow::bail!("--backoff must be a number >= 1.0 (1.0 means a flat poll)");
    }

    let targets = cli
        .targets
        .iter()
        .map(|t| Target::parse(t))
        .collect::<Result<Vec<_>>>()?;

    if !cli.quiet
        && !targets.iter().any(|t| matches!(t, Target::Http { .. }))
        && cli.expect_status != "200-399"
    {
        // Saying so beats silently ignoring it: --expect-status against a
        // pure TCP wait is almost always someone who meant to pass a URL.
        eprintln!("portwait: note: --expect-status only applies to http(s) targets; ignoring it");
    }

    // One shared deadline across every target, so `-t 30s` means 30 seconds
    // total and not 30 seconds each — which is what a CI job's own timeout is
    // actually budgeted against.
    let started = Instant::now();
    for target in &targets {
        let remaining = timeout.saturating_sub(started.elapsed());
        let cfg = WaitConfig {
            timeout: remaining,
            interval,
            backoff: cli.backoff,
            max_interval,
        };

        if !cli.quiet {
            eprintln!(
                "portwait: waiting for {target} (timeout {}, interval {})",
                format_duration(remaining),
                format_duration(interval)
            );
        }

        let prober = prober_for(target, expect.clone(), connect_timeout);
        let mut last_reported = String::new();
        let result = wait_for(prober.as_ref(), &cfg, |attempt, reason| {
            // Only speak up when the reason changes (plus the first attempt).
            // A 30-second wait at 250ms otherwise emits 120 identical lines,
            // which is noise in a CI log; a *change* — "connection refused"
            // becoming "status 503" — is genuinely worth seeing.
            if !cli.quiet && reason != last_reported {
                eprintln!("portwait:   attempt {attempt}: {reason}");
                last_reported = reason.to_string();
            }
        });

        match result {
            Ok(ready) => {
                if !cli.quiet {
                    eprintln!(
                        "portwait: {target} ready after {} ({} attempt{})",
                        format_duration(ready.elapsed),
                        ready.attempts,
                        if ready.attempts == 1 { "" } else { "s" }
                    );
                }
            }
            Err(t) => {
                anyhow::bail!(
                    "timed out after {} waiting for {target} ({} attempt{}); last error: {}",
                    format_duration(started.elapsed()),
                    t.attempts,
                    if t.attempts == 1 { "" } else { "s" },
                    t.last_error
                );
            }
        }
    }

    if cli.command.is_empty() {
        return Ok(0);
    }

    if !cli.quiet {
        eprintln!("portwait: running: {}", cli.command.join(" "));
    }
    // On Unix this never returns: the command takes over this process.
    run_following_command(&cli.command)
}
