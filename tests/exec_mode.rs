//! `portwait ... -- some command`: it has to actually run, actually not run,
//! and actually pass its exit code back.

mod support;

use std::process::Command;
use std::time::{Duration, Instant};

use support::{bin, reserve_port, spawn_delayed_listener};

#[test]
fn runs_the_trailing_command_for_real_once_the_port_opens() {
    let addr = reserve_port();
    let start = Instant::now();
    spawn_delayed_listener(addr, Duration::from_millis(500));

    let out = Command::new(bin())
        .args([
            &addr.to_string(),
            "--timeout",
            "10s",
            "--interval",
            "50ms",
            "--",
            "echo",
            "portwait-exec-ran",
        ])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!("[measured] exec after a 500ms-delayed listener: {elapsed:?}");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("portwait-exec-ran"),
        "the command's real stdout should reach our stdout unchanged, got: {stdout:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(500),
        "it ran the command at {elapsed:?}, before the port was open"
    );
    assert!(elapsed < Duration::from_millis(1500), "took {elapsed:?}");
}

#[test]
fn does_not_run_the_trailing_command_when_the_wait_times_out() {
    let start = Instant::now();
    let out = Command::new(bin())
        .args([
            "127.0.0.1:1",
            "--timeout",
            "700ms",
            "--interval",
            "100ms",
            "--",
            "echo",
            "portwait-should-not-have-run",
        ])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!("[measured] exec suppressed by a 700ms timeout: {elapsed:?}");

    assert!(!out.status.success(), "a timeout must exit nonzero");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("portwait-should-not-have-run"),
        "the command ran anyway — that is the one thing this mode must never \
         do; stdout: {stdout:?}"
    );
    assert!(
        stdout.trim().is_empty(),
        "nothing should reach stdout: {stdout:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(650),
        "gave up after {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(2500),
        "overran: {elapsed:?}"
    );
}

#[test]
fn the_commands_own_exit_code_becomes_portwaits_exit_code() {
    let addr = reserve_port();
    spawn_delayed_listener(addr, Duration::from_millis(100));

    let out = Command::new(bin())
        .args([
            &addr.to_string(),
            "--timeout",
            "10s",
            "--interval",
            "50ms",
            "--quiet",
            "--",
            "sh",
            "-c",
            "exit 7",
        ])
        .output()
        .expect("run portwait");

    assert_eq!(
        out.status.code(),
        Some(7),
        "a wrapper that swallows the wrapped command's exit code silently \
         breaks every `set -e` script it is dropped into"
    );
}

#[test]
fn quiet_keeps_stderr_clean_so_the_command_owns_the_output() {
    let addr = reserve_port();
    spawn_delayed_listener(addr, Duration::from_millis(100));

    let out = Command::new(bin())
        .args([
            &addr.to_string(),
            "--timeout",
            "10s",
            "--interval",
            "50ms",
            "--quiet",
            "--",
            "sh",
            "-c",
            "echo out; echo err 1>&2",
        ])
        .output()
        .expect("run portwait");

    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "out");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "err",
        "--quiet should leave stderr entirely to the exec'd command"
    );
}

#[test]
fn a_command_that_does_not_exist_is_reported_clearly() {
    let addr = reserve_port();
    spawn_delayed_listener(addr, Duration::from_millis(50));

    let out = Command::new(bin())
        .args([
            &addr.to_string(),
            "--timeout",
            "10s",
            "--quiet",
            "--",
            "portwait-no-such-program",
        ])
        .output()
        .expect("run portwait");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("portwait-no-such-program"),
        "stderr should name the command it could not run: {stderr}"
    );
}
