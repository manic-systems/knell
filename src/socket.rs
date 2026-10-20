//! Unix-socket binding and peer authorization.
//!
//! [`bind_unix`] binds a listener with a restrictive umask and an explicit
//! mode, reclaiming a stale socket while refusing to clobber a live one.
//! [`peer_uid`] and [`require_uid`] read the connecting peer's uid over
//! `SO_PEERCRED` (Linux only).

use std::{
  fs::{
    self,
    Permissions,
  },
  os::unix::{
    fs::{
      FileTypeExt as _,
      PermissionsExt as _,
    },
    net::{
      UnixListener,
      UnixStream,
    },
  },
  path::Path,
};

use crate::error::{
  Error,
  Result,
  io,
};

struct UmaskGuard(libc::mode_t);

impl UmaskGuard {
  fn set(mask: libc::mode_t) -> Self {
    // SAFETY: binding runs during single-threaded daemon startup, so no other
    // thread observes the transient umask change.
    let previous = unsafe { libc::umask(mask) };
    Self(previous)
  }
}

impl Drop for UmaskGuard {
  fn drop(&mut self) {
    // SAFETY: restores the umask captured in `set`.
    unsafe {
      libc::umask(self.0);
    }
  }
}

/// Binds a [`UnixListener`] at `path` with permission bits `mode`.
///
/// Creates the parent directory if needed, binds under `umask 0o077`, then sets
/// the socket to exactly `mode` (e.g. `0o600`). If `path` already exists it is
/// probed: a live socket is refused, a dead one is reclaimed, and a non-socket
/// path is left untouched.
///
/// # Errors
///
/// Returns [`Error::SocketInUse`] if a live daemon owns the socket,
/// [`Error::NotASocket`] if the path exists but is not a socket, or an
/// [`Error::Io`] for any failing filesystem or bind operation.
pub fn bind_unix(path: impl AsRef<Path>, mode: u32) -> Result<UnixListener> {
  let path = path.as_ref();
  prepare_socket_path(path)?;

  let listener = {
    let _umask = UmaskGuard::set(0o077);
    UnixListener::bind(path).map_err(io(format!("binding {}", path.display())))?
  };

  if let Err(err) = fs::set_permissions(path, Permissions::from_mode(mode))
    .map_err(io(format!("setting permissions on {}", path.display())))
  {
    let _ = fs::remove_file(path);
    return Err(err);
  }

  Ok(listener)
}

fn prepare_socket_path(path: &Path) -> Result<()> {
  if let Some(parent) = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
  {
    fs::create_dir_all(parent).map_err(io(format!(
      "creating socket directory {}",
      parent.display()
    )))?;
  }

  let metadata = match fs::symlink_metadata(path) {
    Ok(metadata) => metadata,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    Err(err) => {
      return Err(io(format!(
        "checking existing socket path {}",
        path.display()
      ))(err));
    },
  };

  if !metadata.file_type().is_socket() {
    return Err(Error::NotASocket(path.to_path_buf()));
  }

  match UnixStream::connect(path) {
    Ok(_) => return Err(Error::SocketInUse(path.to_path_buf())),
    Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {},
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    Err(err) => {
      return Err(io(format!("probing existing socket {}", path.display()))(
        err,
      ));
    },
  }

  fs::remove_file(path).map_err(io(format!("removing stale socket {}", path.display())))?;
  Ok(())
}

/// Looks up the peer's uid for `stream` using `SO_PEERCRED`.
///
/// # Returns
///
/// The uid of the peer process connected to `stream`.
///
/// # Errors
///
/// Returns the underlying `getsockopt` error if the lookup fails.
///
/// # Panics
///
/// Panics only if `size_of::<libc::ucred>()` does not fit in a
/// `socklen_t`, which cannot happen on any supported platform.
#[cfg(target_os = "linux")]
pub fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
  use std::os::fd::AsRawFd as _;

  let mut cred = libc::ucred {
    pid: 0,
    uid: 0,
    gid: 0,
  };
  let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
    .expect("ucred size fits socklen_t");
  // SAFETY: `cred` and `len` are valid out-parameters sized for `ucred`; the fd
  // is owned by the borrowed stream for the duration of the call.
  let rc = unsafe {
    libc::getsockopt(
      stream.as_raw_fd(),
      libc::SOL_SOCKET,
      libc::SO_PEERCRED,
      std::ptr::addr_of_mut!(cred).cast(),
      &raw mut len,
    )
  };
  if rc != 0 {
    return Err(std::io::Error::last_os_error());
  }
  Ok(cred.uid)
}

/// Looks up the peer's uid for `stream`.
///
/// # Errors
///
/// Always returns [`std::io::ErrorKind::Unsupported`], because `SO_PEERCRED` is
/// only available on Linux.
#[cfg(not(target_os = "linux"))]
pub fn peer_uid(_stream: &UnixStream) -> std::io::Result<u32> {
  Err(std::io::Error::new(
    std::io::ErrorKind::Unsupported,
    "SO_PEERCRED is only available on Linux",
  ))
}

/// Checks that the peer connected to `stream` has uid `expected`.
///
/// # Errors
///
/// Returns [`Error::Unauthorized`] if the peer's uid differs from `expected`;
/// [`Error::Unsupported`] on non-Linux targets; or [`Error::Io`] if the peer
/// credential lookup fails.
pub fn require_uid(stream: &UnixStream, expected: u32) -> Result<()> {
  let peer = peer_uid(stream).map_err(|err| {
    if err.kind() == std::io::ErrorKind::Unsupported {
      Error::Unsupported
    } else {
      io("reading peer credentials")(err)
    }
  })?;
  if peer != expected {
    return Err(Error::Unauthorized { peer, expected });
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reclaims_stale_socket() {
    let dir = std::env::temp_dir().join(format!("knell-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stale.sock");
    let _ = fs::remove_file(&path);

    let first = bind_unix(&path, 0o600).expect("first bind");
    drop(first);
    assert!(path.exists());

    let second = bind_unix(&path, 0o600).expect("rebind over stale socket");
    let meta = fs::metadata(&path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    drop(second);

    let _ = fs::remove_file(&path);
    let _ = fs::remove_dir(&dir);
  }

  #[test]
  fn refuses_live_socket() {
    let dir = std::env::temp_dir().join(format!("knell-live-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("live.sock");
    let _ = fs::remove_file(&path);

    let _listener = bind_unix(&path, 0o600).expect("bind");
    match bind_unix(&path, 0o600) {
      Err(Error::SocketInUse(_)) => {},
      other => panic!("expected SocketInUse, got {other:?}"),
    }

    let _ = fs::remove_file(&path);
    let _ = fs::remove_dir(&dir);
  }

  #[test]
  fn refuses_non_socket_path() {
    let dir = std::env::temp_dir().join(format!("knell-reg-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("not.sock");
    fs::write(&path, b"regular file").unwrap();

    match bind_unix(&path, 0o600) {
      Err(Error::NotASocket(_)) => {},
      other => panic!("expected NotASocket, got {other:?}"),
    }

    let _ = fs::remove_file(&path);
    let _ = fs::remove_dir(&dir);
  }
}
