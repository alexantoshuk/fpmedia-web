//! MP4 / ISOBMFF demux via `re_mp4` + ranged [`MediaSource`].

use std::cell::{Cell, Ref, RefCell};
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use re_mp4::{Mp4, TrackKind};

use crate::audio::Mp4AudioTrack;
use crate::audio::build_audio_track;
use crate::bitstream::{
    PacketType, avc_nal_length_size, determine_avc_packet_type, determine_hevc_packet_type,
    hevc_nal_length_size,
};
use crate::error::MediaError;
use crate::frame_index::{FrameEntry, FrameIndex};
use crate::source::{CachedSource, FetchMap, MediaSource, MemorySource};
use crate::types::{CodecConfig, EncodedSample, FrameId, VideoCodec, VideoMetadata};
use crate::util::block_on;

/// Options for [`open_mp4_source`].
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenMp4Options {
    /// Mediabunny-style bitstream sync verify (AVC IDR / SEI; HEVC IRAP).
    pub verify_avc_sync: bool,
    /// When [`Self::verify_avc_sync`], do not block open on a full scan — queue
    /// candidates for background / on-demand [`Mp4VideoTrack::ensure_key_verified`]
    /// (`mediabunny: verifyKeyPackets` on retrieve). Eager when false (tests).
    pub defer_avc_sync: bool,
}

/// Deferred bitstream sync-verify bookkeeping (moov flags until checked).
#[derive(Clone, Debug, Default)]
struct SyncVerify {
    pending: VecDeque<FrameId>,
    checked: HashSet<FrameId>,
    complete: bool,
    length_size: Option<usize>,
    /// [`VideoCodec::Avc`] or [`VideoCodec::Hevc`] when armed.
    codec: Option<VideoCodec>,
}

/// Demuxed H.264/H.265 (etc.) video track with a presentation-order frame index.
///
/// Sample bytes are fetched on demand from [`MediaSource`] using **file-absolute**
/// offsets from the sample table (not stub-relative).
#[derive(Clone)]
pub struct Mp4VideoTrack {
    source: Arc<dyn MediaSource>,
    /// Uncached (or same as [`Self::source`]) reads for bitstream sync verify.
    /// After [`CachedSource`] attach, keep the raw inner here so deferred verify
    /// does not 8 MiB-prefetch every scattered key (GitHub/proxy 502 under scrub).
    sync_source: Arc<dyn MediaSource>,
    /// ISOBMFF `tkhd` track id (for `tfra` provisional duration).
    track_id: u32,
    metadata: RefCell<VideoMetadata>,
    index: RefCell<FrameIndex>,
    sync_verify: RefCell<SyncVerify>,
    /// Bumped when sync table changes (bg demote) so hosts can refresh GOP UI.
    sync_gen: Cell<u64>,
    /// fMP4 warm-open remainder (mediabunny: fragments after `readMetadata`).
    fragment_backfill: RefCell<Option<FragmentBackfillState>>,
}

#[derive(Clone)]
struct FragmentBackfillState {
    /// `None` while a chunk step holds the plan across `.await`.
    plan: Option<crate::demux::FragmentBackfillPlan>,
    source: Arc<dyn MediaSource>,
    complete: bool,
    started: bool,
    /// Bg wasm worker already spawned (avoid double-loop).
    worker_running: bool,
    error: Option<String>,
}

impl fmt::Debug for Mp4VideoTrack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mp4VideoTrack")
            .field("metadata", &*self.metadata.borrow())
            .field("frames", &self.index.borrow().len())
            .field("source_size", &self.source.size())
            .field("sync_verify_complete", &self.sync_verify_complete())
            .finish()
    }
}

impl Mp4VideoTrack {
    pub fn metadata(&self) -> VideoMetadata {
        self.metadata.borrow().clone()
    }

