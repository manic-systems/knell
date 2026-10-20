//! A signal self-pipe for `SIGINT`/`SIGTERM`.
//!
//! [`SignalPipe::install`] registers an async-signal-safe handler that flips a
//! process-global flag and writes one byte to a pipe, so an accept loop can
//! wake from a blocking poll and shut down cleanly. Dropping the pipe restores
//! the previous handlers.

use std::{
  os::fd::RawFd,
  sync::atomic::{
    AtomicBool,
    AtomicI32,
    Ordering,
  },
};

use crate::error::{
  Result,
  io,
};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// Shutdown status observed across the process.
///
/// # Returns
///
/// `true` once a `SIGINT` or `SIGTERM` has been observed by an installed
/// [`SignalPipe`], and `false` otherwise.
#[must_use]
pub fn shutdown_requested() -> bool {
  SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

/// An installed signal self-pipe.
///
/// Only one should be live at a time per process; the global write end is
/// shared by the async-signal-safe handler.
pub struct SignalPipe {
  read_fd:  RawFd,
  write_fd: RawFd,
  old_int:  libc::sigaction,
  old_term: libc::sigaction,
}

impl SignalPipe {
  /// Installs handlers for `SIGINT` and `SIGTERM` and creates the self-pipe.
  ///
  /// The read end is nonblocking and close-on-exec; poll it alongside your
  /// listener to wake on a signal. Resets [`shutdown_requested`] to `false`.
  ///
  /// # Errors
  ///
  /// Returns an [`Error::Io`](crate::Error::Io) if the pipe cannot be created
  /// or configured, or if a signal handler cannot be installed.
  pub fn install() -> Result<Self> {
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);

    let mut fds = [0; 2];
    // SAFETY: `fds` is a writable pair of integers populated by `pipe`.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
      return Err(io("creating signal pipe")(std::io::Error::last_os_error()));
    }

    if let Err(err) = configure(fds[0], fds[1]) {
      close_fd(fds[0]);
      close_fd(fds[1]);
      return Err(err);
    }

    SHUTDOWN_WRITE_FD.store(fds[1], Ordering::SeqCst);

    let old_int = match install_handler(libc::SIGINT) {
      Ok(old) => old,
      Err(err) => {
        SHUTDOWN_WRITE_FD.store(-1, Ordering::SeqCst);
        close_fd(fds[0]);
        close_fd(fds[1]);
        return Err(err);
      },
    };
    let old_term = match install_handler(libc::SIGTERM) {
      Ok(old) => old,
      Err(err) => {
        restore_handler(libc::SIGINT, &old_int);
        SHUTDOWN_WRITE_FD.store(-1, Ordering::SeqCst);
        close_fd(fds[0]);
        close_fd(fds[1]);
        return Err(err);
      },
    };

    Ok(Self {
      read_fd: fds[0],
      write_fd: fds[1],
      old_int,
      old_term,
    })
  }

  /// Readable end of the self-pipe.
  ///
  /// # Returns
  ///
  /// The file descriptor for the readable end, suitable for polling alongside
  /// a listener.
  #[must_use]
  pub fn read_fd(&self) -> RawFd {
    self.read_fd
  }

  /// Whether shutdown has been requested.
  ///
  /// # Returns
  ///
  /// `true` once a `SIGINT` or `SIGTERM` has been observed.
  #[must_use]
  pub fn shutdown_requested(&self) -> bool {
    shutdown_requested()
  }

  /// Drains any pending wake-up bytes from the pipe.
  pub fn drain(&self) {
    let mut buf = [0_u8; 64];
    loop {
      // SAFETY: `buf` is a valid writable buffer; the read end is a nonblocking
      // pipe, so this returns rather than blocking once drained.
      let n = unsafe { libc::read(self.read_fd, buf.as_mut_ptr().cast(), buf.len()) };
      if n <= 0 {
        break;
      }
    }
  }
}

impl Drop for SignalPipe {
  fn drop(&mut self) {
    restore_handler(libc::SIGINT, &self.old_int);
    restore_handler(libc::SIGTERM, &self.old_term);
    SHUTDOWN_WRITE_FD.store(-1, Ordering::SeqCst);
    close_fd(self.read_fd);
    close_fd(self.write_fd);
  }
}

fn configure(read_fd: RawFd, write_fd: RawFd) -> Result<()> {
  set_cloexec(read_fd)?;
  set_cloexec(write_fd)?;
  set_nonblocking(read_fd)?;
  set_nonblocking(write_fd)?;
  Ok(())
}

fn set_cloexec(fd: RawFd) -> Result<()> {
  // SAFETY: reads descriptor flags for a valid open fd.
  let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
  if flags < 0 {
    return Err(io("reading fd flags")(std::io::Error::last_os_error()));
  }
  // SAFETY: writes descriptor flags for the same valid open fd.
  if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
    return Err(io("setting fd cloexec")(std::io::Error::last_os_error()));
  }
  Ok(())
}

fn set_nonblocking(fd: RawFd) -> Result<()> {
  // SAFETY: reads status flags for a valid open fd.
  let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
  if flags < 0 {
    return Err(io("reading fd status")(std::io::Error::last_os_error()));
  }
  // SAFETY: writes status flags for the same valid open fd.
  if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
    return Err(io("setting fd nonblocking")(std::io::Error::last_os_error()));
  }
  Ok(())
}

fn install_handler(signal: libc::c_int) -> Result<libc::sigaction> {
  // SAFETY: an all-zero `sigaction` is a valid starting point before its fields
  // are populated below.
  let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
  action.sa_sigaction = handle_signal as *const () as usize;
  action.sa_flags = 0;
  // SAFETY: initializes the signal mask owned by `action`.
  if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } < 0 {
    return Err(io("initializing signal mask")(
      std::io::Error::last_os_error(),
    ));
  }

  // SAFETY: provides zeroed storage for the previous action returned below.
  let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
  // SAFETY: installs an async-signal-safe handler for a real signal number.
  if unsafe { libc::sigaction(signal, &raw const action, &raw mut old) } < 0 {
    return Err(io("installing signal handler")(
      std::io::Error::last_os_error(),
    ));
  }
  Ok(old)
}

fn restore_handler(signal: libc::c_int, action: &libc::sigaction) {
  // SAFETY: restores an action previously captured from `sigaction`.
  unsafe {
    libc::sigaction(signal, action, std::ptr::null_mut());
  }
}

extern "C" fn handle_signal(_signal: libc::c_int) {
  SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
  let fd = SHUTDOWN_WRITE_FD.load(Ordering::SeqCst);
  if fd >= 0 {
    let byte = [1_u8];
    // SAFETY: `write` is async-signal-safe; `fd` is a nonblocking pipe write
    // end while handlers are installed. The result is intentionally
    // ignored.
    unsafe {
      libc::write(fd, byte.as_ptr().cast(), byte.len());
    }
  }
}

fn close_fd(fd: RawFd) {
  // SAFETY: best-effort close of a pipe fd owned by this module.
  unsafe {
    libc::close(fd);
  }
}
