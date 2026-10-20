//! The crate-wide [`Error`] and [`Result`] types.

use std::{
  io,
  path::PathBuf,
  result,
};

/// A [`Result`](result::Result) whose error is this crate's [`Error`].
pub type Result<T> = result::Result<T, Error>;

/// Everything that can go wrong inside `knell`.
#[non_exhaustive]
#[derive(Debug, flub::Error)]
pub enum Error {
  /// A system call or filesystem operation failed; `context` names what was
  /// being attempted.
  #[error("{context}")]
  Io {
    /// Human-readable description of the operation that failed.
    context: String,
    /// The underlying I/O error.
    #[source]
    source:  io::Error,
  },
  /// The socket path exists but is not a socket, so it was left untouched.
  #[error("refusing to remove non-socket path {0}")]
  NotASocket(PathBuf),
  /// A live daemon is already listening on the socket path.
  #[error("a live daemon already owns the socket at {0}")]
  SocketInUse(PathBuf),
  /// The connecting peer's uid did not match the required uid.
  #[error("peer uid {peer} is not authorized; expected {expected}")]
  Unauthorized {
    /// The uid reported by the peer.
    peer:     u32,
    /// The uid that was required.
    expected: u32,
  },
  /// A step of the double-fork daemonization sequence failed.
  #[error("daemonization failed: {0}")]
  Daemonize(String),
  /// Another process already holds the lock on the pidfile.
  #[error("pidfile {0} is already locked by another process")]
  PidFileLocked(PathBuf),
  /// The requested operation is not available on this platform.
  #[error("peer credentials are not available on this platform")]
  Unsupported,
}

/// Builds a mapper that wraps an [`io::Error`] as [`Error::Io`] tagged with
/// `context`.
pub(crate) fn io<C>(context: C) -> impl FnOnce(io::Error) -> Error
where
  C: Into<String>,
{
  let owned = context.into();
  move |source| {
    Error::Io {
      context: owned,
      source,
    }
  }
}
