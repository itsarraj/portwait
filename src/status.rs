//! What counts as "ready" in HTTP mode.
//!
//! The whole reason HTTP mode exists is that a socket accepting connections
//! and an app being ready are different events, so the acceptance test has to
//! be precise about *which* responses mean ready. `--expect-status` takes a
//! spec rather than a single number because real health endpoints differ:
//! `200` for most, `200,204` for ones that 204 when there's nothing to
//! report, `2xx` when you don't care which, `200-399` (the default) when a
//! redirect to a login page still proves the app is up.

use std::fmt;

use anyhow::{bail, Result};

/// A set of acceptable HTTP status codes, expressed as inclusive ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSpec {
    ranges: Vec<(u16, u16)>,
    source: String,
}

impl StatusSpec {
    /// Parses a comma-separated spec. Each item is a single code (`200`), an
    /// inclusive range (`200-299`), or a class shorthand (`2xx`).
    pub fn parse(input: &str) -> Result<Self> {
        let mut ranges = Vec::new();

        for item in input.split(',') {
            let item = item.trim();
            if item.is_empty() {
                bail!("`{input}` is not a status spec: empty item (a stray comma?)");
            }

            let stripped = item.strip_suffix("xx").or_else(|| item.strip_suffix("XX"));
            let range = if let Some(class) = stripped {
                let digit: u16 = parse_code_part(class, input)?;
                if !(1..=5).contains(&digit) {
                    bail!("`{item}` is not a status class (expected 1xx through 5xx)");
                }
                (digit * 100, digit * 100 + 99)
            } else if let Some((lo, hi)) = item.split_once('-') {
                let lo = parse_code_part(lo.trim(), input)?;
                let hi = parse_code_part(hi.trim(), input)?;
                check_code(lo, input)?;
                check_code(hi, input)?;
                if lo > hi {
                    bail!("`{item}` is a backwards range: {lo} is greater than {hi}");
                }
                (lo, hi)
            } else {
                let code = parse_code_part(item, input)?;
                check_code(code, input)?;
                (code, code)
            };

            ranges.push(range);
        }

        Ok(StatusSpec {
            ranges,
            source: input.trim().to_string(),
        })
    }

    /// Does this response code count as ready?
    pub fn matches(&self, code: u16) -> bool {
        self.ranges.iter().any(|&(lo, hi)| code >= lo && code <= hi)
    }
}

impl fmt::Display for StatusSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Echo back what the user typed rather than a normalized form, so the
        // timeout message quotes the same string that's in their compose file.
        f.write_str(&self.source)
    }
}

fn parse_code_part(part: &str, whole: &str) -> Result<u16> {
    part.parse::<u16>()
        .map_err(|_| anyhow::anyhow!("`{whole}` is not a status spec: `{part}` is not a number"))
}

fn check_code(code: u16, whole: &str) -> Result<()> {
    if !(100..=599).contains(&code) {
        bail!("`{whole}` is not a status spec: {code} is not an HTTP status code (100-599)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_code_matches_only_itself() {
        let spec = StatusSpec::parse("200").unwrap();
        assert!(spec.matches(200));
        // The exact false positive HTTP mode exists to prevent: a server that
        // is listening and answering, but answering 503.
        assert!(!spec.matches(503));
        assert!(!spec.matches(201));
    }

    #[test]
    fn an_inclusive_range_includes_both_ends() {
        let spec = StatusSpec::parse("200-299").unwrap();
        assert!(spec.matches(200));
        assert!(spec.matches(250));
        assert!(spec.matches(299));
        assert!(!spec.matches(199));
        assert!(!spec.matches(300));
    }

    #[test]
    fn the_class_shorthand_expands_to_the_whole_hundred() {
        let spec = StatusSpec::parse("2xx").unwrap();
        assert!(spec.matches(200));
        assert!(spec.matches(204));
        assert!(spec.matches(299));
        assert!(!spec.matches(300));
    }

    #[test]
    fn a_comma_list_mixes_singles_ranges_and_classes() {
        let spec = StatusSpec::parse("200,204,301-302,5xx").unwrap();
        for ok in [200, 204, 301, 302, 500, 599] {
            assert!(spec.matches(ok), "{ok} should match");
        }
        for not_ok in [201, 300, 303, 499] {
            assert!(!spec.matches(not_ok), "{not_ok} should not match");
        }
    }

    #[test]
    fn the_default_spec_accepts_a_redirect_but_not_a_server_error() {
        let spec = StatusSpec::parse("200-399").unwrap();
        assert!(spec.matches(200));
        assert!(
            spec.matches(302),
            "a redirect to /login still proves it's up"
        );
        assert!(!spec.matches(404));
        assert!(!spec.matches(503));
    }

    #[test]
    fn malformed_specs_are_rejected_with_a_specific_reason() {
        for bad in ["", "ok", "600", "99", "299-200", "200,", "9xx"] {
            assert!(
                StatusSpec::parse(bad).is_err(),
                "`{bad}` should have been rejected"
            );
        }
    }

    #[test]
    fn display_echoes_the_spec_the_user_typed() {
        assert_eq!(StatusSpec::parse("200,204").unwrap().to_string(), "200,204");
    }
}
