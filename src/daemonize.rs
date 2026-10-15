use std::{
  env,
  fs::OpenOptions,
  io::{BufRead, BufReader, Write},
  os::{
    fd::{AsRawFd as _, RawFd},
    unix::net::UnixStream,
  },
  path::PathBuf,
  process,
};

use crate::error::{Error, Result, io};

/// Builder for turning the current process into a daemon.
///
/// Configures the double-fork sequence: an optional working-directory change,
/// stdio redirection to `/dev/null`, and an optional readiness pipe that lets
/// the daemon report startup success or failure back to the launching process.
#[derive(Debug, Clone)]
pub struct Daemonize {
  working_directory: Option<PathBuf>,
  redirect_stdio:    bool,
  readiness_pipe:    bool,
}

impl Default for Daemonize {
  fn default() -> Self {
    Self {
      working_directory: Some(PathBuf::from("/")),
      redirect_stdio:    true,
      readiness_pipe:    true,
    }
  }
}

impl Daemonize {
  /// Creates a builder with the default configuration: change to `/`, redirect
  /// stdio to `/dev/null`, and use a readiness pipe.
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Sets the directory the daemon changes into after forking.
  #[must_use]
  pub fn working_directory(mut self, dir: impl Into<PathBuf>) -> Self {
    self.working_directory = Some(dir.into());
    self
  }

  /// Keeps the current working directory instead of changing it.
  #[must_use]
  pub fn keep_working_directory(mut self) -> Self {
    self.working_directory = None;
    self
  }

  /// Controls whether stdin, stdout, and stderr are redirected to `/dev/null`.
  #[must_use]
  pub fn redirect_stdio(mut self, redirect: bool) -> Self {
    self.redirect_stdio = redirect;
    self
  }

  /// Controls whether a readiness pipe is used to report startup status.
  ///
  /// When disabled, the launching process does not wait for readiness and
  /// [`daemonize`](Daemonize::daemonize) returns a no-op [`Readiness`].
  #[must_use]
  pub fn readiness_pipe(mut self, enabled: bool) -> Self {
    self.readiness_pipe = enabled;
    self
  }

  /// Daemonizes the current process via a double fork and `setsid`.
  ///
  /// When the readiness pipe is enabled the original process blocks until the
  /// daemon reports status through the returned [`Readiness`], then exits with
  /// code 0 (ready) or 1 (failed or exited early); only the daemon process
  /// returns from this call.
  ///
  /// # Errors
  ///
  /// Returns [`Error::Daemonize`] if a `fork` or `setsid` fails, or an
  /// [`Error::Io`] if the working-directory change or stdio redirection fails.
  pub fn daemonize(self) -> Result<Readiness> {
    let pipe = if self.readiness_pipe {
      Some(UnixStream::pair().map_err(io("creating readiness pipe"))?)
    } else {
      None
    };

    match pipe {
      Some((reader, writer)) => self.fork_with_pipe(reader, writer),
      None => self.fork_detached(),
    }
  }

  fn fork_with_pipe(
    self,
    reader: UnixStream,
    writer: UnixStream,
  ) -> Result<Readiness> {
    // SAFETY: a single fork of this process; the child inherits both pipe ends.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
      return Err(Error::Daemonize("fork failed".into()));
    }
    if pid > 0 {
      drop(writer);
      wait_for_readiness(reader);
    }

    drop(reader);
    let mut reporter = Readiness { pipe: Some(writer) };

    if let Err(err) = self.detach() {
      reporter.failed(&err.to_string());
      process::exit(1);
    }

