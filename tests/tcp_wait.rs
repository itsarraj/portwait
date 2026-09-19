//! TCP mode, against real sockets, with real measured elapsed time.

mod support;

use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use support::{bin, reserve_port, spawn_delayed_listener};

#[test]
fn blocks_until_a_real_listener_appears_and_unblocks_right_after() {
    let addr = reserve_port();

    // The clock starts *before* the delay thread, so "elapsed >= 500ms" is a
    // statement about real wall time and not about scheduling slop between
    // two separately-started timers.
    let start = Instant::now();
    spawn_delayed_listener(addr, Duration::from_millis(500));

    let out = Command::new(bin())
        .args([&addr.to_string(), "--timeout", "10s", "--interval", "50ms"])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!("[measured] tcp wait for a 500ms-delayed listener: {elapsed:?}");

    assert!(
        out.status.success(),
        "portwait should have succeeded; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed >= Duration::from_millis(500),
        "it returned in {elapsed:?}, before the listener could possibly exist — \
         that would be a false positive, not a wait"
    );
    assert!(
        elapsed < Duration::from_millis(1500),
        "it took {elapsed:?} to notice a port that opened at 500ms; \
         the poll loop is not unblocking promptly"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ready after"), "stderr: {stderr}");
}

#[test]
fn times_out_nonzero_on_a_port_nothing_is_listening_on() {
    // 127.0.0.1:1 needs root to bind, so no other process on this machine —
    // including whatever else is running in parallel right now — can make
    // this port unexpectedly open and turn the test green for the wrong
    // reason.
    let start = Instant::now();
    let out = Command::new(bin())
        .args(["127.0.0.1:1", "--timeout", "1s", "--interval", "100ms"])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!("[measured] tcp timeout with --timeout 1s: {elapsed:?}");

    assert!(!out.status.success(), "a timeout must exit nonzero");
    assert_eq!(out.status.code(), Some(1), "timeout exit code");
    assert!(
        elapsed >= Duration::from_millis(950),
        "gave up after only {elapsed:?}; it should have kept retrying for the \
         full --timeout instead of bailing on the first refused connection"
    );
    assert!(
        elapsed < Duration::from_millis(2500),
        "took {elapsed:?} for a 1s timeout — it overran the deadline"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("timed out"), "stderr: {stderr}");
    assert!(
        stderr.contains("127.0.0.1:1"),
        "stderr must name the target: {stderr}"
    );
    assert!(
        stderr.contains("last error"),
        "stderr must quote why it kept failing: {stderr}"
    );
}

#[test]
fn returns_immediately_when_the_port_is_already_open() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an OS-assigned port");
    let addr = listener.local_addr().unwrap();

    let start = Instant::now();
    let out = Command::new(bin())
        .args([&addr.to_string(), "--timeout", "10s"])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!("[measured] tcp wait for an already-open port: {elapsed:?}");

    assert!(out.status.success());
    assert!(
        elapsed < Duration::from_millis(500),
        "an already-open port should not cost {elapsed:?}"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("1 attempt"));
}

#[test]
fn waits_for_every_target_when_several_are_given() {
    let fast = reserve_port();
    let slow = reserve_port();
    assert_ne!(fast, slow, "the OS should not hand out the same port twice");

    let start = Instant::now();
    spawn_delayed_listener(fast, Duration::from_millis(200));
    spawn_delayed_listener(slow, Duration::from_millis(700));

    let out = Command::new(bin())
        .args([
            &fast.to_string(),
            &slow.to_string(),
            "--timeout",
            "10s",
            "--interval",
            "50ms",
        ])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!("[measured] tcp wait for two targets (200ms + 700ms): {elapsed:?}");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed >= Duration::from_millis(700),
        "returned in {elapsed:?} — it cannot have waited for the slower target"
    );
    assert!(elapsed < Duration::from_millis(2000), "took {elapsed:?}");
}

#[test]
fn a_malformed_target_fails_fast_instead_of_waiting_out_the_timeout() {
    let start = Instant::now();
    let out = Command::new(bin())
        .args(["not-a-target", "--timeout", "10s"])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();

    assert!(!out.status.success());
    assert!(
        elapsed < Duration::from_millis(500),
        "a typo'd target should be reported at once, not after {elapsed:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("has no port"), "stderr: {stderr}");
}

#[test]
fn backoff_still_finds_the_port_and_still_honours_the_deadline() {
    let addr = reserve_port();
    let start = Instant::now();
    spawn_delayed_listener(addr, Duration::from_millis(400));

    let out = Command::new(bin())
        .args([
            &addr.to_string(),
            "--timeout",
            "10s",
            "--interval",
            "50ms",
            "--backoff",
            "2.0",
            "--max-interval",
            "300ms",
        ])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!("[measured] tcp wait with --backoff 2.0 (400ms delay): {elapsed:?}");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed >= Duration::from_millis(400),
        "returned in {elapsed:?}"
    );
    // With backoff the gap grows 50→100→200→300(capped), so the worst-case
    // overshoot past the 400ms mark is one capped interval.
    assert!(
        elapsed < Duration::from_millis(1200),
        "backing off should still notice within one --max-interval, took {elapsed:?}"
    );
}
