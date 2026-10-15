//! Browser [`web_sys::Blob`] / `File` source — mediabunny WebKit-safe path.
//!
//! mediabunny `BlobSource`: prefer `blob.slice(start,end).arrayBuffer()` when
//! `blob.stream()` is unreliable (Safari/iOS). Same approach here.

use bytes::Bytes;
use js_sys::Uint8Array;
use wasm_bindgen_futures::JsFuture;
use web_sys::Blob;

use crate::error::MediaError;

use super::{MediaSource, ReadFuture};

/// Ranged reads from a browser [`Blob`] (including `File`).
#[derive(Clone, Debug)]
pub struct BlobSource {
    blob: Blob,
}

impl BlobSource {
    pub fn new(blob: Blob) -> Self {
        Self { blob }
    }

    pub fn blob(&self) -> &Blob {
        &self.blob
    }
}

impl MediaSource for BlobSource {
    fn size(&self) -> u64 {
        self.blob.size() as u64
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        let blob = self.blob.clone();
        Box::pin(async move {
            let size = blob.size() as u64;
            if offset > size {
                return Err(MediaError::ReadOutOfRange { offset, len, size });
            }
            let available = (size - offset) as usize;
            let want = len.min(available);
            if want == 0 {
                return Ok(Bytes::new());
            }

            // mediabunny: `await blob.slice(start, end).arrayBuffer()`
            // Do not console.log every read — 8 MiB play prefetches flood the
            // main thread and drop paint FPS on large local masters.
            let end = offset + want as u64;
            let slice = blob
                .slice_with_f64_and_f64(offset as f64, end as f64)
                .map_err(MediaError::from)?;
            let buffer = JsFuture::from(slice.array_buffer())
                .await
                .map_err(MediaError::from)?;
            let array = Uint8Array::new(&buffer);
            // One copy into WASM; [`CachedSource`] + [`bytes::Bytes`] share from here.
            let mut v = vec![0u8; array.length() as usize];
            array.copy_to(&mut v);
            Ok(Bytes::from(v))
        })
    }
}
