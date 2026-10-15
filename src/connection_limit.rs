use std::sync::{Arc, Mutex};

/// Caps the number of concurrently held [`ConnectionSlot`]s.
///
/// Wrap it in an [`Arc`] and call [`acquire`](ConnectionLimiter::acquire) per
/// connection; the returned slot releases its count when dropped.
pub struct ConnectionLimiter {
  active: Mutex<usize>,
  max:    usize,
}

impl ConnectionLimiter {
  /// Creates a new limiter that permits at most `max` concurrent slots.
  #[must_use]
  pub fn new(max: usize) -> Self {
    Self {
      active: Mutex::new(0),
      max,
    }
  }

  /// Number of slots currently held.
  ///
  /// # Returns
  ///
  /// The number of [`ConnectionSlot`]s currently held.
  ///
  /// # Panics
  ///
  /// Panics if the internal mutex was poisoned by a thread that panicked while
  /// holding it.
  #[must_use]
  pub fn active(&self) -> usize {
    *self.active.lock().expect("connection limit poisoned")
  }

  /// Acquires a slot.
  ///
  /// # Returns
  ///
  /// `Some(slot)` when a slot was available, or `None` when all `max` slots are
  /// already held.
  ///
  /// # Panics
  ///
  /// Panics if the internal mutex was poisoned by a thread that panicked while
  /// holding it.
  pub fn acquire(self: &Arc<Self>) -> Option<ConnectionSlot> {
    let mut active = self.active.lock().expect("connection limit poisoned");
    if *active >= self.max {
      return None;
    }
    *active += 1;
    Some(ConnectionSlot {
      limiter: Arc::clone(self),
    })
  }
}

/// A held connection slot; releases its count back to the
/// [`ConnectionLimiter`] when dropped.
pub struct ConnectionSlot {
  limiter: Arc<ConnectionLimiter>,
}

impl Drop for ConnectionSlot {
  fn drop(&mut self) {
    let mut active = self
      .limiter
      .active
      .lock()
      .expect("connection limit poisoned");
    *active -= 1;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn counts_and_caps() {
    let limiter = Arc::new(ConnectionLimiter::new(2));
    let a = limiter.acquire().expect("first slot");
    let b = limiter.acquire().expect("second slot");
    assert_eq!(limiter.active(), 2);
    assert!(limiter.acquire().is_none());

    drop(a);
    assert_eq!(limiter.active(), 1);
    let _c = limiter.acquire().expect("slot freed after drop");
    assert_eq!(limiter.active(), 2);
    drop(b);
    assert_eq!(limiter.active(), 1);
  }
}