    pub fn index(&self) -> Ref<'_, FrameIndex> {
        self.index.borrow()
    }

    pub fn frame_count(&self) -> u64 {
        self.metadata.borrow().frame_count
    }

    pub fn source(&self) -> &Arc<dyn MediaSource> {
        &self.source
    }

    /// Swap playback I/O to a cached wrapper; sync-verify keeps the previous source.
    pub fn set_playback_source(&mut self, source: Arc<dyn MediaSource>) {
        self.source = source;
    }

    /// True when AVC sync verify finished (or was never required).
    pub fn sync_verify_complete(&self) -> bool {
        self.sync_verify.borrow().complete
    }

    /// How many moov sync candidates still need bitstream checks.
    pub fn sync_verify_pending(&self) -> usize {
        self.sync_verify.borrow().pending.len()
    }

    pub fn sync_verify_generation(&self) -> u64 {
        self.sync_gen.get()
    }

    /// How many frames are currently in the index (may be &lt; [`Self::frame_count`] while backfilling).
    pub fn indexed_frame_count(&self) -> u64 {
        self.index.borrow().len() as u64
    }

    /// True when fMP4 fragment tables are fully loaded (or file was never fragmented).
    pub fn fragments_complete(&self) -> bool {
        self.fragment_backfill
            .borrow()
            .as_ref()
            .map(|s| s.complete)
            .unwrap_or(true)
    }

    pub(crate) fn arm_fragment_backfill(
        &self,
        plan: crate::demux::FragmentBackfillPlan,
        source: Arc<dyn MediaSource>,
    ) {
        self.apply_provisional_fragment_length(&plan);
        *self.fragment_backfill.borrow_mut() = Some(FragmentBackfillState {
            plan: Some(plan),
            source,
            complete: false,
            started: false,
            worker_running: false,
            error: None,
        });
    }

    /// Scrubber length before remaining moofs load (`mediabunny: tfra` lookup times).
    ///
    /// Index stays warm-only; [`Self::ensure_frame_indexed`] awaits chunks past it.
    fn apply_provisional_fragment_length(&self, plan: &crate::demux::FragmentBackfillPlan) {
        let warm_moofs = plan.warm_moof_count.max(1) as u64;
        let total_moofs = plan.idx.moof_offsets.len() as u64;
        if total_moofs <= warm_moofs {
            return;
        }
        let mut meta = self.metadata.borrow_mut();
        let warm_frames = meta.frame_count.max(1);
        let warm_dur = meta.duration_ticks;

        // Uniform fragment density (this plate: 120×GOP12 → exact 1440).
        meta.frame_count = warm_frames.saturating_mul(total_moofs) / warm_moofs;

        // Prefer last video `tfra` time + one warm-moof span (last entry is fragment
        // start, not EOF). Else scale warm duration by moof count.
        let moof_span = if warm_dur > 0 {
            warm_dur / warm_moofs
        } else {
            0
        };
        if let Some((_, last_t)) = plan
            .idx
            .tfra_last_time
            .iter()
            .find(|(id, _)| *id == self.track_id)
            .copied()
        {
            meta.duration_ticks = last_t.saturating_add(moof_span).max(1);
        } else if warm_dur > 0 {
            meta.duration_ticks = warm_dur.saturating_mul(total_moofs) / warm_moofs;
        }

        if meta.duration_ticks > 0 && meta.frame_count > 0 {
            meta.fps =
                Some(meta.frame_count as f64 * meta.timescale as f64 / meta.duration_ticks as f64);
        }
        self.sync_gen.set(self.sync_gen.get().saturating_add(1));
    }

    /// Mark backfill worker as started (idempotent).
    pub fn kick_fragment_backfill(&self) {
        if let Some(s) = self.fragment_backfill.borrow_mut().as_mut() {
            s.started = true;
        }
    }

    /// Claim the bg chunk loop (wasm). `false` if already running / done / none.
    pub fn begin_fragment_backfill_worker(&self) -> bool {
        let mut slot = self.fragment_backfill.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return false;
        };
        if state.complete || state.worker_running {
            return false;
        }
        state.worker_running = true;
        state.started = true;
        true
    }

    /// Load the next moof chunk and extend the frame index.
    ///
    /// Returns `true` when more moofs remain. Play awaits only the needed frame via
    /// [`Self::ensure_frame_indexed`] — not the full file.
    /// `mediabunny: isobmff-demuxer.ts` `readFragment`.
    pub async fn run_fragment_backfill_step(&self) -> Result<bool, MediaError> {
        let (mut plan, source) = {
            let mut slot = self.fragment_backfill.borrow_mut();
            let Some(state) = slot.as_mut() else {
                return Ok(false);
            };
            if state.complete {
                return Ok(false);
            }
            let Some(plan) = state.plan.take() else {
                // Another step holds the plan across await — more work pending.
                return Ok(true);
            };
            state.started = true;
            (plan, Arc::clone(&state.source))
        };

        match crate::demux::advance_mfra_backfill_chunk(source.as_ref(), &mut plan, |_| {}).await {
            Ok((mp4, complete)) => {
                let provisional = if complete {
                    None
                } else {
                    let meta = self.metadata.borrow();
                    Some((meta.frame_count, meta.duration_ticks, meta.fps))
                };
                let timecode_start = self.metadata.borrow().timecode_start;
                let rebuilt = build_video_track(
                    &mp4,
                    Arc::clone(&self.sync_source),
                    OpenMp4Options {
                        verify_avc_sync: false,
                        defer_avc_sync: false,
                    },
                    timecode_start,
                );
                match rebuilt {
                    Ok(Some(fresh)) => {
                        *self.index.borrow_mut() = fresh.index.borrow().clone();
                        if let Some((fc, dur, fps)) = provisional {
                            let mut meta = self.metadata.borrow_mut();
                            *meta = fresh.metadata.borrow().clone();
                            // Keep scrubber length from tfra until the last chunk.
                            meta.frame_count = fc.max(self.index.borrow().len() as u64);
                            meta.duration_ticks = dur;
                            meta.fps = fps;
                        } else {
                            *self.metadata.borrow_mut() = fresh.metadata.borrow().clone();
                        }
                        if let Some(state) = self.fragment_backfill.borrow_mut().as_mut() {
                            state.plan = if complete { None } else { Some(plan) };
                            state.complete = complete;
                            state.error = None;
                        }
                        self.sync_gen.set(self.sync_gen.get().saturating_add(1));
                        Ok(!complete)
                    }
                    Ok(None) => {
                        if let Some(state) = self.fragment_backfill.borrow_mut().as_mut() {
                            state.plan = Some(plan);
                            state.error = Some("mfra backfill: no video track".into());
                            state.complete = true;
                        }
                        Err(MediaError::Mp4("mfra backfill: no video track".into()))
                    }
                    Err(e) => {
                        if let Some(state) = self.fragment_backfill.borrow_mut().as_mut() {
                            state.plan = Some(plan);
                            state.error = Some(e.to_string());
                            state.complete = true;
                        }
                        Err(e)
                    }
                }
            }
            Err(e) => {
                if let Some(state) = self.fragment_backfill.borrow_mut().as_mut() {
                    state.plan = Some(plan);
                    state.error = Some(e.to_string());
                    state.complete = true;
                }
                Err(e)
            }
        }
    }

    /// Block until fragment backfill finishes (or there is nothing to do).
    pub async fn await_fragment_backfill(&self) -> Result<(), MediaError> {
        if self.fragments_complete() {
            return Ok(());
        }
        // Native / tests: run chunks inline. Wasm: bg worker in VideoPlayer runs
        // steps; we only poll here.
        #[cfg(not(target_arch = "wasm32"))]
        {
            while !self.fragments_complete() {
                let more = self.run_fragment_backfill_step().await?;
                if !more {
                    break;
                }
            }
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.kick_fragment_backfill();
            while !self.fragments_complete() {
                if let Some(err) = self
                    .fragment_backfill
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.error.clone())
                {
                    return Err(MediaError::Mp4(err));
                }
                crate::util::yield_now().await;
            }
            Ok(())
        }
    }

    /// Ensure `frame` is in the index — wait only for chunks past it, not the
    /// whole file (`mediabunny: readFragment` on demand).
    async fn ensure_frame_indexed(&self, frame: FrameId) -> Result<(), MediaError> {
        if (frame as usize) < self.index.borrow().len() {
            return Ok(());
        }
        if self.fragments_complete() {
            return Err(MediaError::FrameOutOfRange(frame, self.frame_count()));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            while (frame as usize) >= self.index.borrow().len() && !self.fragments_complete() {
                let more = self.run_fragment_backfill_step().await?;
                if !more {
                    break;
                }
            }
            if (frame as usize) < self.index.borrow().len() {
                Ok(())
            } else {
                Err(MediaError::FrameOutOfRange(frame, self.frame_count()))
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.kick_fragment_backfill();
            while (frame as usize) >= self.index.borrow().len() && !self.fragments_complete() {
                if let Some(err) = self
                    .fragment_backfill
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.error.clone())
                {
                    return Err(MediaError::Mp4(err));
                }
                crate::util::yield_now().await;
            }
            if (frame as usize) < self.index.borrow().len() {
                Ok(())
            } else {
                Err(MediaError::FrameOutOfRange(frame, self.frame_count()))
            }
        }
    }

    /// Ensure nearest sync ≤ `frame` is bitstream-verified; return that key.
    ///
    /// mediabunny: `verifyKeyPackets` on retrieve — demote lying stss and retry.
    pub async fn ensure_key_verified(&self, frame: FrameId) -> Result<FrameId, MediaError> {
        self.ensure_frame_indexed(frame).await?;
        if self.sync_verify.borrow().length_size.is_none() {
            return self
                .index
                .borrow()
                .nearest_keyframe(frame)
                .ok_or(MediaError::FrameOutOfRange(frame, self.frame_count()));
        }
        // Cap retries if every sync demotes (corrupt file).
        for _ in 0..64 {
            let key = self
                .index
                .borrow()
                .nearest_keyframe(frame)
                .ok_or(MediaError::FrameOutOfRange(frame, self.frame_count()))?;
            if self.sync_verify.borrow().checked.contains(&key) {
                return Ok(key);
            }
            let keep = self.verify_sync_candidate(key).await?;
            if keep {
                return Ok(key);
            }
        }
        Err(MediaError::Mp4(
            "no verified keyframe after sync demotions".into(),
        ))
    }

    /// Verify the next pending moov sync (background). `Ok(false)` when done.
    pub async fn verify_next_pending(&self) -> Result<bool, MediaError> {
        let next = {
            let sv = self.sync_verify.borrow();
            if sv.complete || sv.length_size.is_none() {
                return Ok(false);
            }
            sv.pending.front().copied()
        };
        let Some(frame) = next else {
            self.finish_sync_verify();
            return Ok(false);
        };
        let _ = self.verify_sync_candidate(frame).await?;
        Ok(!self.sync_verify.borrow().complete)
    }

    /// Block until every moov sync candidate is checked (scrub settle gate).
    pub async fn await_sync_verify_complete(&self) -> Result<(), MediaError> {
        while self.verify_next_pending().await? {
            crate::util::yield_now().await;
        }
        Ok(())
    }

    fn finish_sync_verify(&self) {
        let mut sv = self.sync_verify.borrow_mut();
        if sv.complete {
            return;
        }
        sv.pending.clear();
        sv.complete = true;
        drop(sv);
        let gop = self.index.borrow().gop_profile();
        self.metadata.borrow_mut().gop_profile = gop;
        self.sync_gen.set(self.sync_gen.get().saturating_add(1));
    }

    fn arm_deferred_sync_verify(&self, length_size: usize, codec: VideoCodec) {
        let pending: VecDeque<FrameId> = self.index.borrow().sync_frame_ids().into();
        let complete = pending.is_empty();
        *self.sync_verify.borrow_mut() = SyncVerify {
            pending,
            checked: HashSet::new(),
            complete,
            length_size: Some(length_size),
            codec: Some(codec),
        };
        if complete {
            self.finish_sync_verify();
        }
    }

    /// Bitstream-check one sync candidate. Returns whether it stays a key.
    async fn verify_sync_candidate(&self, frame: FrameId) -> Result<bool, MediaError> {
        if self.sync_verify.borrow().checked.contains(&frame) {
            return Ok(self
                .index
                .borrow()
                .get(frame)
                .map(|e| e.is_sync)
                .unwrap_or(false));
        }
        let (length_size, codec) = {
            let sv = self.sync_verify.borrow();
            let length_size = sv
                .length_size
                .ok_or_else(|| MediaError::Mp4("sync verify not armed".into()))?;
            let codec = sv
                .codec
                .ok_or_else(|| MediaError::Mp4("sync verify codec missing".into()))?;
            (length_size, codec)
        };
        let (offset, size) = {
            let index = self.index.borrow();
            let entry = index
                .get(frame)
                .ok_or(MediaError::FrameOutOfRange(frame, self.frame_count()))?;
            (entry.offset, entry.size)
        };
        let sample = self.sync_source.read(offset, size as usize).await?;
        let keep = sample.len() == size as usize
            && matches!(
                match codec {
                    VideoCodec::Avc => determine_avc_packet_type(&sample, length_size),
                    VideoCodec::Hevc => determine_hevc_packet_type(&sample, length_size),
                    VideoCodec::Other => None,
                },
                Some(PacketType::Key)
            );
        {
            let mut index = self.index.borrow_mut();
            let mut sv = self.sync_verify.borrow_mut();
            if !keep {
                index.set_sync(frame, false);
            }
            sv.checked.insert(frame);
            sv.pending.retain(|f| *f != frame);
            let done = sv.pending.is_empty();
            if done {
                sv.complete = true;
            }
            drop(sv);
            drop(index);
            if done {
                let gop = self.index.borrow().gop_profile();
                self.metadata.borrow_mut().gop_profile = gop;
            }
            if !keep || done {
                self.sync_gen.set(self.sync_gen.get().saturating_add(1));
            }
        }
        Ok(keep)
    }

    pub async fn encoded_sample(&self, frame: FrameId) -> Result<EncodedSample, MediaError> {
        self.ensure_frame_indexed(frame).await?;
        let entry = {
            let index = self.index.borrow();
            index
                .get(frame)
                .cloned()
                .ok_or(MediaError::FrameOutOfRange(frame, self.frame_count()))?
        };
        let data = self.source.read(entry.offset, entry.size as usize).await?;
        if data.len() != entry.size as usize {
            return Err(MediaError::Mp4(format!(
                "short sample read at {}: got {} want {}",
                entry.offset,
                data.len(),
                entry.size
            )));
        }

        Ok(EncodedSample {
            frame: entry.frame,
            decode_order: entry.decode_order,
            is_keyframe: entry.is_sync,
            decode_timestamp_ticks: entry.decode_timestamp_ticks,
            composition_timestamp_ticks: entry.composition_timestamp_ticks,
            duration_ticks: entry.duration_ticks,
            timescale: entry.timescale,
            data,
        })
    }

    /// Encoded samples from nearest keyframe through `frame`, decode order.
    pub async fn decode_cascade(&self, frame: FrameId) -> Result<Vec<EncodedSample>, MediaError> {
        let _ = self.ensure_key_verified(frame).await?;
        let window = self
            .index
            .borrow()
            .decode_window(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, self.frame_count()))?;
        let mut out = Vec::with_capacity(window.len());
        for f in window {
            out.push(self.encoded_sample(f).await?);
        }
        Ok(out)
    }

    /// Copy only samples with `decode_order` in `from_do..=to_do` (inclusive), decode order.
    pub async fn samples_decode_range(
        &self,
        from_do: usize,
        to_do: usize,
    ) -> Result<Vec<EncodedSample>, MediaError> {
        if from_do > to_do {
            return Ok(Vec::new());
        }
        let frames: Vec<FrameId> = {
            let index = self.index.borrow();
            let mut entries: Vec<_> = index
                .frames()
                .iter()
                .filter(|e| e.decode_order >= from_do && e.decode_order <= to_do)
                .map(|e| (e.decode_order, e.frame))
                .collect();
            entries.sort_by_key(|(d, _)| *d);
            entries.into_iter().map(|(_, f)| f).collect()
        };
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            out.push(self.encoded_sample(f).await?);
        }
        Ok(out)
    }

    /// Next `limit` samples after `after_do` within the GOP that starts at `key_do`.
    pub async fn samples_after_in_gop(
        &self,
        key_do: usize,
        after_do: usize,
        limit: usize,
    ) -> Result<Vec<EncodedSample>, MediaError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let frames: Vec<FrameId> = {
            let index = self.index.borrow();
            let next_key_do = index
                .frames()
                .iter()
                .filter(|e| e.is_sync && e.decode_order > key_do)
                .map(|e| e.decode_order)
                .min();
            let mut entries: Vec<_> = index
                .frames()
                .iter()
                .filter(|e| {
                    e.decode_order > after_do
                        && next_key_do.map(|nk| e.decode_order < nk).unwrap_or(true)
                })
                .map(|e| (e.decode_order, e.frame))
                .collect();
            entries.sort_by_key(|(d, _)| *d);
            entries.truncate(limit);
            entries.into_iter().map(|(_, f)| f).collect()
        };
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            out.push(self.encoded_sample(f).await?);
        }
        Ok(out)
    }

    /// Next `limit` samples after `after_do` in **track** decode order (crosses GOPs).
    pub async fn samples_after_decode(
        &self,
        after_do: usize,
        limit: usize,
    ) -> Result<Vec<EncodedSample>, MediaError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let frames: Vec<FrameId> = {
            let index = self.index.borrow();
            let mut entries: Vec<_> = index
                .frames()
                .iter()
                .filter(|e| e.decode_order > after_do)
                .map(|e| (e.decode_order, e.frame))
                .collect();
            entries.sort_by_key(|(d, _)| *d);
            entries.truncate(limit);
            entries.into_iter().map(|(_, f)| f).collect()
        };
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            out.push(self.encoded_sample(f).await?);
        }
        Ok(out)
    }
}

