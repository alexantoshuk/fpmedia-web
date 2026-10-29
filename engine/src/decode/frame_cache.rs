//! Exact scrub blit plane + hover thumbs.
//!
//! - [`ScrubBlitLru`] — live `ImageBitmap` for scrub/step present, over
//!   [`AimWindow`] (evict farthest from playhead). Markers + run geometry live in
//!   [`crate::residency`]. Thumbs stay separate.

use std::collections::HashMap;

use crate::policy::scrub_preview_neighbor;
use crate::residency::AimWindow;
use crate::types::FrameId;
use web_sys::ImageBitmap;

/// Scrub/filmstrip keyframe GOPs retained (one sync-key slot each).
/// Sized for Pause sparse fill (≥16 timeline keys + midpoint densify headroom).
/// **no equivalent** — mediabunny `CanvasSink` `poolSize` is iterator reuse, not residency.
pub const DEFAULT_MAX_THUMB_GOPS: usize = 256;
pub const DEFAULT_MAX_THUMBS: usize = 256;

/// Live scrub blit handles. Evict farthest from playhead (not classic LRU) so
/// densify ahead cannot wipe just-presented frames (Prev stayed cold on long-GOP).
pub struct ScrubBlitLru {
    window: AimWindow<ImageBitmap>,
}

impl ScrubBlitLru {
    pub fn new(cap: usize) -> Self {
        Self {
            window: AimWindow::new(cap),
        }
    }

    /// Resize live blit window (profile policy). Evicts farthest from playhead.
    pub fn set_max(&mut self, cap: usize) {
        self.window.set_max(cap);
        while self.window.len() > self.window.max() {
            let Some((_, bmp)) = self.window.take_farthest(None) else {
                break;
            };
            bmp.close();
        }
    }

    pub fn clear(&mut self) {
        for (_, bmp) in self.window.take_all() {
            bmp.close();
        }
    }

    pub fn set_playhead(&mut self, playhead: FrameId) {
        // Aim only — never prune here (soft-drag samples this every move).
        self.window.set_aim(playhead);
    }

    /// Farthest-from-aim eviction without a `put` (densify Covered-only / under-cap).
    ///
    /// Radius = blit cap in frames. Call from Pause densify every tick — not only
    /// when Exact near-gap is None (soft-drag holes kept the idle gate from firing).
    /// Not on drag `set_playhead`. Local (`mediabunny:` aim-sized pool).
    pub fn evict_far_from_aim_when_idle(&mut self) {
        let radius = self.window.max() as u64;
        for (_, bmp) in self.window.take_farther_than(radius) {
            bmp.close();
        }
    }

    /// Frames with a live `ImageBitmap` (demo Exact strip / step-ready).
    pub fn cached_frame_ids(&self) -> Vec<FrameId> {
        self.window.keys().collect()
    }

    pub fn contains(&self, frame: FrameId) -> bool {
        self.window.contains(frame)
    }

    pub fn len(&self) -> usize {
        self.window.len()
    }

    pub fn max(&self) -> usize {
        self.window.max()
    }

    /// Same refuse gate as [`Self::put`] at capacity (no bitmap alloc).
    pub fn would_evict_closer_than(&self, frame: FrameId, from: FrameId) -> bool {
        self.window.would_evict_closer_than(frame, from)
    }

    /// Exact neighbor biased by scrub/hover `dir` (floor forward / ceil back;
    /// `dir == 0` → abs-nearest).
    pub fn nearest_frame_with_dir(&self, target: FrameId, dir: i8) -> Option<FrameId> {
        self.window.nearest_with_dir(target, dir)
    }

    /// Next frame missing a live bitmap near `playhead` (biased by `dir`).
    ///
    /// Used after Play wipes blit while scrub markers stay — densify must see
    /// ImageBitmap holes, not only marker gaps. Margins = same pair as markers;
    /// see [`AimWindow::capped_next_gap`].
    pub fn next_gap_frame(
        &self,
        playhead: FrameId,
        last: FrameId,
        dir: i8,
        margin_left: u64,
        margin_right: u64,
    ) -> Option<FrameId> {
        self.window
            .capped_next_gap(playhead, last, dir, margin_left, margin_right)
    }

