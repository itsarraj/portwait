# portwait

Block until a dependency is actually ready, then get out of the way. This is
the job `wait-for-it.sh` does — the 180-line bash script that gets vendored
into half the `Dockerfile`s on earth — and that `dockerize` (Go) does with a
whole container-entrypoint framework attached. Rust has no equivalent: a
single static binary you can `COPY --from` into a scratch image, that knows
how to wait for a TCP port *and* how to wait for an HTTP health check, and
that fails loudly with a nonzero exit code instead of hanging forever.

The second part matters more than it sounds. A socket accepts connections the
moment the process `bind()`s and `listen()`s — well before the app behind it
has run migrations, warmed a connection pool, or decided it is healthy.
Waiting on the socket alone is a false positive, and it's the one that makes
"but it passes locally" CI failures so hard to pin down. Against a real
Python HTTP server that answers 503 for its first two seconds, this is the
whole problem in six lines:

```
$ portwait 127.0.0.1:49319
portwait: waiting for tcp 127.0.0.1:49319 (timeout 30.00s, interval 250ms)
portwait: tcp 127.0.0.1:49319 ready after <1ms (1 attempt)       # wrong

$ portwait http://127.0.0.1:49319/healthz --expect-status 200
portwait:   attempt 1: got status 503, want 200
portwait: http http://127.0.0.1:49319/healthz ready after 1.76s (8 attempts)
```

## Usage

```bash
# TCP mode — the wait-for-it.sh check. Anything without a URL scheme.
portwait db:5432
portwait db:5432 -t 90s -i 500ms          # 90s budget, poll twice a second
portwait '[::1]:5432'                     # IPv6 literals, brackets and all
portwait db:5432 redis:6379               # several deps, one shared timeout

# HTTP mode — poll a real health endpoint, not just the socket.
portwait http://app:8080/healthz --expect-status 200
portwait http://app:8080/healthz --expect-status 2xx
portwait http://app:8080/ready --expect-status 200,204
portwait https://api.example.com/health   # TLS works; certs are verified

# Exec mode — wait, then hand the process over. The compose/CI shape.
portwait db:5432 -- npm start
portwait db:5432 redis:6379 --quiet -- ./bin/server
```

A real run, start to finish, against a Python server that 503s for two
seconds and then returns 200:

```
$ portwait http://127.0.0.1:49319/healthz --expect-status 200 -- echo "app is genuinely up"
portwait: waiting for http http://127.0.0.1:49319/healthz (timeout 30.00s, interval 250ms)
portwait:   attempt 1: got status 503, want 200
portwait: http http://127.0.0.1:49319/healthz ready after 1.76s (8 attempts)
portwait: running: echo app is genuinely up
app is genuinely up
```

...and a real timeout, which is the case a wrapper has to get right:

```
$ portwait 127.0.0.1:1 -t 3s -i 500ms
portwait: waiting for tcp 127.0.0.1:1 (timeout 3.00s, interval 500ms)
portwait:   attempt 1: 127.0.0.1:1: Connection refused (os error 111)
portwait: timed out after 3.00s waiting for tcp 127.0.0.1:1 (7 attempts); last error: 127.0.0.1:1: Connection refused (os error 111)
$ echo $?
1
```

In a compose file that's:

```yaml
command: ["portwait", "db:5432", "http://cache:6379/", "-t", "60s", "--", "npm", "start"]
```

## How it behaves

- **Mode is inferred from the target**, not a flag: `http://`/`https://` means
  an HTTP health check, anything else is a TCP connect. There's no ambiguity
  to resolve — `db:5432` is never a URL and `http://app:8080/healthz` is never
  a `host:port` pair — so a flag would only be one more thing to get wrong.
- **Progress goes to stderr, only when it changes.** A 30-second wait at the
  default 250ms interval is 120 attempts; printing all of them buries a CI log.
  A *change* of reason — "connection refused" becoming "got status 503" — is
  the thing actually worth seeing, so that's what gets printed. `--quiet`
  silences it entirely.
- **`--` execs, it doesn't spawn.** On Unix the trailing command replaces this
  process via `execvp`, so it keeps the same PID, inherits stdin/stdout/stderr
  untouched (a TTY stays a TTY), and receives `SIGTERM` from `docker stop`
  directly rather than through a parent that would have to forward it. Its
  exit code is `portwait`'s exit code.
- **Exit codes**: `0` ready (or the exec'd command's own code), `1` timed out
  or failed, `2` bad usage.
- **The deadline is honoured, not approximated.** Each attempt is handed only
  what's left of `--timeout` as its connect/read budget, and the sleep between
  attempts is clamped so a long `--max-interval` can't return *after* the
  deadline. `--backoff` defaults to `1.0` (flat polling) on purpose: for a
  local dependency that comes up in a second, backing off actively hurts — it
  leaves you asleep through the moment it became ready.
- **DNS failure is "not ready", not fatal.** In a compose network a service
  name routinely doesn't resolve for the first second or two. Resolving once
  up front would make this tool fail instantly in the exact situation it
  exists for, so the name is re-resolved on every attempt.

## Status: built and verified live against real local listeners, with measured timings