/// Demuxed video + optional audio from one MP4 parse.
#[derive(Clone, Debug)]
pub struct OpenedMp4 {
    pub video: Option<Mp4VideoTrack>,
    pub audio: Option<Mp4AudioTrack>,
    /// Resident [`CachedSource`] window for the demo gray scrubber strip.
    pub fetch_map: Arc<FetchMap>,
}

impl OpenedMp4 {
    pub fn require_video(self) -> Result<Mp4VideoTrack, MediaError> {
        self.video.ok_or(MediaError::NoVideoTrack)
    }

    pub fn require_av(self) -> Result<Self, MediaError> {
        if self.video.is_none() && self.audio.is_none() {
            return Err(MediaError::NoAvTrack);
        }
        Ok(self)
    }
}

/// Open an MP4 from an owned byte buffer (fixtures / sync API).
///
/// Still parses the full buffer with `re_mp4` (fast for small fixtures) and keeps
/// samples behind [`MemorySource`]. Prefer [`open_mp4_source`] for large Blobs.
pub fn open_mp4_bytes(bytes: Vec<u8>) -> Result<Mp4VideoTrack, MediaError> {
    open_mp4_media_bytes(bytes)?.require_video()
}

/// Like [`open_mp4_bytes`], but keeps an optional audio track.
pub fn open_mp4_media_bytes(bytes: Vec<u8>) -> Result<OpenedMp4, MediaError> {
    let bytes: Arc<[u8]> = Arc::from(bytes);
    let fetch_map = Arc::new(FetchMap::new());
    fetch_map.mark_full(bytes.len() as u64);
    let mp4 = Mp4::read_bytes(bytes.as_ref())?;
    let source: Arc<dyn MediaSource> = Arc::new(MemorySource::new(Arc::clone(&bytes)));
    let timecode_start = crate::demux::timecode::find_moov_box(bytes.as_ref()).and_then(|moov| {
        crate::demux::timecode::read_tmcd_start_sample_blocking(&mp4, moov, source.as_ref())
            .ok()
            .flatten()
    });
    build_opened(
        mp4,
        source,
        fetch_map,
        OpenMp4Options {
            verify_avc_sync: true,
            defer_avc_sync: false,
        },
        timecode_start,
    )?
    .require_av()
}