    pub fn get(&mut self, frame: FrameId) -> Option<&ImageBitmap> {
        self.window.get(frame)
    }

    /// Insert or replace a live bitmap. Returns `false` when refused at capacity
    /// (would only evict a closer resident — caller must close unused planes).
    pub fn put(&mut self, frame: FrameId, bitmap: ImageBitmap) -> bool {
        if !self.window.contains(frame)
            && self.window.len() >= self.window.max()
            && self
                .window
                .would_evict_closer_than(frame, self.window.aim())
        {
            bitmap.close();
            return false;
        }
        if let Some(old) = self.window.insert(frame, bitmap) {
            old.close();
            return true;
        }
        while self.window.len() > self.window.max() {
            let Some((_, bmp)) = self.window.take_farthest(Some(frame)) else {
                break;
            };
            bmp.close();
        }
        if self.window.len() > self.window.max() {
            if let Some(bmp) = self.window.remove(frame) {
                bmp.close();
            }
            return false;
        }
        true
    }
}

impl Drop for ScrubBlitLru {
    fn drop(&mut self) {
        self.clear();
    }
}

/// One GOP of hover / filmstrip thumbs at **caller-requested** bitmap size.
pub struct ThumbGop {
    pub by_time: HashMap<i64, ThumbBitmap>,
}

impl ThumbGop {
    fn close_all(&mut self) {
        for (_, thumb) in self.by_time.drain() {
            thumb.close();
        }
    }
}

/// Hover thumb plane — separate budget from Exact blit (`AGENTS.md` / product).
pub enum ThumbBitmap {
    Ready {
        bitmap: ImageBitmap,
        width: u32,
        height: u32,
        /// Progressive rank — lower = coarser timeline grid (kept longer).
        rank: u16,
    },
}

impl ThumbBitmap {
    fn close(self) {
        match self {
            Self::Ready { bitmap, .. } => bitmap.close(),
        }
    }

    fn matches_size(&self, width: u32, height: u32) -> bool {
        match self {
            Self::Ready {
                width: w,
                height: h,
                ..
            } => *w == width && *h == height,
        }
    }

    fn bitmap(&self) -> &ImageBitmap {
        match self {
            Self::Ready { bitmap, .. } => bitmap,
        }
    }

    fn rank(&self) -> u16 {
        match self {
            Self::Ready { rank, .. } => *rank,
        }
    }
}

/// Filmstrip / hover thumbs — **not** charged to Exact `blit_mib`.
///
/// Residency is progressive-rank based (not LRU): denser / Exact thumbs yield
/// so the coarse timeline grid stays even. **no equivalent** (filmstrip local).
pub struct ThumbCache {
    gops: HashMap<FrameId, ThumbGop>,
    max_gops: usize,
    max_thumbs: usize,
}

impl ThumbCache {
    pub fn new(max_key_gops: usize, max_thumbs: usize) -> Self {
        Self {
            gops: HashMap::new(),
            max_gops: max_key_gops.max(1),
            max_thumbs: max_thumbs.max(1),
        }
    }

    pub fn clear(&mut self) {
        for (_, mut gop) in self.gops.drain() {
            gop.close_all();
        }
    }

    pub fn thumb_count(&self) -> usize {
        self.gops.values().map(|g| g.by_time.len()).sum()
    }

    pub fn max_thumbs(&self) -> usize {
        self.max_thumbs
    }

    /// True when another distinct thumb / sync-key slot would hit caps.
    pub fn at_capacity(&self) -> bool {
        self.thumb_count() >= self.max_thumbs || self.gops.len() >= self.max_gops
    }

    pub fn has_gop(&self, key: FrameId) -> bool {
        self.gops.contains_key(&key)
    }

