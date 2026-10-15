# knell

Batteries-included Unix-socket daemon harness: double-fork daemonization,
readiness pipe, signal self-pipe, `SO_PEERCRED` authorization, stale-socket
reclaim.

## Motivation

knell is built with the goal of packaging the (relatively unglamorous) lifecycle
plumbing every Unix-socket daemon reimplements. That is, and you may consider
this a feature list of sorts, becoming a daemon cleanly, telling the launching
shell whether it came up, shutting down on a signal without racing its accept
loop, binding a socket nobody else can read, and refusing connections from the
wrong user.

At the low low cost of a tiny library, you get a bunch of things that you would
normally write over and over again.

## What you get

- **`Daemonize`**. Double-fork + `setsid`, optional working-directory change and
  stdio redirection to `/dev/null`. An optional readiness pipe lets the daemon
  report success or a startup error back to the foreground parent, which prints
  status and exits with the matching code.
- **`signals::SignalPipe`** is a self-pipe installer for `SIGINT`/`SIGTERM`. The
  handler is async-signal-safe (i.e., it only flips an atomic and writes one
  byte); poll `read_fd()` alongside your listener and check
  `shutdown_requested()`.
- **`socket::bind_unix`** binds a `UnixListener` with `umask 0o077`, applies an
  explicit mode (e.g. `0o600`), and reclaims a stale socket after probing it
  (while refusing to clobber a live daemon or a non-socket path).
- **`socket::peer_uid` / `socket::require_uid`** are for peer-uid lookup over
  `SO_PEERCRED` and an authorization helper.
- **`FrameReader` / `read_limited_line`** is a newline-framed reader with a
  configurable max-bytes guard.
- **`ConnectionLimiter`** is a mutex-counter that caps concurrent connections
  and frees a slot on drop.
- **`PidFile`** is a pidfile guarded by a non-blocking `flock`; a second daemon
  gets `Error::PidFileLocked` instead of racing. Removed on drop.
- **`Server`** is ties a listener, a `SignalPipe`, and a `ConnectionLimiter`
  together into a ready-made accept loop so you do not hand-wire the poll.

## Usage

The parent process blocks inside `daemonize()` until the daemon reports
readiness, so the launching shell learns whether startup succeeded and exits
with the matching code. `Server::run` then polls the listener and the signal
pipe together and returns cleanly on `SIGINT`/`SIGTERM`.

```rust
use std::io::Write as _;

use knell::{Daemonize, PidFile, Server, SignalPipe, bind_unix, require_uid};

fn main() -> knell::Result<()> {
  let mut readiness = Daemonize::new().daemonize()?;
  let _pidfile = PidFile::create("/run/user/1000/example.pid")?;

  let listener = match bind_unix("/run/user/1000/example.sock", 0o600) {
    Ok(listener) => listener,
    Err(err) => {
      readiness.failed(&err.to_string());
      return Err(err);
    },
  };

  let signals = SignalPipe::install()?;
  let me = unsafe { libc::getuid() };
  readiness.ready_with("listening");

  Server::new(listener, signals, 64).run(move |mut stream| {
    if require_uid(&stream, me).is_ok() {
      let _ = stream.write_all(b"hello\n");
    }
  })
}
```

If you need control over the loop itself, you may instead drive it by hand.
Simply poll `signals.read_fd()` alongside `listener`, check
`signals.shutdown_requested()`, and gate admission with
`ConnectionLimiter::acquire`.

### Platform Notes

For the time being Knell is **Linux-only**. This will no doubt improve, but we
are bound by certain limitations. `peer_uid` / `require_uid` use `SO_PEERCRED`
and are available on Linux only; on every other target they return an
`Unsupported` error (`require_uid` maps this to `Error::Unsupported`).
`Daemonize`, `SignalPipe`, `bind_unix`, `PidFile`, `Server`, `FrameReader`, and
`ConnectionLimiter` compile and run on any Unix.

## License

<!--markdownlint-disable MD033 MD059-->

[provided here]: https://interoperable-europe.ec.europa.eu/sites/default/files/custom-page/attachment/eupl_v1.2_en.pdf

This project is made available under European Union Public Licence (EUPL)
version 1.2. See [LICENSE](LICENSE) for more details on the exact conditions. An
online copy is [provided here].

<!--markdownlint-enable MD033 MD059-->