/// Progressive open: ranged `ftyp`+`moov`(+`moof`) → sparse `re_mp4` parse → index.
pub async fn open_mp4_source(
    source: Arc<dyn MediaSource>,
    options: OpenMp4Options,
) -> Result<Mp4VideoTrack, MediaError> {
    open_mp4_source_with_progress(source, options, |_| {}).await
}

/// Like [`open_mp4_source`], with UI progress callbacks.
pub async fn open_mp4_source_with_progress(
    source: Arc<dyn MediaSource>,
    options: OpenMp4Options,
    mut progress: impl FnMut(&str),
) -> Result<Mp4VideoTrack, MediaError> {
    let fetch_map = Arc::new(FetchMap::new());
    let parsed =
        crate::demux::parse_mp4_progressive_with_backfill(source.as_ref(), &mut progress).await?;
    // Verify separately (async) — `block_on` cannot drive BlobSource reads.
    let timecode_start =
        resolve_timecode_start(&parsed.mp4, parsed.moov_bytes.as_deref(), source.as_ref()).await;
    let mut opened = build_opened(
        parsed.mp4,
        source,
        fetch_map,
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
        timecode_start,
    )?;
    if let (Some(plan), Some(v)) = (parsed.backfill, opened.video.as_mut()) {
        v.arm_fragment_backfill(plan, Arc::clone(&v.sync_source));
        #[cfg(not(target_arch = "wasm32"))]
        {
            v.await_fragment_backfill().await?;
        }
    }
    apply_avc_sync_option(opened.video.as_mut(), options, &mut progress).await?;
    opened.require_video()
}

