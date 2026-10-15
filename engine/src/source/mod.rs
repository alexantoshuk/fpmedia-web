//! Ranged media byte sources (mediabunny `Source` / `BlobSource` / `UrlSource`).
//!
//! Progressive open reads only the ranges it needs (`moov`, then samples).
//! Native `FileSource` is deferred — see `.ai/state.md`.

mod cache;
mod fetch_map;
mod memory;
mod orchestrator;
mod url_gate;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod blob;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod url;

pub use cache::{CachedSource, DEFAULT_CACHE_BUDGET, DEFAULT_CACHE_SIZE, LOCAL_CACHE_BUDGET};
pub use fetch_map::FetchMap;
pub use memory::MemorySource;
pub use url_gate::{DEFAULT_URL_PARALLELISM, GATE_LARGE_BYTES, OPEN_URL_PARALLELISM};

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use blob::BlobSource;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use url::{UrlSource, UrlSourceOptions, is_no_http_range};

use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;

use crate::error::MediaError;

/// Async future returned by [`MediaSource::read`].
///
/// [`Bytes`] so [`CachedSource`] hits share the window allocation with the
/// decoder (no per-NAL `to_vec`). mediabunny keeps packet bytes in JS
/// `Uint8Array` from `Blob.slice` — we still cross into WASM once on fill,
/// then share.
pub type ReadFuture<'a> = Pin<Box<dyn Future<Output = Result<Bytes, MediaError>> + 'a>>;

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

    /// Cancel in-flight IO / workers (mediabunny `Source` / `Input.dispose`).
    ///
    /// Default no-op. [`UrlSource`] aborts Range workers immediately so a clip
    /// switch does not leave zombie fetches while `Shared` bg tasks still hold
    /// Arcs. Idempotent.
    fn dispose(&self) {}

    /// True when sample-miss blocking reads should floor at
    /// mediabunny `URL_SOURCE_MIN_LOAD_AMOUNT` (0.5 MiB) to amortize HTTP Range RTT.
    ///
    /// [`UrlSource`] → true. [`BlobSource`] / [`MemorySource`] → false (local
    /// `slice` has no RTT; awaiting 0.5 MiB per NAL burns the main thread).
    /// mediabunny: min-load is a UrlSource concern; BlobSource is a local window.
    fn network_min_load(&self) -> bool {
        false
    }
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

    fn dispose(&self) {
        (**self).dispose()
    }

    fn network_min_load(&self) -> bool {
        (**self).network_min_load()
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

    fn dispose(&self) {
        (**self).dispose()
    }

    fn network_min_load(&self) -> bool {
        (**self).network_min_load()
    }
}