    Ok(reporter)
  }

  fn fork_detached(self) -> Result<Readiness> {
    self.detach()?;
    Ok(Readiness { pipe: None })
  }

  fn detach(&self) -> Result<()> {
    // SAFETY: creates a new session for the already-forked child.
    if unsafe { libc::setsid() } < 0 {
      return Err(Error::Daemonize("setsid failed".into()));
    }

    // SAFETY: second fork guarantees the daemon can never reacquire a
    // controlling terminal.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
      return Err(Error::Daemonize("second fork failed".into()));
    }
    if pid > 0 {
      process::exit(0);
    }

    if let Some(dir) = &self.working_directory {
      env::set_current_dir(dir)
        .map_err(io(format!("changing directory to {}", dir.display())))?;
    }

    if self.redirect_stdio {
      redirect_stdio_to_dev_null()?;
    }

    Ok(())
  }
}

/// Handle the daemon uses to report startup status to the launching process.
///
/// The first call writes the status down the readiness pipe; later calls are
/// no-ops. If the pipe was disabled, every method is a no-op.
pub struct Readiness {
  pipe: Option<UnixStream>,
}

impl Readiness {
  /// Reports successful startup; the launching process exits with code 0.
  pub fn ready(&mut self) {
    self.report('1', "");
  }

  /// Reports successful startup and prints `message` from the launching
  /// process before it exits with code 0.
  pub fn ready_with(&mut self, message: &str) {
    self.report('1', message);
  }

  /// Reports a startup failure; the launching process prints `message` to
  /// stderr and exits with code 1.
  pub fn failed(&mut self, message: &str) {
    self.report('0', message);
  }

  fn report(&mut self, tag: char, message: &str) {
    if let Some(mut pipe) = self.pipe.take() {
      let line = format!("{tag}{}\n", message.replace('\n', " "));
      let _ = pipe.write_all(line.as_bytes());
      let _ = pipe.flush();
    }
  }
}

fn wait_for_readiness(reader: UnixStream) -> ! {
  let mut line = String::new();
  match BufReader::new(reader).read_line(&mut line) {
    Ok(0) => {
      eprintln!("daemon exited before reporting readiness");
      process::exit(1);
    },
    Ok(_) => {},
    Err(err) => {
      eprintln!("reading daemon readiness: {err}");
      process::exit(1);
    },
  }

  let trimmed = line.trim_end_matches('\n');
  let (tag, message) = trimmed.split_at(trimmed.len().min(1));
  if tag == "1" {
    if !message.is_empty() {
      println!("{message}");
    }
    process::exit(0);
  }

  if message.is_empty() {
    eprintln!("daemon startup failed");
  } else {
    eprintln!("{message}");
  }
  process::exit(1);
}

fn redirect_stdio_to_dev_null() -> Result<()> {
  let dev_null = OpenOptions::new()
    .read(true)
    .write(true)
    .open("/dev/null")
    .map_err(io("opening /dev/null"))?;
  let source = stable_source_fd(dev_null.as_raw_fd())?;

  for target in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
    // SAFETY: `source` is an open fd for /dev/null and `target` is a standard
    // descriptor owned by this process.
    if unsafe { libc::dup2(source, target) } < 0 {
      let err = std::io::Error::last_os_error();
      if source != dev_null.as_raw_fd() {
        // SAFETY: closes the temporary fd created by `stable_source_fd`.
        unsafe {
          libc::close(source);
        }
      }
      return Err(io("redirecting stdio to /dev/null")(err));
    }
  }

  if source != dev_null.as_raw_fd() {
    // SAFETY: closes the temporary fd created by `stable_source_fd`.
    unsafe {
      libc::close(source);
    }
  }

  Ok(())
}

fn stable_source_fd(fd: RawFd) -> Result<RawFd> {
  if fd > libc::STDERR_FILENO {
    return Ok(fd);
  }

  // SAFETY: duplicates a valid fd to a number above the stdio range so dropping
  // the owning `File` cannot close descriptor 0, 1, or 2.
  let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
  if duplicate < 0 {
    return Err(io("duplicating /dev/null descriptor")(
      std::io::Error::last_os_error(),
    ));
  }
  Ok(duplicate)
}