/// Open via progressive parse, then attach an 8 MiB [`CachedSource`] for sample I/O.
///
/// Demux/scan/verify run on the **raw** source so header reads do not prefetch
/// multi‑MiB windows. Ranges already loaded during open are **seeded** into the
/// cache so [`FetchMap`] (gray strip) covers `ftyp`/`moov` instead of a permanent
/// hole at the file head. Further sample I/O still uses the resident LRU.
pub async fn open_mp4_source_cached(
    source: Arc<dyn MediaSource>,
    options: OpenMp4Options,
) -> Result<Mp4VideoTrack, MediaError> {
    open_mp4_source_cached_with_progress(source, options, |_| {}).await
}

/// Like [`open_mp4_source_cached`], with UI progress callbacks.
pub async fn open_mp4_source_cached_with_progress(
    source: Arc<dyn MediaSource>,
    options: OpenMp4Options,
    mut progress: impl FnMut(&str),
) -> Result<Mp4VideoTrack, MediaError> {
    let fetch_map = Arc::new(FetchMap::new());
    let parsed =
        crate::demux::parse_mp4_progressive_with_backfill(source.as_ref(), &mut progress).await?;
    // Verify on the raw source first — CachedSource's 8 MiB prefetch on every
    // scattered keyframe would thrash WASM (~600 syncs on long 1080p → multi‑GB of Blob reads).
    let timecode_start =
        resolve_timecode_start(&parsed.mp4, parsed.moov_bytes.as_deref(), source.as_ref()).await;
    let mut opened = build_opened(
        parsed.mp4,
        Arc::clone(&source),
        Arc::clone(&fetch_map),
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
        timecode_start,
    )?;
    if let (Some(plan), Some(v)) = (parsed.backfill, opened.video.as_mut()) {
        v.arm_fragment_backfill(plan, Arc::clone(&source));
        #[cfg(not(target_arch = "wasm32"))]
        {
            v.await_fragment_backfill().await?;
        }
    }
    apply_avc_sync_option(opened.video.as_mut(), options, &mut progress).await?;
    let cached = wrap_cached_playback(source, fetch_map, parsed.cache_seed);
    if let Some(v) = opened.video.as_mut() {
        v.set_playback_source(Arc::clone(&cached));
    }
    if let Some(a) = opened.audio.as_mut() {
        a.set_source(Arc::clone(&cached));
    }
    opened.require_video()
}