    /// Whether a new thumb with `rank` may enter (evict only denser / same band).
    ///
    /// Protects coarser grid: never admits by dropping below the eviction floor.
    /// Exact (`u16::MAX`) may displace densify (`≥1`) but not coarse `0`.
    pub fn can_admit(&self, rank: u16, needs_new_gop: bool) -> bool {
        let thumb_room = self.thumb_count() < self.max_thumbs;
        let gop_room = !needs_new_gop || self.gops.len() < self.max_gops;
        if thumb_room && gop_room {
            return true;
        }
        let floor = Self::eviction_floor(rank);
        self.has_evictable_thumb(floor, None)
            || (needs_new_gop && self.has_evictable_gop(floor, None))
    }

    /// Exact non-key (`u16::MAX`) displaces densify only — never coarse rank 0.
    fn eviction_floor(rank: u16) -> u16 {
        if rank == u16::MAX { 1 } else { rank }
    }

    /// Lookup a ready thumb for `frame` under GOP `key` at the requested size.
    pub fn get(
        &mut self,
        key: FrameId,
        frame: FrameId,
        width: u32,
        height: u32,
    ) -> Option<&ImageBitmap> {
        let gop = self.gops.get_mut(&key)?;
        let thumb = gop.by_time.get(&(frame as i64))?;
        if thumb.matches_size(width, height) {
            Some(thumb.bitmap())
        } else {
            None
        }
    }

    /// Whether a ready thumb exists for `frame` under `key` at size.
    pub fn contains(&self, key: FrameId, frame: FrameId, width: u32, height: u32) -> bool {
        let Some(gop) = self.gops.get(&key) else {
            return false;
        };
        gop.by_time
            .get(&(frame as i64))
            .is_some_and(|t| t.matches_size(width, height))
    }

    /// Closest ready thumb to `target` at size, biased by cursor `dir`
    /// ([`scrub_preview_neighbor`]; `dir == 0` → abs-nearest).
    pub fn nearest_frame_with_dir(
        &self,
        target: FrameId,
        width: u32,
        height: u32,
        dir: i8,
    ) -> Option<(FrameId, FrameId)> {
        let mut floor: Option<(FrameId, FrameId, u64)> = None;
        let mut ceil: Option<(FrameId, FrameId, u64)> = None;
        for (&key, gop) in &self.gops {
            for (&ts, thumb) in &gop.by_time {
                if !thumb.matches_size(width, height) {
                    continue;
                }
                let frame = ts as FrameId;
                let dist = frame.abs_diff(target);
                let better = |best: &Option<(FrameId, FrameId, u64)>| match best {
                    None => true,
                    Some((_, _, d)) if dist < *d => true,
                    Some((bk, bf, d))
                        if dist == *d && (frame < *bf || (frame == *bf && key < *bk)) =>
                    {
                        true
                    }
                    _ => false,
                };
                if frame <= target && better(&floor) {
                    floor = Some((key, frame, dist));
                }
                if frame >= target && better(&ceil) {
                    ceil = Some((key, frame, dist));
                }
            }
        }
        let pick = scrub_preview_neighbor(
            target,
            floor.map(|(_, f, _)| f),
            ceil.map(|(_, f, _)| f),
            dir,
        )?;
        if floor.is_some_and(|(_, f, _)| f == pick) {
            floor.map(|(k, f, _)| (k, f))
        } else {
            ceil.map(|(k, f, _)| (k, f))
        }
    }

    /// Insert or replace a ready thumb with progressive `rank`.
    ///
    /// Evicts denser thumbs (higher rank) first so the coarse grid stays even.
    /// Refuses the insert (closes `bitmap`) when nothing denser can be freed.
    pub fn put_ready(
        &mut self,
        key: FrameId,
        frame: FrameId,
        bitmap: ImageBitmap,
        width: u32,
        height: u32,
        rank: u16,
    ) {
        let replacing = self
            .gops
            .get(&key)
            .is_some_and(|g| g.by_time.contains_key(&(frame as i64)));
        let new_gop = !self.gops.contains_key(&key);

        if !replacing {
            let floor = Self::eviction_floor(rank);
            while self.thumb_count() >= self.max_thumbs {
                if !self.evict_thumb(floor, Some((key, frame as i64))) {
                    bitmap.close();
                    return;
                }
            }
            if new_gop {
                while self.gops.len() >= self.max_gops {
                    if !self.evict_gop(floor, Some(key)) {
                        bitmap.close();
                        return;
                    }
                }
            }
        }

        self.gops.entry(key).or_insert_with(|| ThumbGop {
            by_time: HashMap::new(),
        });
        let gop = self.gops.get_mut(&key).expect("just ensured");
        let keep_rank = gop
            .by_time
            .get(&(frame as i64))
            .map(|t| t.rank().min(rank))
            .unwrap_or(rank);
        if let Some(old) = gop.by_time.insert(
            frame as i64,
            ThumbBitmap::Ready {
                bitmap,
                width,
                height,
                rank: keep_rank,
            },
        ) {
            old.close();
        }
        self.enforce_thumb_budget();
    }

