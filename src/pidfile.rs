use std::{
  fs::{
    File,
    OpenOptions,
  },
  io::Write as _,
  os::fd::AsRawFd as _,
  path::{
    Path,
    PathBuf,
  },
  process,
};

use crate::error::{
  Error,
  Result,
  io,
};

/// A pidfile held under an exclusive advisory lock.
///
/// [`PidFile::create`] opens (or creates) the file, takes a non-blocking
/// `flock(LOCK_EX)`, and writes the current pid. A second daemon that tries to
/// create the same pidfile gets [`Error::PidFileLocked`] instead of silently
/// racing. The lock is released and the file removed on drop.
pub struct PidFile {
  path: PathBuf,
  file: File,
}

impl PidFile {
  /// Creates and locks the pidfile at `path`, writing the current pid.
  ///
  /// The parent directory is created if necessary. The lock is advisory
  /// (`flock`) and released when the returned handle is dropped or the process
  /// exits.
  ///
  /// # Errors
  ///
  /// Returns [`Error::PidFileLocked`] if another process already holds the
  /// lock, or an [`Error::Io`] if the file cannot be created, locked, or
  /// written.
  pub fn create(path: impl AsRef<Path>) -> Result<Self> {
    let path = path.as_ref().to_path_buf();

    if let Some(parent) = path
      .parent()
      .filter(|parent| !parent.as_os_str().is_empty())
    {
      std::fs::create_dir_all(parent).map_err(io(format!(
        "creating pidfile directory {}",
        parent.display()
      )))?;
    }

    let mut file = OpenOptions::new()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(&path)
      .map_err(io(format!("opening pidfile {}", path.display())))?;

    // SAFETY: `flock` on a valid open fd; LOCK_NB makes it return rather than
    // block when another process holds the lock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
      let err = std::io::Error::last_os_error();
      if matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ResourceBusy
      ) {
        return Err(Error::PidFileLocked(path));
      }
      return Err(io(format!("locking pidfile {}", path.display()))(err));
    }

    file
      .set_len(0)
      .map_err(io(format!("truncating pidfile {}", path.display())))?;
    writeln!(file, "{}", process::id())
      .map_err(io(format!("writing pidfile {}", path.display())))?;
    file
      .flush()
      .map_err(io(format!("flushing pidfile {}", path.display())))?;

    Ok(Self { path, file })
  }

  /// Path of this pidfile.
  ///
  /// # Returns
  ///
  /// The filesystem path the pidfile was created at.
  #[must_use]
  pub fn path(&self) -> &Path {
    &self.path
  }
}

impl Drop for PidFile {
  fn drop(&mut self) {
    let _ = self.file.flush();
    let _ = std::fs::remove_file(&self.path);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn writes_pid_and_rejects_second_lock() {
    let dir = std::env::temp_dir().join(format!("knell-pid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("daemon.pid");
    let _ = std::fs::remove_file(&path);

    let pidfile = PidFile::create(&path).expect("create pidfile");
    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents.trim(), process::id().to_string());

    match PidFile::create(&path) {
      Err(Error::PidFileLocked(_)) => {},
      Err(other) => panic!("expected PidFileLocked, got {other:?}"),
      Ok(_) => panic!("expected PidFileLocked, got a second lock"),
    }

    drop(pidfile);
    assert!(!path.exists());

    let _ = std::fs::remove_dir(&dir);
  }
}