/// Progressive cached open returning video + optional audio.
pub async fn open_mp4_media_source_cached(
    source: Arc<dyn MediaSource>,
    options: OpenMp4Options,
) -> Result<OpenedMp4, MediaError> {
    open_mp4_media_source_cached_with_progress(source, options, |_| {}).await
}

/// Like [`open_mp4_media_source_cached`], with UI progress callbacks.
pub async fn open_mp4_media_source_cached_with_progress(
    source: Arc<dyn MediaSource>,
    options: OpenMp4Options,
    mut progress: impl FnMut(&str),
) -> Result<OpenedMp4, MediaError> {
    let fetch_map = Arc::new(FetchMap::new());
    let parsed =
        crate::demux::parse_mp4_progressive_with_backfill(source.as_ref(), &mut progress).await?;
    let timecode_start =
        resolve_timecode_start(&parsed.mp4, parsed.moov_bytes.as_deref(), source.as_ref()).await;
    let mut opened = build_opened(
        parsed.mp4,
        Arc::clone(&source),
        Arc::clone(&fetch_map),
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
        timecode_start,
    )?;
    if let (Some(plan), Some(v)) = (parsed.backfill, opened.video.as_mut()) {
        v.arm_fragment_backfill(plan, Arc::clone(&source));
        #[cfg(not(target_arch = "wasm32"))]
        {
            progress("Indexing remaining fragments…");
            v.await_fragment_backfill().await?;
        }
    }
    apply_avc_sync_option(opened.video.as_mut(), options, &mut progress).await?;
    let cached = wrap_cached_playback(source, fetch_map, parsed.cache_seed);
    if let Some(v) = opened.video.as_mut() {
        v.set_playback_source(Arc::clone(&cached));
    }
    if let Some(a) = opened.audio.as_mut() {
        a.set_source(cached);
    }
    opened.require_av()
}

