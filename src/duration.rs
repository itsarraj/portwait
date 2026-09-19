//! Human-readable durations, in and out.
//!
//! Every time knob on the CLI (`--timeout`, `--interval`, `--max-interval`,
//! `--connect-timeout`) takes the same spelling, because a startup-ordering
//! tool is something people write once in a `compose.yml` and read a year
//! later: `--timeout 90s` should not silently mean 90 milliseconds, and
//! `--interval 250ms` should not need to be written `0.25`.

use std::time::Duration;

use anyhow::{bail, Context, Result};

/// Parses `250ms`, `1.5s`, `2m`, `1h`, or a bare number meaning seconds
/// (`30` == `30s`, the `wait-for-it.sh -t` convention, kept so existing
/// scripts port over without re-learning anything).
pub fn parse_duration(input: &str) -> Result<Duration> {
    let text = input.trim();
    if text.is_empty() {
        bail!("empty duration: expected something like `30s`, `250ms` or `2m`");
    }

    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);

    if number.is_empty() {
        bail!("`{input}` is not a duration: expected a number first, e.g. `30s`, `250ms`, `2m`");
    }

    let value: f64 = number
        .parse()
        .with_context(|| format!("`{input}` is not a duration: `{number}` is not a number"))?;
    if !value.is_finite() {
        bail!("`{input}` is not a finite duration");
    }

    let seconds = match unit {
        "" | "s" => value,
        "ms" => value / 1_000.0,
        "m" => value * 60.0,
        "h" => value * 3_600.0,
        other => bail!("`{input}` has an unknown time unit `{other}` (use ms, s, m or h)"),
    };

    Ok(Duration::from_secs_f64(seconds))
}

/// Renders a duration the way a human reading a terminal wants it: sub-second
/// values in whole milliseconds, everything else in seconds to 2dp. Used in
/// the "ready after ..." line and the timeout message, so the number a user
/// sees is directly comparable to the `--timeout` they typed.
pub fn format_duration(d: Duration) -> String {
    if d < Duration::from_millis(1) {
        // Rounding this down to a flat "0ms" would read as "didn't measure";
        // `<1ms` says "measured, and it really was that fast" — which is the
        // honest answer for an already-open port.
        "<1ms".to_string()
    } else if d < Duration::from_secs(1) {
        format!("{}ms", d.as_millis())
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_supported_unit() {
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn a_bare_number_means_seconds_like_wait_for_it() {
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn fractional_values_are_allowed() {
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_duration("0.25").unwrap(), Duration::from_millis(250));
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        assert_eq!(
            parse_duration("  750ms ").unwrap(),
            Duration::from_millis(750)
        );
    }

    #[test]
    fn an_unknown_unit_is_rejected_rather_than_silently_guessed() {
        // `30sec` and `30secs` are the plausible typos; guessing "seconds"
        // for them would be friendly right up until someone writes `30min`
        // and gets 30 seconds.
        let err = parse_duration("30sec").unwrap_err().to_string();
        assert!(err.contains("unknown time unit"), "got: {err}");
        assert!(err.contains("ms, s, m or h"), "got: {err}");
    }

    #[test]
    fn nonsense_input_is_rejected() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("s").is_err());
    }

    #[test]
    fn formats_sub_second_in_millis_and_the_rest_in_seconds() {
        assert_eq!(format_duration(Duration::from_micros(300)), "<1ms");
        assert_eq!(format_duration(Duration::from_millis(512)), "512ms");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1.50s");
        assert_eq!(format_duration(Duration::from_secs(30)), "30.00s");
    }
}
