//! MP4 / ISOBMFF demux via `re_mp4` + ranged [`MediaSource`].

use std::fmt;
use std::sync::Arc;

use re_mp4::{Mp4, TrackKind};

use crate::bitstream::{PacketType, avc_nal_length_size, determine_avc_packet_type};
use crate::demux::Mp4AudioTrack;
use crate::demux::audio::build_audio_track;
use crate::error::MediaError;
use crate::frame_index::{FrameEntry, FrameIndex};
use crate::source::{CachedSource, MediaSource, MemorySource};
use crate::types::{CodecConfig, EncodedSample, FrameId, VideoCodec, VideoMetadata};
use crate::util::block_on;

/// Options for [`open_mp4_source`].
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenMp4Options {
    /// Mediabunny-style bitstream sync verify (AVC IDR / Recovery Point SEI).
    /// For progressive/Blob opens this runs **async** after moov parse (do not
    /// use `block_on` on Blob).
    pub verify_avc_sync: bool,
}

/// Demuxed H.264/H.265 (etc.) video track with a presentation-order frame index.
///
/// Sample bytes are fetched on demand from [`MediaSource`] using **file-absolute**
/// offsets from the sample table (not stub-relative).
#[derive(Clone)]
pub struct Mp4VideoTrack {
    source: Arc<dyn MediaSource>,
    metadata: VideoMetadata,
    index: FrameIndex,
}

impl fmt::Debug for Mp4VideoTrack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mp4VideoTrack")
            .field("metadata", &self.metadata)
            .field("frames", &self.index.len())
            .field("source_size", &self.source.size())
            .finish()
    }
}

impl Mp4VideoTrack {
    pub fn metadata(&self) -> &VideoMetadata {
        &self.metadata
    }

    pub fn index(&self) -> &FrameIndex {
        &self.index
    }

    pub fn frame_count(&self) -> u64 {
        self.metadata.frame_count
    }

    pub fn source(&self) -> &Arc<dyn MediaSource> {
        &self.source
    }

    pub async fn encoded_sample(&self, frame: FrameId) -> Result<EncodedSample, MediaError> {
        let entry = self
            .index
            .get(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, self.frame_count()))?;
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
        let window = self
            .index
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
        let mut entries: Vec<_> = self
            .index
            .frames()
            .iter()
            .filter(|e| e.decode_order >= from_do && e.decode_order <= to_do)
            .collect();
        entries.sort_by_key(|e| e.decode_order);
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            out.push(self.encoded_sample(e.frame).await?);
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
        let next_key_do = self
            .index
            .frames()
            .iter()
            .filter(|e| e.is_sync && e.decode_order > key_do)
            .map(|e| e.decode_order)
            .min();
        let mut entries: Vec<_> = self
            .index
            .frames()
            .iter()
            .filter(|e| {
                e.decode_order > after_do
                    && next_key_do.map(|nk| e.decode_order < nk).unwrap_or(true)
            })
            .collect();
        entries.sort_by_key(|e| e.decode_order);
        entries.truncate(limit);
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            out.push(self.encoded_sample(e.frame).await?);
        }
        Ok(out)
    }
}

/// Demuxed video + optional audio from one MP4 parse.
#[derive(Clone, Debug)]
pub struct OpenedMp4 {
    pub video: Option<Mp4VideoTrack>,
    pub audio: Option<Mp4AudioTrack>,
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
    let mp4 = Mp4::read_bytes(bytes.as_ref())?;
    let source: Arc<dyn MediaSource> = Arc::new(MemorySource::new(Arc::clone(&bytes)));
    build_opened(
        mp4,
        source,
        OpenMp4Options {
            verify_avc_sync: true,
        },
    )?
    .require_av()
}

/// Progressive open: ranged `ftyp`+`moov` → sparse `re_mp4` parse → index (file offsets).
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
    let mp4 =
        crate::demux::parse_mp4_progressive_with_progress(source.as_ref(), &mut progress).await?;
    // Verify separately (async) — `block_on` cannot drive BlobSource reads.
    let mut opened = build_opened(
        mp4,
        source,
        OpenMp4Options {
            verify_avc_sync: false,
        },
    )?;
    if options.verify_avc_sync
        && let Some(track) = opened.video.as_mut()
    {
        verify_track_avc_sync_async(track, &mut progress).await?;
    }
    opened.require_video()
}