    fn enforce_thumb_budget(&mut self) {
        while self.thumb_count() > self.max_thumbs {
            if !self.evict_thumb(0, None) {
                break;
            }
        }
        while self.gops.len() > self.max_gops {
            if !self.evict_gop(0, None) {
                break;
            }
        }
    }

    fn has_evictable_thumb(&self, incoming_rank: u16, skip: Option<(FrameId, i64)>) -> bool {
        for (&key, gop) in &self.gops {
            for (&ts, thumb) in &gop.by_time {
                if skip == Some((key, ts)) {
                    continue;
                }
                if thumb.rank() >= incoming_rank {
                    return true;
                }
            }
        }
        false
    }

    fn has_evictable_gop(&self, incoming_rank: u16, skip: Option<FrameId>) -> bool {
        if self.gops.len() <= 1 {
            return false;
        }
        for (&key, gop) in &self.gops {
            if skip == Some(key) {
                continue;
            }
            let max_r = gop.by_time.values().map(|t| t.rank()).max().unwrap_or(0);
            if max_r >= incoming_rank {
                return true;
            }
        }
        false
    }

    /// Drop one thumb with highest rank among those `>= incoming_rank`.
    fn evict_thumb(&mut self, incoming_rank: u16, skip: Option<(FrameId, i64)>) -> bool {
        let mut best: Option<(u16, bool, FrameId, i64)> = None; // rank, is_exact, key, ts
        for (&key, gop) in &self.gops {
            for (&ts, thumb) in &gop.by_time {
                if skip == Some((key, ts)) {
                    continue;
                }
                let r = thumb.rank();
                if r < incoming_rank {
                    continue;
                }
                let is_exact = ts as FrameId != key;
                let replace = match best {
                    None => true,
                    Some((br, be, bk, bts)) => {
                        r > br
                            || (r == br && is_exact && !be)
                            || (r == br && is_exact == be && (key > bk || (key == bk && ts > bts)))
                    }
                };
                if replace {
                    best = Some((r, is_exact, key, ts));
                }
            }
        }
        let Some((_, _, key, ts)) = best else {
            return false;
        };
        let Some(gop) = self.gops.get_mut(&key) else {
            return false;
        };
        if let Some(old) = gop.by_time.remove(&ts) {
            old.close();
        }
        if gop.by_time.is_empty() {
            self.gops.remove(&key);
        }
        true
    }

    /// Drop one GOP whose densest thumb rank is highest (`>= incoming_rank`).
    fn evict_gop(&mut self, incoming_rank: u16, skip: Option<FrameId>) -> bool {
        if self.gops.len() <= 1 {
            return false;
        }
        let mut best: Option<(u16, FrameId)> = None; // max_rank, key
        for (&key, gop) in &self.gops {
            if skip == Some(key) {
                continue;
            }
            let max_r = gop.by_time.values().map(|t| t.rank()).max().unwrap_or(0);
            if max_r < incoming_rank {
                continue;
            }
            let replace = match best {
                None => true,
                Some((br, bk)) => max_r > br || (max_r == br && key > bk),
            };
            if replace {
                best = Some((max_r, key));
            }
        }
        let Some((_, key)) = best else {
            return false;
        };
        if let Some(mut gop) = self.gops.remove(&key) {
            gop.close_all();
            true
        } else {
            false
        }
    }
}

impl Drop for ThumbCache {
    fn drop(&mut self) {
        self.clear();
    }
}
