//! Handing off to the command after `--`.
//!
//! `wait-for-it.sh db:5432 -- npm start` is overwhelmingly how the original is
//! used, so this matches it exactly, including the part that matters most in a
//! container: on Unix the command *replaces* this process via `execvp` rather
//! than being spawned as a child. That means it inherits this process's PID,
//! its stdin/stdout/stderr as-is (no pipes in between, so a TTY stays a TTY
//! and output isn't buffered differently), and receives signals directly —
//! `docker stop` reaches your app instead of stopping at a `portwait` parent
//! that would have to forward SIGTERM itself and get it subtly wrong.

use std::process::Command;

use anyhow::{Context, Result};

/// Replaces this process with `argv` (Unix) or runs it as a child and returns
/// its exit code (elsewhere). On Unix this only ever returns on failure.
#[cfg(unix)]
pub fn run_following_command(argv: &[String]) -> Result<i32> {
    use std::os::unix::process::CommandExt;

    let (program, args) = argv.split_first().context("no command given after `--`")?;

    // `exec` does not return on success — from here on this *is* the command.
    let err = Command::new(program).args(args).exec();
    Err(err).with_context(|| format!("failed to exec `{program}`"))
}

#[cfg(not(unix))]
pub fn run_following_command(argv: &[String]) -> Result<i32> {
    let (program, args) = argv.split_first().context("no command given after `--`")?;

    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run `{program}`"))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_argv_is_an_error_not_a_panic() {
        let err = run_following_command(&[]).unwrap_err().to_string();
        assert!(err.contains("no command given"), "got: {err}");
    }

    #[test]
    fn a_nonexistent_program_reports_which_program_failed() {
        // Not exec'ing anything real here: a failed `execvp` returns normally,
        // so this stays in-process.
        let argv = vec!["portwait-no-such-program-exists".to_string()];
        let err = format!("{:#}", run_following_command(&argv).unwrap_err());
        assert!(
            err.contains("portwait-no-such-program-exists"),
            "got: {err}"
        );
    }
}