/// Attach [`CachedSource`] for playback and seed ranges already read during demux.
fn wrap_cached_playback(
    source: Arc<dyn MediaSource>,
    fetch_map: Arc<FetchMap>,
    cache_seed: Vec<(u64, Vec<u8>)>,
) -> Arc<dyn MediaSource> {
    let cached = CachedSource::with_fetch_map(source, fetch_map);
    cached.seed_windows(cache_seed);
    Arc::new(cached)
}

/// Open an MP4 from a filesystem path (native / tests).
#[cfg(not(target_arch = "wasm32"))]
pub fn open_mp4_path(path: impl AsRef<std::path::Path>) -> Result<Mp4VideoTrack, MediaError> {
    let bytes = std::fs::read(path)?;
    open_mp4_bytes(bytes)
}

/// Resolve embedded `tmcd` start before `build_opened` consumes `Mp4`.
async fn resolve_timecode_start(
    mp4: &Mp4,
    moov: Option<&[u8]>,
    source: &dyn MediaSource,
) -> Option<timecode::Timecode> {
    let moov = moov?;
    crate::demux::timecode::read_tmcd_start_sample(mp4, moov, source)
        .await
        .ok()
        .flatten()
}

fn build_opened(
    mp4: Mp4,
    source: Arc<dyn MediaSource>,
    fetch_map: Arc<FetchMap>,
    options: OpenMp4Options,
    timecode_start: Option<timecode::Timecode>,
) -> Result<OpenedMp4, MediaError> {
    let video = build_video_track(&mp4, Arc::clone(&source), options, timecode_start)?;
    let audio = build_audio_track(&mp4, source)?;
    Ok(OpenedMp4 {
        video,
        audio,
        fetch_map,
    })
}

fn build_video_track(
    mp4: &Mp4,
    source: Arc<dyn MediaSource>,
    options: OpenMp4Options,
    timecode_start: Option<timecode::Timecode>,
) -> Result<Option<Mp4VideoTrack>, MediaError> {
    let Some(track) = mp4
        .tracks()
        .values()
        .find(|t| t.kind == Some(TrackKind::Video))
    else {
        return Ok(None);
    };

    if track.samples.is_empty() {
        return Ok(None);
    }

    let codec_string = track.codec_string(mp4);
    let description = track
        .raw_codec_config(mp4)
        .ok_or(MediaError::UnsupportedCodec)?;

    let codec = classify_codec(codec_string.as_deref());

    let mut entries: Vec<FrameEntry> = track
        .samples
        .iter()
        .enumerate()
        .map(|(decode_order, sample)| FrameEntry {
            frame: 0,
            decode_order,
            is_sync: sample.is_sync,
            decode_timestamp_ticks: sample.decode_timestamp,
            composition_timestamp_ticks: sample.composition_timestamp,
            duration_ticks: sample.duration,
            timescale: sample.timescale,
            offset: sample.offset,
            size: sample.size,
        })
        .collect();

    // Eager sync only for MemorySource builds (fixtures). Progressive opens
    // pass verify_avc_sync: false here and apply deferred/eager after build.
    if options.verify_avc_sync && !options.defer_avc_sync {
        match codec {
            VideoCodec::Avc => {
                verify_avc_sync_flags(&mut entries, source.as_ref(), &description)?;
            }
            VideoCodec::Hevc => {
                verify_hevc_sync_flags(&mut entries, source.as_ref(), &description)?;
            }
            VideoCodec::Other => {}
        }
    }

    let index = FrameIndex::from_entries(entries);
    let frame_count = index.len() as u64;
    let gop_profile = index.gop_profile();

    let fps = if track.duration > 0 && frame_count > 0 {
        Some(frame_count as f64 * track.timescale as f64 / track.duration as f64)
    } else {
        None
    };

    let metadata = VideoMetadata {
        width: u32::from(track.width),
        height: u32::from(track.height),
        timescale: track.timescale,
        duration_ticks: track.duration,
        frame_count,
        fps,
        codec: CodecConfig {
            codec,
            codec_string,
            description,
        },
        gop_profile,
        timecode_start,
    };

    Ok(Some(Mp4VideoTrack {
        sync_source: Arc::clone(&source),
        source,
        track_id: track.track_id,
        metadata: RefCell::new(metadata),
        index: RefCell::new(index),
        sync_verify: RefCell::new(SyncVerify {
            complete: true,
            ..SyncVerify::default()
        }),
        sync_gen: Cell::new(0),
        fragment_backfill: RefCell::new(None),
    }))
}

