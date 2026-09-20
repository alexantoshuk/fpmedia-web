//! [`MediaSource`] wrapper that records every successful read into a [`FetchMap`].

use std::sync::Arc;

use super::fetch_map::FetchMap;
use super::{MediaSource, ReadFuture};

/// Forwards reads to `inner` and unions returned bytes into [`FetchMap`].
pub struct TrackingSource<S> {
    inner: S,
    map: Arc<FetchMap>,
}

impl<S> TrackingSource<S> {
    pub fn new(inner: S, map: Arc<FetchMap>) -> Self {
        Self { inner, map }
    }

    pub fn fetch_map(&self) -> &Arc<FetchMap> {
        &self.map
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: MediaSource> MediaSource for TrackingSource<S> {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        Box::pin(async move {
            let data = self.inner.read(offset, len).await?;
            self.map.record(offset, data.len());
            Ok(data)
        })
    }
}
