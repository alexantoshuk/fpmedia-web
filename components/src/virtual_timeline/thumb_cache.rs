//! VT global thumb **bitmap** wallet (playlist Σ / Single identity).
//!
//! Separate from slot `ThumbSink` / Exact blit. Progressive-rank eviction
//! (coarse grid kept longer). **no equivalent** (filmstrip local).
//! Decode: `mediabunny:` CanvasSink — producer only.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fpmedia_web::FrameId;
use web_sys::ImageBitmap;

/// Shared handle for one timeline session.
pub type ThumbCacheHandle = Rc<RefCell<VtThumbCache>>;

pub fn new_thumb_cache_handle() -> ThumbCacheHandle {
    Rc::new(RefCell::new(VtThumbCache::default()))
}

/// Default cap — matches engine `DEFAULT_MAX_THUMBS`.
pub const VT_THUMB_MAX: usize = 256;

struct VtThumbEntry {
    bitmap: ImageBitmap,
    width: u32,
    height: u32,
    rank: u16,
}

impl VtThumbEntry {
    fn close(self) {
        self.bitmap.close();
    }

    fn matches_size(&self, width: u32, height: u32) -> bool {
        self.width == width && self.height == height
    }
}

/// Global FrameId → sized thumb bitmap for one timeline session.
pub struct VtThumbCache {
    by_frame: HashMap<FrameId, VtThumbEntry>,
    max_thumbs: usize,
}

impl VtThumbCache {
    pub fn new(max_thumbs: usize) -> Self {
        Self {
            by_frame: HashMap::new(),
            max_thumbs: max_thumbs.max(1),
        }
    }

    pub fn max_thumbs(&self) -> usize {
        self.max_thumbs
    }

    pub fn len(&self) -> usize {
        self.by_frame.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_frame.is_empty()
    }

    pub fn clear(&mut self) {
        for (_, e) in self.by_frame.drain() {
            e.close();
        }
    }

    pub fn has(&self, global: FrameId, width: u32, height: u32) -> bool {
        self.by_frame
            .get(&global)
            .is_some_and(|e| e.matches_size(width, height))
    }

    pub fn has_any(&self, global: FrameId) -> bool {
        self.by_frame.contains_key(&global)
    }

    /// Exact frame hit at size.
    pub fn get(&self, global: FrameId, width: u32, height: u32) -> Option<&ImageBitmap> {
        let e = self.by_frame.get(&global)?;
        e.matches_size(width, height).then_some(&e.bitmap)
    }

    /// Closest resident thumb to `target`. Prefers `width`×`height` match, else any size.
    pub fn nearest(
        &self,
        target: FrameId,
        width: u32,
        height: u32,
        dir: i8,
    ) -> Option<(FrameId, &ImageBitmap, u32, u32)> {
        let _ = dir;
        let pick = |require_size: bool| -> Option<(FrameId, u64)> {
            let mut best: Option<(FrameId, u64)> = None;
            for (&id, e) in &self.by_frame {
                if require_size && !e.matches_size(width, height) {
                    continue;
                }
                let dist = id.abs_diff(target);
                let better = match best {
                    None => true,
                    Some((bid, d)) => dist < d || (dist == d && id < bid),
                };
                if better {
                    best = Some((id, dist));
                }
            }
            best
        };
        let (id, _) = pick(true).or_else(|| pick(false))?;
        self.by_frame
            .get(&id)
            .map(|e| (id, &e.bitmap, e.width, e.height))
    }

    /// Insert or replace. Evicts denser (higher rank) first. Closes `bitmap` on refuse.
    pub fn put(
        &mut self,
        global: FrameId,
        bitmap: ImageBitmap,
        width: u32,
        height: u32,
        rank: u16,
    ) -> bool {
        let replacing = self.by_frame.contains_key(&global);
        if !replacing {
            let floor = if rank == u16::MAX { 1 } else { rank };
            while self.by_frame.len() >= self.max_thumbs {
                if !self.evict_one(floor, Some(global)) {
                    bitmap.close();
                    return false;
                }
            }
        }
        let keep_rank = self
            .by_frame
            .get(&global)
            .map(|e| e.rank.min(rank))
            .unwrap_or(rank);
        if let Some(old) = self.by_frame.insert(
            global,
            VtThumbEntry {
                bitmap,
                width,
                height,
                rank: keep_rank,
            },
        ) {
            old.close();
        }
        while self.by_frame.len() > self.max_thumbs {
            if !self.evict_one(0, None) {
                break;
            }
        }
        true
    }

    fn evict_one(&mut self, incoming_rank: u16, skip: Option<FrameId>) -> bool {
        let mut best: Option<(u16, FrameId)> = None;
        for (&id, e) in &self.by_frame {
            if skip == Some(id) {
                continue;
            }
            if e.rank < incoming_rank {
                continue;
            }
            let replace = match best {
                None => true,
                Some((br, bid)) => e.rank > br || (e.rank == br && id > bid),
            };
            if replace {
                best = Some((e.rank, id));
            }
        }
        let Some((_, id)) = best else {
            return false;
        };
        if let Some(e) = self.by_frame.remove(&id) {
            e.close();
            true
        } else {
            false
        }
    }
}

impl Default for VtThumbCache {
    fn default() -> Self {
        Self::new(VT_THUMB_MAX)
    }
}

impl Drop for VtThumbCache {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ImageBitmap is wasm-only — unit tests cover eviction math via a stub path
    // when we add a test double; smoke logic checked in wasm e2e.
    #[test]
    fn empty_defaults() {
        let c = VtThumbCache::new(4);
        assert_eq!(c.max_thumbs(), 4);
        assert!(c.is_empty());
        assert!(!c.has_any(0));
    }
}