fn classify_codec(codec_string: Option<&str>) -> VideoCodec {
    match codec_string {
        Some(s) if s.starts_with("avc1") || s.starts_with("avc3") => VideoCodec::Avc,
        Some(s) if s.starts_with("hvc1") || s.starts_with("hev1") => VideoCodec::Hevc,
        _ => VideoCodec::Other,
    }
}

/// Apply [`OpenMp4Options`] bitstream sync policy after moov build (AVC / HEVC).
async fn apply_avc_sync_option(
    track: Option<&mut Mp4VideoTrack>,
    options: OpenMp4Options,
    progress: &mut dyn FnMut(&str),
) -> Result<(), MediaError> {
    let Some(track) = track else {
        return Ok(());
    };
    if !options.verify_avc_sync {
        return Ok(());
    }
    let codec = track.metadata.borrow().codec.codec;
    let length_size = match codec {
        VideoCodec::Avc => avc_nal_length_size(&track.metadata.borrow().codec.description),
        VideoCodec::Hevc => hevc_nal_length_size(&track.metadata.borrow().codec.description),
        VideoCodec::Other => None,
    };
    let Some(length_size) = length_size else {
        return Ok(());
    };
    if options.defer_avc_sync {
        track.arm_deferred_sync_verify(length_size, codec);
        let n = track.sync_verify_pending();
        if n > 0 {
            progress(&format!(
                "Keyframe verify deferred ({n} candidates, background)…"
            ));
        }
        return Ok(());
    }
    track.arm_deferred_sync_verify(length_size, codec);
    let total = track.sync_verify_pending();
    progress(&format!("Verifying keyframes (0/{total})…"));
    let mut i = 0usize;
    while track.verify_next_pending().await? {
        i += 1;
        if i.is_multiple_of(4) {
            progress(&format!("Verifying keyframes ({i}/{total})…"));
            crate::util::yield_now().await;
        }
    }
    Ok(())
}

/// Clear `is_sync` when the AVCC AU is not a bitstream key (or undecidable).
///
/// Key = IDR, or exact Recovery Point SEI when the runtime accepts SEI keys
/// (`accept_avc_recovery_point_sei`; Chromium &lt;145 demotes them).
/// Only for immediately-ready sources (`MemorySource`) — uses [`block_on`].
fn verify_avc_sync_flags(
    entries: &mut [FrameEntry],
    source: &dyn MediaSource,
    avcc: &[u8],
) -> Result<(), MediaError> {
    let Some(length_size) = avc_nal_length_size(avcc) else {
        return Ok(());
    };
    for entry in entries.iter_mut().filter(|e| e.is_sync) {
        let sample = block_on(source.read(entry.offset, entry.size as usize))?;
        if sample.len() != entry.size as usize {
            entry.is_sync = false;
            continue;
        }
        match determine_avc_packet_type(&sample, length_size) {
            Some(PacketType::Key) => {}
            Some(PacketType::Delta) | None => entry.is_sync = false,
        }
    }
    Ok(())
}

/// Clear `is_sync` when the HVCC AU is not an IRAP bitstream key.
///
/// mediabunny: `determineVideoPacketType` hevc — NAL types 16…23.
fn verify_hevc_sync_flags(
    entries: &mut [FrameEntry],
    source: &dyn MediaSource,
    hvcc: &[u8],
) -> Result<(), MediaError> {
    let Some(length_size) = hevc_nal_length_size(hvcc) else {
        return Ok(());
    };
    for entry in entries.iter_mut().filter(|e| e.is_sync) {
        let sample = block_on(source.read(entry.offset, entry.size as usize))?;
        if sample.len() != entry.size as usize {
            entry.is_sync = false;
            continue;
        }
        match determine_hevc_packet_type(&sample, length_size) {
            Some(PacketType::Key) => {}
            Some(PacketType::Delta) | None => entry.is_sync = false,
        }
    }
    Ok(())
}
