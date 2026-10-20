//! Integration tests that actually exercise the fork, signal, and socket paths.
//!
//! Each test isolates the process-global and signal-handler state it touches by
//! running inside a forked child, so the tests do not interfere with one
//! another or with the cargo test harness.

#![expect(
  clippy::exit,
  clippy::expect_used,
  clippy::panic,
  reason = "forked test children report through exit codes, helpers fail the test"
)]

use std::{
  env,
  fs,
  io::{
    BufRead as _,
    BufReader,
    Write as _,
  },
  os::unix::net::UnixStream,
  path::{
    Path,
    PathBuf,
  },
  process,
  sync::atomic::{
    AtomicU64,
    Ordering,
  },
  thread,
  time::{
    Duration,
    Instant,
  },
};

use knell::{
  daemonize::Daemonize,
  framed::{
    DEFAULT_MAX_FRAME_BYTES,
    FrameReader,
  },
  pidfile::PidFile,
  server::Server,
  signals::SignalPipe,
  socket::{
    bind_unix,
    peer_uid,
    require_uid,
  },
};

fn scratch_dir(tag: &str) -> PathBuf {
  let dir = env::temp_dir().join(format!("knell-it-{tag}-{}-{}", process::id(), unique()));
  fs::create_dir_all(&dir).expect("create scratch dir");
  dir
}