/// Open via progressive parse, then attach an 8 MiB [`CachedSource`] for sample I/O.
///
/// Important: demux/scan runs on the **raw** source so header reads do not prefetch
/// multi‑MiB windows from the middle of `mdat`.
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
    let mp4 =
        crate::demux::parse_mp4_progressive_with_progress(source.as_ref(), &mut progress).await?;
    // Verify on the raw source first — CachedSource's 8 MiB prefetch on every
    // scattered keyframe would thrash WASM (BBB ≈600 syncs → multi‑GB of Blob reads).
    let mut opened = build_opened(
        mp4,
        Arc::clone(&source),
        OpenMp4Options {
            verify_avc_sync: false,
        },
    )?;
    if options.verify_avc_sync
        && let Some(track) = opened.video.as_mut()
    {
        verify_track_avc_sync_async(track, &mut progress).await?;
    }
    let cached: Arc<dyn MediaSource> = Arc::new(CachedSource::new(source));
    if let Some(v) = opened.video.as_mut() {
        v.source = Arc::clone(&cached);
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
    let mp4 =
        crate::demux::parse_mp4_progressive_with_progress(source.as_ref(), &mut progress).await?;
    let mut opened = build_opened(
        mp4,
        Arc::clone(&source),
        OpenMp4Options {
            verify_avc_sync: false,
        },
    )?;
    if options.verify_avc_sync
        && let Some(track) = opened.video.as_mut()
    {
        verify_track_avc_sync_async(track, &mut progress).await?;
    }
    let cached: Arc<dyn MediaSource> = Arc::new(CachedSource::new(source));
    if let Some(v) = opened.video.as_mut() {
        v.source = Arc::clone(&cached);
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
    options: OpenMp4Options,
) -> Result<OpenedMp4, MediaError> {
    let video = build_video_track(&mp4, Arc::clone(&source), options)?;
    let audio = build_audio_track(&mp4, source)?;
    Ok(OpenedMp4 { video, audio })
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

    if options.verify_avc_sync && codec == VideoCodec::Avc {
        // Sync path only works for immediately-ready sources (MemorySource).
        // Progressive Blob open uses [`verify_avc_sync_flags_async`] after build.
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
        source,
        metadata,
        index,
    }))
}

fn classify_codec(codec_string: Option<&str>) -> VideoCodec {
    match codec_string {
        Some(s) if s.starts_with("avc1") || s.starts_with("avc3") => VideoCodec::Avc,
        Some(s) if s.starts_with("hvc1") || s.starts_with("hev1") => VideoCodec::Hevc,
        _ => VideoCodec::Other,
    }
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

/// Async mediabunny-style `verifyKeyPackets` for progressive / Blob sources.
async fn verify_track_avc_sync_async(
    track: &mut Mp4VideoTrack,
    progress: &mut dyn FnMut(&str),
) -> Result<(), MediaError> {
    if track.metadata.codec.codec != VideoCodec::Avc {
        return Ok(());
    }
    let Some(length_size) = avc_nal_length_size(&track.metadata.codec.description) else {
        return Ok(());
    };
    let sync_frames: Vec<FrameId> = track
        .index
        .frames()
        .iter()
        .filter(|e| e.is_sync)
        .map(|e| e.frame)
        .collect();
    let total = sync_frames.len();
    if total == 0 {
        return Ok(());
    }
    progress(&format!("Verifying keyframes (0/{total})…"));
    let mut demoted = 0usize;
    for (i, frame) in sync_frames.into_iter().enumerate() {
        if i % 4 == 0 {
            progress(&format!("Verifying keyframes ({}/{total})…", i + 1));
            crate::util::yield_now().await;
        }
        let (offset, size) = {
            let entry = track
                .index
                .get(frame)
                .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
            (entry.offset, entry.size)
        };
        let sample = track.source.read(offset, size as usize).await?;
        let keep = sample.len() == size as usize
            && matches!(
                determine_avc_packet_type(&sample, length_size),
                Some(PacketType::Key)
            );
        if !keep {
            track.index.set_sync(frame, false);
            demoted += 1;
        }
    }
    if demoted > 0 {
        progress(&format!(
            "Keyframe verify: demoted {demoted} lying sync flag(s)"
        ));
        #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
        web_sys::console::log_1(
            &format!("webmedia: demoted {demoted}/{total} lying stss sync flags").into(),
        );
        // Sync map changed — refresh scrub-policy profile.
        track.metadata.gop_profile = track.index.gop_profile();
    }
    Ok(())
}
