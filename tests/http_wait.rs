//! HTTP mode, against a real HTTP server that is genuinely unhealthy first.
//!
//! The server these tests use is *listening from the first millisecond* and
//! answers real 503s for the first half second. That is the actual false
//! positive this mode exists to fix, so the tests assert on it directly: the
//! TCP check passes immediately (wrongly), the HTTP check does not.

mod support;

use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use support::{bin, spawn_flaky_http_server};

#[test]
fn waits_for_a_real_200_and_is_not_fooled_by_the_real_503s_it_sees_first() {
    let server = spawn_flaky_http_server(Some(Duration::from_millis(500)));
    let start = Instant::now();

    let out = Command::new(bin())
        .args([
            &server.url("/healthz"),
            "--expect-status",
            "200",
            "--timeout",
            "10s",
            "--interval",
            "50ms",
        ])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    let served_503 = server.served_503.load(Ordering::SeqCst);
    let served_200 = server.served_200.load(Ordering::SeqCst);
    eprintln!(
        "[measured] http wait for a 503->200 server: {elapsed:?} \
         ({served_503} real 503s served, then {served_200} x 200)"
    );

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        served_503 > 0,
        "the server never actually served a 503, so this run proves nothing \
         about false positives"
    );
    assert_eq!(
        served_200, 1,
        "it should stop polling the instant it gets a 200"
    );
    assert!(
        elapsed >= Duration::from_millis(500),
        "returned in {elapsed:?} — it accepted one of the 503s as ready"
    );
    assert!(
        elapsed < Duration::from_millis(1500),
        "took {elapsed:?} to notice a 200 that was available at 500ms"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("status 503"),
        "the progress output should say what it actually saw: {stderr}"
    );
}

#[test]
fn a_tcp_check_on_that_same_port_passes_immediately_which_is_the_whole_point() {
    let server = spawn_flaky_http_server(Some(Duration::from_millis(500)));

    let start = Instant::now();
    let out = Command::new(bin())
        .args([&server.addr.to_string(), "--timeout", "10s"])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!("[measured] tcp check against the same still-503 server: {elapsed:?}");

    assert!(out.status.success());
    assert!(
        elapsed < Duration::from_millis(400),
        "the socket is open from t=0, so TCP mode should pass at once; took {elapsed:?}"
    );
    assert_eq!(
        server.served_200.load(Ordering::SeqCst),
        0,
        "TCP mode never even asked the app — which is exactly why it is the \
         wrong check for an app that boots slowly"
    );
}

#[test]
fn times_out_nonzero_when_the_status_never_becomes_acceptable() {
    let server = spawn_flaky_http_server(None); // 503 forever

    let start = Instant::now();
    let out = Command::new(bin())
        .args([
            &server.url("/healthz"),
            "--expect-status",
            "200",
            "--timeout",
            "1s",
            "--interval",
            "100ms",
        ])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!(
        "[measured] http timeout against a permanently-503 server (--timeout 1s): {elapsed:?}, \
         {} 503s served",
        server.served_503.load(Ordering::SeqCst)
    );

    assert!(!out.status.success(), "a timeout must exit nonzero");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        elapsed >= Duration::from_millis(950),
        "gave up after {elapsed:?} instead of retrying for the full timeout"
    );
    assert!(
        elapsed < Duration::from_millis(3000),
        "overran: {elapsed:?}"
    );
    assert!(
        server.served_503.load(Ordering::SeqCst) >= 2,
        "it should have polled repeatedly, not once"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("timed out"), "stderr: {stderr}");
    assert!(
        stderr.contains("status 503"),
        "the failure should quote the real status it kept getting: {stderr}"
    );
}

#[test]
fn a_range_spec_accepts_a_status_a_single_code_would_have_rejected() {
    let server = spawn_flaky_http_server(None); // 503 forever

    let out = Command::new(bin())
        .args([
            &server.url("/healthz"),
            "--expect-status",
            "500-599",
            "--timeout",
            "2s",
        ])
        .output()
        .expect("run portwait");

    assert!(
        out.status.success(),
        "503 is inside 500-599, so this should be ready at once; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(server.served_503.load(Ordering::SeqCst), 1);
}

#[test]
fn a_connection_refused_url_times_out_rather_than_erroring_out_early() {
    let start = Instant::now();
    let out = Command::new(bin())
        .args([
            "http://127.0.0.1:1/healthz",
            "--timeout",
            "1s",
            "--interval",
            "100ms",
        ])
        .output()
        .expect("run portwait");
    let elapsed = start.elapsed();
    eprintln!("[measured] http against a closed port (--timeout 1s): {elapsed:?}");

    assert!(!out.status.success());
    assert!(
        elapsed >= Duration::from_millis(950),
        "a not-yet-started server is the normal case for this tool; it must \
         keep retrying, not fail at {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(3000),
        "overran: {elapsed:?}"
    );
}
