//! Dense GOP-chunked `VideoFrame` cache + sparse thumb cache shell.
//!
//! Exact play/PREV frames stay full-res. Thumbs (caller-sized bitmaps) are a
//! separate LRU so filmstrip work cannot evict the playhead window.

use std::collections::HashMap;
use std::num::NonZeroUsize;

use lru::LruCache;
use web_sys::VideoFrame;

use crate::types::FrameId;

/// Default: a few GOPs around the playhead.
pub const DEFAULT_MAX_CHUNKS: usize = 4;
/// Soft cap on total cached `VideoFrame`s across chunks.
pub const DEFAULT_MAX_FRAMES: usize = 96;
/// Scrub/filmstrip keyframe GOPs retained.
pub const DEFAULT_MAX_THUMB_GOPS: usize = 8;
pub const DEFAULT_MAX_THUMBS: usize = 64;

struct FrameChunk {
    frames: HashMap<FrameId, VideoFrame>,
}

impl Drop for FrameChunk {
    fn drop(&mut self) {
        for (_, frame) in self.frames.drain() {
            frame.close();
        }
    }
}

/// GOP-aligned exact-frame cache (`lru` on chunk keys = sync `FrameId`).
pub struct DenseFrameCache {
    chunks: LruCache<FrameId, FrameChunk>,
    max_frames: usize,
    playhead: FrameId,
    playhead_key: FrameId,
}

