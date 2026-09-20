//! Ranged media byte sources (mediabunny `Source` / `BlobSource` / `UrlSource`).
//!
//! Progressive open reads only the ranges it needs (`moov`, then samples).
//! Native `FileSource` is deferred — see `.ai/state.md`.

mod cache;
mod fetch_map;
mod memory;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod blob;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod url;

pub use cache::{CachedSource, DEFAULT_CACHE_BUDGET, DEFAULT_CACHE_SIZE};
pub use fetch_map::FetchMap;
pub use memory::MemorySource;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use blob::BlobSource;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use url::{UrlSource, is_no_http_range};

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
}

impl<T: MediaSource + ?Sized> MediaSource for &T {
    fn size(&self) -> u64 {
        (**self).size()
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        (**self).read(offset, len)
    }
}

impl<T: MediaSource + ?Sized> MediaSource for std::sync::Arc<T> {
    fn size(&self) -> u64 {
        (**self).size()
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        (**self).read(offset, len)
    }
}
