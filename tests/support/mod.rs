//! Real servers for the integration tests. Nothing here is a mock: every
//! helper binds a genuine socket on 127.0.0.1 and every port is assigned by
//! the OS (`:0`) rather than hard-coded, so these tests can't collide with
//! anything else running on the machine.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// The `portwait` binary cargo just built for this test run.
pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_portwait")
}

/// Asks the OS for a free port, then releases it.
///
/// This is the only way to test "blocks until something starts listening":
/// the port genuinely has to be closed first, and a listener that is already
/// bound accepts connections into the kernel backlog whether or not anything
/// ever calls `accept()`, so "delay the accept" would not delay `connect` at
/// all. `TcpListener` sets `SO_REUSEADDR` on Unix, so rebinding this exact
/// address a moment later works.
pub fn reserve_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an OS-assigned port");
    let addr = listener.local_addr().expect("read back the assigned port");
    drop(listener);
    addr
}

/// Starts listening on `addr` after `delay`, then accepts forever.
pub fn spawn_delayed_listener(addr: SocketAddr, delay: Duration) {
    thread::spawn(move || {
        thread::sleep(delay);
        let listener = TcpListener::bind(addr)
            .unwrap_or_else(|e| panic!("test listener could not bind {addr}: {e}"));
        for stream in listener.incoming().flatten() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    });
}

/// A real HTTP/1.1 server that is listening from the very first millisecond
/// but answers 503 until `unhealthy_for` has elapsed, then 200 — i.e. exactly
/// the app that a plain TCP check declares ready far too early.
pub struct FlakyServer {
    pub addr: SocketAddr,
    pub served_503: Arc<AtomicUsize>,
    pub served_200: Arc<AtomicUsize>,
}

impl FlakyServer {
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

/// `unhealthy_for = None` means "never become healthy".
pub fn spawn_flaky_http_server(unhealthy_for: Option<Duration>) -> FlakyServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an OS-assigned port");
    let addr = listener.local_addr().expect("read back the assigned port");
    let served_503 = Arc::new(AtomicUsize::new(0));
    let served_200 = Arc::new(AtomicUsize::new(0));

    let c503 = Arc::clone(&served_503);
    let c200 = Arc::clone(&served_200);
    let started = Instant::now();

    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            // Drain the whole request head before replying and closing.
            // Leaving unread bytes in the receive buffer at close time makes
            // the kernel send RST instead of FIN, which the client then
            // reports as "connection reset" instead of a clean response.
            if read_request_head(&mut stream).is_err() {
                continue;
            }

            let healthy = unhealthy_for.is_some_and(|d| started.elapsed() >= d);
            let (status, body) = if healthy {
                c200.fetch_add(1, Ordering::SeqCst);
                ("200 OK", "ready")
            } else {
                c503.fetch_add(1, Ordering::SeqCst);
                ("503 Service Unavailable", "starting up")
            };

            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Write);
        }
    });

    FlakyServer {
        addr,
        served_503,
        served_200,
    }
}

fn read_request_head(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut seen = Vec::with_capacity(512);
    let mut buf = [0u8; 256];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        seen.extend_from_slice(&buf[..n]);
        if seen.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(());
        }
    }
}
