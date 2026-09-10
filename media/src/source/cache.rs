//! Single-window read cache (~8 MiB), mediabunny `BlobSource` default size.

use std::sync::Mutex;

use crate::error::MediaError;

use super::{MediaSource, ReadFuture};

/// Default cache capacity (mediabunny `BlobSource` default).
pub const DEFAULT_CACHE_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, Default)]
struct Window {
    /// File-absolute start of cached bytes.
    offset: u64,
    bytes: Vec<u8>,
}

impl Window {
    fn contains(&self, offset: u64, len: usize) -> bool {
        if self.bytes.is_empty() {
            return false;
        }
        let end = offset.saturating_add(len as u64);
        let win_end = self.offset.saturating_add(self.bytes.len() as u64);
        offset >= self.offset && end <= win_end
    }

    fn slice(&self, offset: u64, len: usize) -> Vec<u8> {
        let start = (offset - self.offset) as usize;
        self.bytes[start..start + len].to_vec()
    }
}

/// Wraps an inner [`MediaSource`] with a single contiguous cache window.
///
/// Requests larger than `max_cache_size` bypass the cache. Smaller misses replace
/// the window with a fill starting at `offset` (capped by EOF and `max_cache_size`).
pub struct CachedSource<S> {
    inner: S,
    max_cache_size: usize,
    window: Mutex<Window>,
}

impl<S> CachedSource<S> {
    pub fn new(inner: S) -> Self {
        Self::with_capacity(inner, DEFAULT_CACHE_SIZE)
    }

    pub fn with_capacity(inner: S, max_cache_size: usize) -> Self {
        Self {
            inner,
            max_cache_size: max_cache_size.max(1),
            window: Mutex::new(Window::default()),
        }
    }

    pub fn into_inner(self) -> S {
        self.inner
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn max_cache_size(&self) -> usize {
        self.max_cache_size
    }
}

impl<S: MediaSource> MediaSource for CachedSource<S> {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        Box::pin(async move {
            let size = self.inner.size();
            if offset > size {
                return Err(MediaError::ReadOutOfRange {
                    offset,
                    len,
                    size,
                });
            }

            let available = (size - offset) as usize;
            let want = len.min(available);
            if want == 0 {
                return Ok(Vec::new());
            }

            // Bypass for oversized requests — do not thrash / truncate the window.
            if want > self.max_cache_size {
                return self.inner.read(offset, want).await;
            }

            {
                let window = self
                    .window
                    .lock()
                    .map_err(|_| MediaError::Source("cache lock poisoned".into()))?;
                if window.contains(offset, want) {
                    return Ok(window.slice(offset, want));
                }
            }

            // ISOBMFF headers are ≤16 bytes — never prefetch on those, or an
            // `mdat` header touch pulls multi‑MiB of media into WASM.
            const NO_PREFETCH_MAX: usize = 16;
            let fill_len = if want <= NO_PREFETCH_MAX {
                want
            } else {
                want.max(self.max_cache_size.min(available))
            };
            let filled = self.inner.read(offset, fill_len).await?;

            let out = filled.get(..want).unwrap_or(&filled).to_vec();

            let mut window = self
                .window
                .lock()
                .map_err(|_| MediaError::Source("cache lock poisoned".into()))?;
            *window = Window {
                offset,
                bytes: filled,
            };

            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::source::MemorySource;

    #[derive(Clone)]
    struct CountingSource {
        inner: MemorySource,
        reads: Arc<AtomicUsize>,
        bytes_fetched: Arc<AtomicUsize>,
    }

    impl MediaSource for CountingSource {
        fn size(&self) -> u64 {
            self.inner.size()
        }

        fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let inner = self.inner.read(offset, len);
            let bytes_fetched = Arc::clone(&self.bytes_fetched);
            Box::pin(async move {
                let data = inner.await?;
                bytes_fetched.fetch_add(data.len(), Ordering::SeqCst);
                Ok(data)
            })
        }
    }

    fn poll_ready<S: MediaSource>(src: &S, offset: u64, len: usize) -> Result<Vec<u8>, MediaError> {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut fut = src.read(offset, len);
        for _ in 0..8 {
            match std::future::Future::poll(std::pin::Pin::new(&mut fut), &mut cx) {
                std::task::Poll::Ready(r) => return r,
                std::task::Poll::Pending => continue,
            }
        }
        panic!("source read stayed pending");
    }

    #[test]
    fn cache_hit_avoids_inner_read() {
        let data: Vec<u8> = (0u8..64).collect();
        let counting = CountingSource {
            inner: MemorySource::from_vec(data.clone()),
            reads: Arc::new(AtomicUsize::new(0)),
            bytes_fetched: Arc::new(AtomicUsize::new(0)),
        };
        let cached = CachedSource::with_capacity(counting.clone(), 32);

        let a = poll_ready(&cached, 0, 20).unwrap();
        assert_eq!(a, &data[0..20]);
        assert_eq!(counting.reads.load(Ordering::SeqCst), 1);
        assert_eq!(counting.bytes_fetched.load(Ordering::SeqCst), 32);

        let b = poll_ready(&cached, 20, 8).unwrap();
        assert_eq!(b, &data[20..28]);
        assert_eq!(counting.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn oversized_read_bypasses_cache() {
        let data: Vec<u8> = (0u8..64).collect();
        let counting = CountingSource {
            inner: MemorySource::from_vec(data.clone()),
            reads: Arc::new(AtomicUsize::new(0)),
            bytes_fetched: Arc::new(AtomicUsize::new(0)),
        };
        let cached = CachedSource::with_capacity(counting.clone(), 16);

        let big = poll_ready(&cached, 0, 32).unwrap();
        assert_eq!(big, &data[0..32]);
        assert_eq!(counting.reads.load(Ordering::SeqCst), 1);
        assert_eq!(counting.bytes_fetched.load(Ordering::SeqCst), 32);

        // Window still empty — small read must fetch again.
        let _ = poll_ready(&cached, 0, 4).unwrap();
        assert_eq!(counting.reads.load(Ordering::SeqCst), 2);
    }
}
