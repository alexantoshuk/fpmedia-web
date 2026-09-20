//! MP4 / ISOBMFF demux via `re_mp4` + ranged [`MediaSource`].

use std::cell::{Cell, Ref, RefCell};
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use re_mp4::{Mp4, TrackKind};

use crate::bitstream::{PacketType, avc_nal_length_size, determine_avc_packet_type};
use crate::demux::Mp4AudioTrack;
use crate::demux::audio::build_audio_track;
use crate::error::MediaError;
use crate::frame_index::{FrameEntry, FrameIndex};
use crate::source::{CachedSource, FetchMap, MediaSource, MemorySource};
use crate::types::{CodecConfig, EncodedSample, FrameId, VideoCodec, VideoMetadata};
use crate::util::block_on;

/// Options for [`open_mp4_source`].
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenMp4Options {
    /// Mediabunny-style bitstream sync verify (AVC IDR / Recovery Point SEI).
    pub verify_avc_sync: bool,
    /// When [`Self::verify_avc_sync`], do not block open on a full scan — queue
    /// candidates for background / on-demand [`Mp4VideoTrack::ensure_key_verified`]
    /// (`mediabunny: verifyKeyPackets` on retrieve). Eager when false (tests).
    pub defer_avc_sync: bool,
}

/// Deferred AVC sync-verify bookkeeping (moov flags until bitstream-checked).
#[derive(Clone, Debug, Default)]
struct SyncVerify {
    pending: VecDeque<FrameId>,
    checked: HashSet<FrameId>,
    complete: bool,
    length_size: Option<usize>,
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
    metadata: RefCell<VideoMetadata>,
    index: RefCell<FrameIndex>,
    sync_verify: RefCell<SyncVerify>,
    /// Bumped when sync table changes (bg demote) so hosts can refresh GOP UI.
    sync_gen: Cell<u64>,
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

    /// Ensure nearest sync ≤ `frame` is bitstream-verified; return that key.
    ///
    /// mediabunny: `verifyKeyPackets` on retrieve — demote lying stss and retry.
    pub async fn ensure_key_verified(&self, frame: FrameId) -> Result<FrameId, MediaError> {
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

    fn arm_deferred_sync_verify(&self, length_size: usize) {
        let pending: VecDeque<FrameId> = self.index.borrow().sync_frame_ids().into();
        let complete = pending.is_empty();
        *self.sync_verify.borrow_mut() = SyncVerify {
            pending,
            checked: HashSet::new(),
            complete,
            length_size: Some(length_size),
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
        let length_size = self
            .sync_verify
            .borrow()
            .length_size
            .ok_or_else(|| MediaError::Mp4("sync verify not armed".into()))?;
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
                determine_avc_packet_type(&sample, length_size),
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
    build_opened(
        mp4,
        source,
        fetch_map,
        OpenMp4Options {
            verify_avc_sync: true,
            defer_avc_sync: false,
        },
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
    let mp4 =
        crate::demux::parse_mp4_progressive_with_progress(source.as_ref(), &mut progress).await?;
    // Verify separately (async) — `block_on` cannot drive BlobSource reads.
    let mut opened = build_opened(
        mp4,
        source,
        fetch_map,
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
    )?;
    apply_avc_sync_option(opened.video.as_mut(), options, &mut progress).await?;
    opened.require_video()
}

/// Open via progressive parse, then attach an 8 MiB [`CachedSource`] for sample I/O.
///
/// Demux/scan/verify run on the **raw** source so header reads do not prefetch
/// multi‑MiB windows. The scrubber gray strip follows the **resident** cache
/// window only ([`CachedSource`] + [`FetchMap`]), not historical Range unions.
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
    let mp4 =
        crate::demux::parse_mp4_progressive_with_progress(source.as_ref(), &mut progress).await?;
    // Verify on the raw source first — CachedSource's 8 MiB prefetch on every
    // scattered keyframe would thrash WASM (~600 syncs on long 1080p → multi‑GB of Blob reads).
    let mut opened = build_opened(
        mp4,
        Arc::clone(&source),
        Arc::clone(&fetch_map),
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
    )?;
    apply_avc_sync_option(opened.video.as_mut(), options, &mut progress).await?;
    let cached: Arc<dyn MediaSource> = Arc::new(CachedSource::with_fetch_map(source, fetch_map));
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
    let mp4 =
        crate::demux::parse_mp4_progressive_with_progress(source.as_ref(), &mut progress).await?;
    let mut opened = build_opened(
        mp4,
        Arc::clone(&source),
        Arc::clone(&fetch_map),
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
    )?;
    apply_avc_sync_option(opened.video.as_mut(), options, &mut progress).await?;
    let cached: Arc<dyn MediaSource> = Arc::new(CachedSource::with_fetch_map(source, fetch_map));
    if let Some(v) = opened.video.as_mut() {
        v.set_playback_source(Arc::clone(&cached));
    }
    if let Some(a) = opened.audio.as_mut() {
        a.set_source(cached);
    }
    opened.require_av()
}

/// Open an MP4 from a filesystem path (native / tests).
#[cfg(not(target_arch = "wasm32"))]
pub fn open_mp4_path(path: impl AsRef<std::path::Path>) -> Result<Mp4VideoTrack, MediaError> {
    let bytes = std::fs::read(path)?;
    open_mp4_bytes(bytes)
}

fn build_opened(
    mp4: Mp4,
    source: Arc<dyn MediaSource>,
    fetch_map: Arc<FetchMap>,
    options: OpenMp4Options,
) -> Result<OpenedMp4, MediaError> {
    let video = build_video_track(&mp4, Arc::clone(&source), options)?;
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
    if options.verify_avc_sync && !options.defer_avc_sync && codec == VideoCodec::Avc {
        verify_avc_sync_flags(&mut entries, source.as_ref(), &description)?;
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
    };

    Ok(Some(Mp4VideoTrack {
        sync_source: Arc::clone(&source),
        source,
        metadata: RefCell::new(metadata),
        index: RefCell::new(index),
        sync_verify: RefCell::new(SyncVerify {
            complete: true,
            ..SyncVerify::default()
        }),
        sync_gen: Cell::new(0),
    }))
}

fn classify_codec(codec_string: Option<&str>) -> VideoCodec {
    match codec_string {
        Some(s) if s.starts_with("avc1") || s.starts_with("avc3") => VideoCodec::Avc,
        Some(s) if s.starts_with("hvc1") || s.starts_with("hev1") => VideoCodec::Hevc,
        _ => VideoCodec::Other,
    }
}

/// Apply [`OpenMp4Options`] AVC sync policy after moov build.
async fn apply_avc_sync_option(
    track: Option<&mut Mp4VideoTrack>,
    options: OpenMp4Options,
    progress: &mut dyn FnMut(&str),
) -> Result<(), MediaError> {
    let Some(track) = track else {
        return Ok(());
    };
    if !options.verify_avc_sync || track.metadata.borrow().codec.codec != VideoCodec::Avc {
        return Ok(());
    }
    let Some(length_size) = avc_nal_length_size(&track.metadata.borrow().codec.description) else {
        return Ok(());
    };
    if options.defer_avc_sync {
        track.arm_deferred_sync_verify(length_size);
        let n = track.sync_verify_pending();
        if n > 0 {
            progress(&format!(
                "Keyframe verify deferred ({n} candidates, background)…"
            ));
        }
        return Ok(());
    }
    track.arm_deferred_sync_verify(length_size);
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