Every number below was measured by the integration tests in `tests/`, which
bind genuine sockets on OS-assigned ports (`127.0.0.1:0`, never a hard-coded
port) and assert on real elapsed wall time — not on "it returned".

- **50 tests** (`cargo test`): 34 unit tests plus 16 integration tests driving
  the real binary as a subprocess.
- **It really blocks, and really unblocks.** A port is reserved from the OS,
  released, and a thread rebinds it exactly 500ms later; `portwait` against it
  returned in **503.9ms / 504.8ms / 512.2ms** across runs — past the 500ms
  mark (so it cannot have false-positived) and within ~12ms of it (so the
  poll loop notices promptly). This is the only honest way to test it: a
  listener that is already bound accepts connections into the kernel backlog
  whether or not anything ever calls `accept()`, so "delay the accept" would
  not delay `connect` by a single millisecond. The port genuinely has to be
  closed first.
- **It really times out.** Pointed at `127.0.0.1:1` (privileged, so no other
  process on the machine can open it and turn the test green for the wrong
  reason) with `--timeout 1s`: exited **1** after **1.0020s / 1.0025s /
  1.0032s** — not instantly on the first `ECONNREFUSED`, not hanging, and
  with stderr naming the target and quoting the real `Connection refused
  (os error 111)` it kept getting.
- **The HTTP false positive, demonstrated both ways on one port.** A
  hand-written HTTP/1.1 server binds immediately but answers real 503s for
  500ms, then 200. HTTP mode with `--expect-status 200` returned in
  **507.0ms / 507.0ms / 516.4ms** having been served **10 real 503 responses
  first** (asserted, not assumed — if the server never actually served a 503
  the test fails as proving nothing) and exactly one 200, stopping on it. A
  TCP check against **that same port** returned in **1.9ms / 2.3ms / 2.5ms**
  with the app having been asked nothing at all. That ~505ms gap is the bug
  this mode fixes, reproduced.
- **A never-healthy server is a timeout, not a hang**: the same server pinned
  at 503 forever, `--expect-status 200 --timeout 1s` → exit **1** after
  **1.0022s**, having polled **11 times**, with the failure message quoting
  `got status 503, want 200`. And `--expect-status 500-599` against that same
  permanently-503 server succeeds on its first attempt, confirming the spec is
  really being evaluated rather than 200 being hard-coded somewhere.
- **The trailing command genuinely runs — and genuinely doesn't.** `-- echo
  portwait-exec-ran` after a 500ms-delayed listener: the marker string was
  captured on real stdout at **504.4ms / 504.9ms / 510.0ms**. The same
  invocation against `127.0.0.1:1` with `--timeout 700ms`: exit **1** at
  **702.0ms / 702.3ms / 710.1ms** with **stdout completely empty** — the
  command did not run. Exit-code passthrough is checked with `-- sh -c 'exit
  7'` (`portwait` exits 7), and `--quiet -- sh -c 'echo out; echo err 1>&2'`
  confirms stdout and stderr both arrive unmodified with nothing of
  `portwait`'s own mixed in.
- **Multi-target and backoff, also measured**: two ports opening at 200ms and
  700ms → **704.4ms / 708.0ms / 720.0ms** (it waits for the slower one, and
  one `--timeout` covers both). With `--backoff 2.0 --max-interval 300ms`
  against a 400ms delay → **652.4ms / 653.0ms / 658.8ms**, i.e. it still
  finds the port, overshooting by at most one capped interval, which is the
  cost of backing off and why the default is not to.
- **HTTPS, checked by hand against the real internet** (not in the test suite,
  which is deliberately self-contained and offline): `portwait
  https://github.com --expect-status 2xx` → ready in **263ms, 1 attempt**; and
  `portwait https://expired.badssl.com --expect-status 200 -t 3s` correctly
  never succeeded, reporting the real `rustls` rejection —
  `invalid peer certificate: certificate expired` — and exiting **1**.
  Certificate validation is genuinely on, not skipped.
- **Unit tests cover the parts worth testing without a socket**: duration
  parsing (`250ms`/`1.5s`/`2m`/`1h`/bare-number-means-seconds, and that
  `30sec` is *rejected* rather than guessed at — guessing is friendly right up
  until someone writes `30min` and gets 30 seconds); `--expect-status` specs
  (singles, inclusive ranges, `2xx` classes, comma lists, and that `299-200`
  and `600` are errors); target parsing (including that `[fe80::1%eth0]:80`
  isn't torn in half at its own colons, and that `http://app:8080/healthz` is
  never mistaken for a `host:port` pair); and the retry schedule itself
  against a fake prober — that a zero timeout still makes one real attempt,
  that the per-probe budget shrinks toward the deadline, and that backoff
  stops at `--max-interval` instead of growing without bound.

**Deliberately deferred**: GET only in HTTP mode — no `--method`, request body,
or custom headers, so a health endpoint behind auth isn't reachable yet;
no response-body matching (`dockerize` doesn't do this either, but "wait until
`/healthz` contains `"status":"ok"`" is a real request and a clean v2); no
Unix-domain-socket targets; and the DNS lookup inside each attempt uses
`std`'s blocking resolver, which the per-attempt budget cannot interrupt — a
pathologically slow resolver can therefore overshoot `--timeout` by one lookup,
which is the one place the deadline is best-effort rather than exact.
