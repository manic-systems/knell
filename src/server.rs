use std::{
  os::{
    fd::AsRawFd as _,
    unix::net::{
      UnixListener,
      UnixStream,
    },
  },
  sync::Arc,
  thread,
};

use crate::{
  connection_limit::ConnectionLimiter,
  error::{
    Result,
    io,
  },
  signals::SignalPipe,
};

/// Ties a [`UnixListener`], a [`SignalPipe`], and a [`ConnectionLimiter`] into
/// a ready-made accept loop so callers do not have to hand-wire the poll.
///
/// [`run`](Server::run) polls the listener and the signal pipe together; each
/// accepted connection runs the handler on its own thread while holding a
/// connection slot, and the loop returns once a shutdown signal arrives.
/// Connections accepted while at the concurrency cap are dropped immediately.
pub struct Server {
  listener: UnixListener,
  signals:  SignalPipe,
  limiter:  Arc<ConnectionLimiter>,
}

impl Server {
  /// Creates a server over `listener`, waking on `signals`, admitting at most
  /// `max_connections` concurrent handlers.
  #[must_use]
  pub fn new(listener: UnixListener, signals: SignalPipe, max_connections: usize) -> Self {
    Self {
      listener,
      signals,
      limiter: Arc::new(ConnectionLimiter::new(max_connections)),
    }
  }

  /// Underlying listener.
  ///
  /// # Returns
  ///
  /// A reference to the [`UnixListener`] this server accepts connections on.
  #[must_use]
  pub fn listener(&self) -> &UnixListener {
    &self.listener
  }

  /// Installed signal pipe.
  ///
  /// # Returns
  ///
  /// A reference to the [`SignalPipe`] the server wakes on.
  #[must_use]
  pub fn signals(&self) -> &SignalPipe {
    &self.signals
  }

  /// Shared connection limiter.
  ///
  /// # Returns
  ///
  /// A reference to the [`ConnectionLimiter`] shared by this server.
  #[must_use]
  pub fn limiter(&self) -> &Arc<ConnectionLimiter> {
    &self.limiter
  }

  /// Runs the accept loop until a `SIGINT`/`SIGTERM` arrives, then returns.
  ///
  /// `handler` is invoked once per accepted connection on a dedicated thread;
  /// the connection slot is released when it returns. Handler threads are
  /// detached and may still be running when this call returns.
  ///
  /// # Errors
  ///
  /// Returns an [`Error::Io`](crate::Error::Io) if the listener cannot be set
  /// non-blocking or if polling fails for a reason other than an interrupt.
  pub fn run<H>(self, handler: H) -> Result<()>
  where
    H: Fn(UnixStream) + Send + Sync + 'static,
  {
    self
      .listener
      .set_nonblocking(true)
      .map_err(io("setting listener non-blocking"))?;

    let handler = Arc::new(handler);
    let listener_fd = self.listener.as_raw_fd();
    let signal_fd = self.signals.read_fd();

    while !self.signals.shutdown_requested() {
      let mut fds = [
        libc::pollfd {
          fd:      listener_fd,
          events:  libc::POLLIN,
          revents: 0,
        },
        libc::pollfd {
          fd:      signal_fd,
          events:  libc::POLLIN,
          revents: 0,
        },
      ];

      // SAFETY: `fds` is a valid, writable array of two `pollfd`s; -1 blocks
      // until a descriptor is ready.
      let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
      if rc < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
          continue;
        }
        return Err(io("polling listener")(err));
      }

      if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
        self.signals.drain();
        break;
      }

      if fds[0].revents & libc::POLLIN == 0 {
        continue;
      }

      self.accept_ready(&handler);
    }

    Ok(())
  }

  fn accept_ready<H>(&self, handler: &Arc<H>)
  where
    H: Fn(UnixStream) + Send + Sync + 'static,
  {
    loop {
      match self.listener.accept() {
        Ok((stream, _)) => {
          let Some(slot) = self.limiter.acquire() else {
            drop(stream);
            continue;
          };
          let handler = Arc::clone(handler);
          let _ = thread::Builder::new().spawn(move || {
            let _slot = slot;
            handler(stream);
          });
        },
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {},
        Err(_) => break,
      }
    }
  }
}