impl DenseFrameCache {
    pub fn new(max_chunks: usize, max_frames: usize) -> Self {
        let cap = NonZeroUsize::new(max_chunks.max(1)).expect("max_chunks >= 1");
        Self {
            chunks: LruCache::new(cap),
            max_frames: max_frames.max(1),
            playhead: 0,
            playhead_key: 0,
        }
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn frame_count(&self) -> usize {
        self.chunks.iter().map(|(_, c)| c.frames.len()).sum()
    }

    pub fn touch_playhead(&mut self, frame: FrameId, key: FrameId) {
        self.playhead = frame;
        self.playhead_key = key;
        let _ = self.chunks.get(&key);
    }

    /// Clone a cached frame (PREV/NEXT re-show). Promotes the chunk.
    pub fn get_clone(&mut self, frame: FrameId, key: FrameId) -> Option<VideoFrame> {
        let chunk = self.chunks.get(&key)?;
        chunk.frames.get(&frame)?.clone().ok()
    }

    /// Move ownership of a presented/decoded frame into its GOP chunk.
    pub fn insert_owned(&mut self, frame: FrameId, key: FrameId, video: VideoFrame) {
        if !self.chunks.contains(&key) {
            self.evict_until_room_for_chunk();
            self.chunks.put(
                key,
                FrameChunk {
                    frames: HashMap::new(),
                },
            );
        }
        if let Some(chunk) = self.chunks.get_mut(&key) {
            if let Some(old) = chunk.frames.insert(frame, video) {
                old.close();
            }
        } else {
            video.close();
        }
        self.enforce_frame_budget();
    }

    /// Insert many frames from a scrub GOP decode (skips IDs already present).
    pub fn insert_gop(&mut self, key: FrameId, frames: HashMap<FrameId, VideoFrame>) {
        if frames.is_empty() {
            return;
        }
        if !self.chunks.contains(&key) {
            self.evict_until_room_for_chunk();
            self.chunks.put(
                key,
                FrameChunk {
                    frames: HashMap::new(),
                },
            );
        }
        if let Some(chunk) = self.chunks.get_mut(&key) {
            for (id, video) in frames {
                if chunk.frames.contains_key(&id) {
                    video.close();
                } else if let Some(old) = chunk.frames.insert(id, video) {
                    old.close();
                }
            }
        } else {
            for (_, video) in frames {
                video.close();
            }
        }
        self.enforce_frame_budget();
    }

    /// Chunk keys to keep around the playhead (bias: fill behind first on PREV).
    pub fn desired_chunk_keys(
        playhead_key: FrameId,
        prev_key: Option<FrameId>,
        next_key: Option<FrameId>,
        fill_behind_first: bool,
    ) -> Vec<FrameId> {
        let mut keys = Vec::with_capacity(3);
        let push = |keys: &mut Vec<FrameId>, k: FrameId| {
            if !keys.contains(&k) {
                keys.push(k);
            }
        };
        if fill_behind_first {
            if let Some(p) = prev_key {
                push(&mut keys, p);
            }
            push(&mut keys, playhead_key);
            if let Some(n) = next_key {
                push(&mut keys, n);
            }
        } else {
            push(&mut keys, playhead_key);
            if let Some(n) = next_key {
                push(&mut keys, n);
            }
            if let Some(p) = prev_key {
                push(&mut keys, p);
            }
        }
        keys
    }

    /// Chunk keys that have no entry yet (not the same as an incomplete GOP —
    /// use [`Self::gop_complete`] after a single-frame `insert_owned`).
    #[allow(dead_code)] // ensure_window uses gop_complete; kept for callers
    pub fn missing_among(&self, keys: &[FrameId]) -> Vec<FrameId> {
        keys.iter()
            .copied()
            .filter(|k| !self.chunks.contains(k))
            .collect()
    }

    /// True when every id in `gop_ids` is present in the chunk for `key`.
    ///
    /// A chunk created by a single `insert_owned` (play/step) must not count as
    /// a full GOP — otherwise scrub fill skips and PREV keeps cold-cascading.
    pub fn gop_complete(&self, key: FrameId, gop_ids: &[FrameId]) -> bool {
        if gop_ids.is_empty() {
            return self.chunks.contains(&key);
        }
        let Some(chunk) = self.chunks.peek(&key) else {
            return false;
        };
        gop_ids.iter().all(|id| chunk.frames.contains_key(id))
    }

    fn evict_until_room_for_chunk(&mut self) {
        // `LruCache::put` already pops LRU when at capacity; prefer dropping a
        // non-playhead chunk first by demoting playhead to MRU.
        let _ = self.chunks.get(&self.playhead_key);
    }

    fn enforce_frame_budget(&mut self) {
        while self.frame_count() > self.max_frames {
            let victim = self
                .chunks
                .iter()
                .rev()
                .map(|(k, _)| *k)
                .find(|k| *k != self.playhead_key);
            if let Some(key) = victim {
                let _ = self.chunks.pop(&key);
                continue;
            }
            if !self.trim_one_from_playhead_chunk() {
                break;
            }
        }
    }

    /// Drop one frame farthest from the playhead inside the playhead chunk.
    fn trim_one_from_playhead_chunk(&mut self) -> bool {
        let playhead = self.playhead;
        let Some(chunk) = self.chunks.get_mut(&self.playhead_key) else {
            return false;
        };
        let farthest = chunk
            .frames
            .keys()
            .copied()
            .filter(|id| *id != playhead)
            .max_by_key(|id| id.abs_diff(playhead));
        let Some(id) = farthest else {
            return false;
        };
        if let Some(f) = chunk.frames.remove(&id) {
            f.close();
        }
        true
    }
}

/// One GOP of scrub previews at **caller-requested** bitmap size (UI owns scale).
#[allow(dead_code)] // filmstrip path fills this later
pub struct ThumbGop {
    pub key_frame: FrameId,
    /// Presentation time (µs) → small bitmap. Filled by filmstrip path later.
    pub by_time: HashMap<i64, ThumbBitmap>,
}

/// Stored thumb raster (exact requested size). `ImageBitmap` when filmstrip lands.
#[allow(dead_code)]
pub enum ThumbBitmap {
    /// Placeholder until blit path is wired.
    Reserved { width: u32, height: u32 },
}

/// Sparse thumb LRU keyed by GOP keyframe (separate budget from dense cache).
#[allow(dead_code)] // clear() used; insert path arrives with samplesAtTimestamps
pub struct ThumbCache {
    gops: LruCache<FrameId, ThumbGop>,
    max_thumbs: usize,
}

#[allow(dead_code)] // clear() used; insert path arrives with samplesAtTimestamps
impl ThumbCache {
    pub fn new(max_key_gops: usize, max_thumbs: usize) -> Self {
        let cap = NonZeroUsize::new(max_key_gops.max(1)).expect("max_key_gops >= 1");
        Self {
            gops: LruCache::new(cap),
            max_thumbs: max_thumbs.max(1),
        }
    }

    pub fn clear(&mut self) {
        self.gops.clear();
    }

    pub fn gop_count(&self) -> usize {
        self.gops.len()
    }

    pub fn thumb_count(&self) -> usize {
        self.gops.iter().map(|(_, g)| g.by_time.len()).sum()
    }

    pub fn get_gop_mut(&mut self, key: FrameId) -> Option<&mut ThumbGop> {
        self.gops.get_mut(&key)
    }

    pub fn ensure_gop(&mut self, key: FrameId) -> &mut ThumbGop {
        if !self.gops.contains(&key) {
            self.gops.put(
                key,
                ThumbGop {
                    key_frame: key,
                    by_time: HashMap::new(),
                },
            );
        }
        self.enforce_thumb_budget();
        self.gops.get_mut(&key).expect("just inserted")
    }

    fn enforce_thumb_budget(&mut self) {
        while self.thumb_count() > self.max_thumbs && self.gops.len() > 1 {
            let victim = self.gops.iter().rev().map(|(k, _)| *k).next();
            if let Some(k) = victim {
                let _ = self.gops.pop(&k);
            } else {
                break;
            }
        }
    }
}
