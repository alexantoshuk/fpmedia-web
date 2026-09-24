//! Ranged media byte sources (mediabunny `Source` / `BlobSource` / `UrlSource`).
//!
//! Progressive open reads only the ranges it needs (`moov`, then samples).
//! Native `FileSource` is deferred — see `.ai/state.md`.

mod cache;
mod fetch_map;
mod memory;
mod url_gate;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod blob;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod url;

pub use cache::{CachedSource, DEFAULT_CACHE_BUDGET, DEFAULT_CACHE_SIZE};
pub use fetch_map::FetchMap;
pub use memory::MemorySource;
pub use url_gate::{DEFAULT_URL_PARALLELISM, OPEN_URL_PARALLELISM};

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use blob::BlobSource;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use url::{UrlSource, UrlSourceOptions, is_no_http_range};

use std::future::Future;
use std::pin::Pin;

use crate::error::MediaError;

/// Async future returned by [`MediaSource::read`].
pub type ReadFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>, MediaError>> + 'a>>;

/// Random-access byte source (`size` + ranged `read`).
///
/// Implementations must treat `offset` as file-absolute. Reads that extend past
/// EOF return a truncated buffer (may be empty); out-of-range starts error.
pub trait MediaSource: Send + Sync {
    /// Total size in bytes.
    fn size(&self) -> u64;

    /// Read up to `len` bytes starting at `offset`.
    ///
    /// Returns fewer bytes than `len` when hitting EOF. Returns an error when
    /// `offset > size`.
    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_>;

    /// Best-effort background fill of `[offset, offset+len)` (no-op default).
    ///
    /// [`CachedSource`] uses this so fMP4 moof backfill can warm the next `mdat`
    /// before play hits the fragment seam (`mediabunny: URLSource` min-load /
    /// on-demand fragment reads still go through the cache).
    fn prefetch(&self, _offset: u64, _len: usize) {}
}

impl<T: MediaSource + ?Sized> MediaSource for &T {
    fn size(&self) -> u64 {
        (**self).size()
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        (**self).read(offset, len)
    }

    fn prefetch(&self, offset: u64, len: usize) {
        (**self).prefetch(offset, len)
    }
}

impl<T: MediaSource + ?Sized> MediaSource for std::sync::Arc<T> {
    fn size(&self) -> u64 {
        (**self).size()
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        (**self).read(offset, len)
    }

    fn prefetch(&self, offset: u64, len: usize) {
        (**self).prefetch(offset, len)
    }
}
