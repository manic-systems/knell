//! Newline-delimited framing with a size ceiling.

use std::io::{
  self,
  BufRead,
};

/// Default frame-size ceiling (16 MiB) for [`FrameReader`].
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Reads bytes up to and including the next newline, failing if the line would
/// exceed `limit` bytes.
///
/// The limit is checked incrementally, so an oversized line is rejected without
/// buffering it in full.
///
/// # Returns
///
/// The bytes read, including the trailing `\n` when present. An empty vector is
/// returned at end of input.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidData`] if the line exceeds `limit` bytes, or
/// any error produced by the underlying reader.
#[inline]
pub fn read_limited_line<R>(reader: &mut R, limit: usize) -> io::Result<Vec<u8>>
where
  R: BufRead,
{
  let mut out = Vec::new();

  loop {
    let available = reader.fill_buf()?;
    if available.is_empty() {
      return Ok(out);
    }

    let newline = available.iter().position(|byte| *byte == b'\n');
    let len = newline.map_or(available.len(), |index| index + 1);
    if out.len() + len > limit {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("frame exceeds {limit} bytes"),
      ));
    }

    out.extend_from_slice(available.get(..len).unwrap_or(available));
    reader.consume(len);
    if newline.is_some() {
      return Ok(out);
    }
  }
}

/// A newline-framed reader that rejects frames larger than a configured
/// maximum and strips the trailing `\n` (and `\r`, if present) from each frame.
pub struct FrameReader<R> {
  /// The wrapped reader.
  inner: R,
  /// Largest frame accepted, in bytes.
  max:   usize,
}

impl<R> FrameReader<R>
where
  R: BufRead,
{
  /// Wraps `inner`, rejecting any frame larger than `max` bytes.
  #[must_use]
  #[inline]
  pub const fn new(inner: R, max: usize) -> Self {
    Self { inner, max }
  }

  /// Reads the next frame.
  ///
  /// # Returns
  ///
  /// `Some(bytes)` with the terminating `\n` (and a preceding `\r`, if any)
  /// removed, or `None` at end of input.
  ///
  /// # Errors
  ///
  /// Returns [`io::ErrorKind::InvalidData`] if the frame exceeds the
  /// configured maximum, or any error produced by the underlying reader.
  #[inline]
  pub fn read_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = read_limited_line(&mut self.inner, self.max)?;
    if bytes.is_empty() {
      return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
      bytes.pop();
      if bytes.last() == Some(&b'\r') {
        bytes.pop();
      }
    }
    Ok(Some(bytes))
  }

  /// Unwraps the reader, returning the inner value.
  #[must_use]
  #[inline]
  pub fn into_inner(self) -> R {
    self.inner
  }
}

#[cfg(test)]
mod tests {
  use std::io::{
    self,
    Cursor,
  };

  use crate::framed::{
    FrameReader,
    read_limited_line,
  };

  #[test]
  fn reads_single_line() {
    let mut reader = FrameReader::new(Cursor::new(b"hello\nworld\n".to_vec()), 64);
    assert_eq!(reader.read_frame().unwrap().as_deref(), Some(&b"hello"[..]));
    assert_eq!(reader.read_frame().unwrap().as_deref(), Some(&b"world"[..]));
    assert_eq!(reader.read_frame().unwrap(), None);
  }

  #[test]
  fn strips_crlf() {
    let mut reader = FrameReader::new(Cursor::new(b"hi\r\n".to_vec()), 64);
    assert_eq!(reader.read_frame().unwrap().as_deref(), Some(&b"hi"[..]));
  }

  #[test]
  fn rejects_oversized_frame() {
    let mut cursor = Cursor::new(vec![b'x'; 128]);
    let err = read_limited_line(&mut cursor, 16).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
  }

  #[test]
  fn rejects_oversized_frame_via_reader() {
    let mut reader = FrameReader::new(Cursor::new(vec![b'x'; 128]), 16);
    reader.read_frame().unwrap_err();
  }
}
