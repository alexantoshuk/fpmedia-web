//! In-memory [`MediaSource`] for fixtures and `open_mp4_bytes`.

use std::sync::Arc;

use crate::error::MediaError;

use super::{MediaSource, ReadFuture};

/// Owns (or shares) the full media bytes; ranged reads are slices.
#[derive(Clone, Debug)]
pub struct MemorySource {
    bytes: Arc<[u8]>,
}

impl MemorySource {
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Arc::from(bytes),
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

impl MediaSource for MemorySource {
    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        Box::pin(async move { read_slice(&self.bytes, offset, len) })
    }
}

fn read_slice(bytes: &[u8], offset: u64, len: usize) -> Result<Vec<u8>, MediaError> {
    let size = bytes.len() as u64;
    if offset > size {
        return Err(MediaError::ReadOutOfRange { offset, len, size });
    }
    let start = offset as usize;
    let end = start.saturating_add(len).min(bytes.len());
    Ok(bytes[start..end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MediaSource;

    #[test]
    fn memory_read_range_and_eof() {
        let src = MemorySource::from_vec(b"abcdefgh".to_vec());
        assert_eq!(src.size(), 8);

        let mid = pollster_read(&src, 2, 3).unwrap();
        assert_eq!(mid, b"cde");

        let tail = pollster_read(&src, 6, 10).unwrap();
        assert_eq!(tail, b"gh");

        let empty = pollster_read(&src, 8, 4).unwrap();
        assert!(empty.is_empty());

        assert!(matches!(
            pollster_read(&src, 9, 1),
            Err(MediaError::ReadOutOfRange { .. })
        ));
    }

    fn pollster_read(src: &MemorySource, offset: u64, len: usize) -> Result<Vec<u8>, MediaError> {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut fut = src.read(offset, len);
        match std::future::Future::poll(std::pin::Pin::new(&mut fut), &mut cx) {
            std::task::Poll::Ready(r) => r,
            std::task::Poll::Pending => panic!("MemorySource::read must be ready"),
        }
    }
}