fn unique() -> u64 {
  static COUNTER: AtomicU64 = AtomicU64::new(0);
  COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn wait_for<F>(deadline: Duration, cond: F) -> bool
where
  F: Fn() -> bool,
{
  let start = Instant::now();
  while start.elapsed() < deadline {
    if cond() {
      return true;
    }
    thread::sleep(Duration::from_millis(20));
  }
  cond()
}

#[test]
fn daemonize_round_trip() {
  let dir = scratch_dir("daemon");
  let sock = dir.join("echo.sock");
  let pid = dir.join("echo.pid");
  let sentinel = dir.join("shutdown.ok");

  // SAFETY: single fork; the child runs the daemon and never returns here.
  let child = unsafe { libc::fork() };
  assert!(child >= 0_i32, "fork failed");

  if child == 0_i32 {
    run_daemon(&sock, &pid, &sentinel);
  }

  let readiness_ok = wait_for(Duration::from_secs(10), || reap_once(child) == Some(0_i32));
  assert!(readiness_ok, "daemon did not report readiness with exit 0");
  assert!(sock.exists(), "socket missing after readiness");

  let mut stream = connect_with_retry(&sock);
  let me = own_uid();
  assert_eq!(
    peer_uid(&stream).expect("peer_uid"),
    me,
    "peer uid should match our own uid"
  );

  stream
    .set_read_timeout(Some(Duration::from_secs(5)))
    .expect("set read timeout");
  stream.write_all(b"ping\n").expect("write frame");

  let mut reply = Vec::new();
  BufReader::new(&stream)
    .read_until(b'\n', &mut reply)
    .expect("read reply");
  assert_eq!(reply, b"ping\n", "echoed frame should round-trip");

  let daemon_pid = read_pid(&pid);
  assert!(daemon_pid > 0_i32, "pidfile should contain the daemon pid");

  // SAFETY: signalling the daemon process we just identified.
  assert_eq!(unsafe { libc::kill(daemon_pid, libc::SIGTERM) }, 0_i32);

  assert!(
    wait_for(Duration::from_secs(10), || sentinel.exists()),
    "SignalPipe should drive a clean shutdown (sentinel never appeared)"
  );
  assert!(
    wait_for(Duration::from_secs(5), || !pid.exists()),
    "pidfile should be removed on clean shutdown"
  );

  drop(fs::remove_dir_all(&dir));
}

fn run_daemon(sock: &Path, pid: &Path, sentinel: &Path) -> ! {
  let mut readiness = Daemonize::new()
    .keep_working_directory()
    .redirect_stdio(false)
    .daemonize()
    .expect("daemonize");

  let pidfile = match PidFile::create(pid) {
    Ok(pidfile) => pidfile,
    Err(err) => {
      readiness.failed(&err.to_string());
      process::exit(1);
    },
  };

  let listener = match bind_unix(sock, 0o600) {
    Ok(listener) => listener,
    Err(err) => {
      readiness.failed(&err.to_string());
      process::exit(1);
    },
  };

  let signals = match SignalPipe::install() {
    Ok(signals) => signals,
    Err(err) => {
      readiness.failed(&err.to_string());
      process::exit(1);
    },
  };

  readiness.ready();

  let me = own_uid();
  let server = Server::new(listener, signals, 8);
  let run = server.run(move |mut stream| {
    if require_uid(&stream, me).is_err() {
      return;
    }
    let frame = {
      let mut reader = FrameReader::new(BufReader::new(&stream), DEFAULT_MAX_FRAME_BYTES);
      reader.read_frame()
    };
    if let Ok(Some(mut echo)) = frame {
      echo.push(b'\n');
      drop(stream.write_all(&echo));
    }
  });

  drop(pidfile);
  drop(fs::remove_file(sock));
  if run.is_ok() {
    drop(fs::write(sentinel, b"ok"));
  }
  process::exit(0);
}

#[test]
fn signal_pipe_flips_and_wakes() {
  // SAFETY: single fork; the child isolates the signal handlers and global
  // shutdown state so it cannot disturb the test harness.
  let child = unsafe { libc::fork() };
  assert!(child >= 0_i32, "fork failed");

  if child == 0_i32 {
    let Ok(signals) = SignalPipe::install() else {
      process::exit(2);
    };
    assert!(!signals.shutdown_requested());

    // SAFETY: getpid has no preconditions.
    let me = unsafe { libc::getpid() };
    // SAFETY: deliver SIGTERM to this process; delivered before kill returns.
    if unsafe { libc::kill(me, libc::SIGTERM) } != 0_i32 {
      process::exit(5);
    }

    if !signals.shutdown_requested() {
      process::exit(3);
    }

    let mut poll = libc::pollfd {
      fd:      signals.read_fd(),
      events:  libc::POLLIN,
      revents: 0,
    };
    // SAFETY: `poll` points at one valid `pollfd`; a 1s timeout bounds the
    // wait.
    let rc = unsafe { libc::poll(&raw mut poll, 1, 1000) };
    if rc != 1_i32 || poll.revents & libc::POLLIN == 0 {
      process::exit(4);
    }

    process::exit(0);
  }

  let status = wait_blocking(child);
  assert_eq!(
    status,
    Some(0_i32),
    "child signal assertions failed (code shown)"
  );
}

fn own_uid() -> u32 {
  // SAFETY: getuid is always safe and never fails.
  unsafe { libc::getuid() }
}

fn connect_with_retry(path: &Path) -> UnixStream {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    match UnixStream::connect(path) {
      Ok(stream) => return stream,
      Err(_) if Instant::now() < deadline => {
        thread::sleep(Duration::from_millis(20));
      },
      Err(err) => panic!("connect to {}: {err}", path.display()),
    }
  }
}

fn read_pid(path: &Path) -> libc::pid_t {
  let contents = fs::read_to_string(path).unwrap_or_default();
  contents.trim().parse().unwrap_or(0)
}

fn reap_once(child: libc::pid_t) -> Option<i32> {
  let mut status = 0_i32;
  // SAFETY: standard non-blocking wait for a known child pid.
  let rc = unsafe { libc::waitpid(child, &raw mut status, libc::WNOHANG) };
  if rc == child { exit_code(status) } else { None }
}

fn wait_blocking(child: libc::pid_t) -> Option<i32> {
  let mut status = 0_i32;
  // SAFETY: standard blocking wait for a known child pid.
  let rc = unsafe { libc::waitpid(child, &raw mut status, 0) };
  if rc == child { exit_code(status) } else { None }
}

const fn exit_code(status: libc::c_int) -> Option<i32> {
  if libc::WIFEXITED(status) {
    Some(libc::WEXITSTATUS(status))
  } else {
    None
  }
}
