//! A mutex-counted cap on concurrent connections.

use std::sync::{
  Arc,
  Mutex,
  PoisonError,
};

/// Caps the number of concurrently held [`ConnectionSlot`]s.
///
/// Wrap it in an [`Arc`] and call [`acquire`](ConnectionLimiter::acquire) per
/// connection; the returned slot releases its count when dropped.
pub struct ConnectionLimiter {
  /// Number of slots currently held.
  active: Mutex<usize>,
  /// Most slots that may be held at once.
  max:    usize,
}

impl ConnectionLimiter {
  /// Creates a new limiter that permits at most `max` concurrent slots.
  #[must_use]
  #[inline]
  pub const fn new(max: usize) -> Self {
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
  #[must_use]
  #[inline]
  pub fn active(&self) -> usize {
    *self.active.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Acquires a slot.
  ///
  /// # Returns
  ///
  /// `Some(slot)` when a slot was available, or `None` when all `max` slots
  /// are already held.
  #[inline]
  pub fn acquire(self: &Arc<Self>) -> Option<ConnectionSlot> {
    let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
    if *active >= self.max {
      return None;
    }
    *active += 1;
    drop(active);

    Some(ConnectionSlot {
      limiter: Arc::clone(self),
    })
  }
}

/// A held connection slot; releases its count back to the
/// [`ConnectionLimiter`] when dropped.
pub struct ConnectionSlot {
  /// The limiter this slot is counted against.
  limiter: Arc<ConnectionLimiter>,
}

impl Drop for ConnectionSlot {
  #[inline]
  fn drop(&mut self) {
    let mut active = self
      .limiter
      .active
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    *active -= 1;
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use crate::connection_limit::ConnectionLimiter;

  #[test]
  fn counts_and_caps() {
    let limiter = Arc::new(ConnectionLimiter::new(2));
    let first = limiter.acquire().expect("first slot");
    let second = limiter.acquire().expect("second slot");
    assert_eq!(limiter.active(), 2);
    assert!(limiter.acquire().is_none());

    drop(first);
    assert_eq!(limiter.active(), 1);
    let _third = limiter.acquire().expect("slot freed after drop");
    assert_eq!(limiter.active(), 2);
    drop(second);
    assert_eq!(limiter.active(), 1);
  }
}
