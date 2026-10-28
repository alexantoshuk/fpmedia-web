//! MP4 audio track demux (`mp4a` AAC + QT/ISO PCM via `re_mp4`).

use std::cell::{Cell, RefCell};
use std::fmt;
use std::sync::Arc;

use re_mp4::{Mp4, StsdBoxContent, TrackKind, is_pcm_sample_entry};

use crate::audio::pcm_entry::{find_pcm_in_moov, pcm_from_fourcc_only};
use crate::error::MediaError;
use crate::source::MediaSource;
use crate::types::{
    AudioCodec, AudioCodecConfig, AudioMetadata, EncodedAudioSample, Timestamp, build_aac_asc,
};

/// AAC (etc.) warm-ahead tile (`mediabunny: URLSource` min-load shape).
const WARM_TILE_BYTES: usize = 512 * 1024;
const WARM_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Zero-based audio sample index in track order.
pub type AudioSampleId = u64;

/// One audio sample's table entry (file-absolute offset).
#[derive(Clone, Debug)]
pub struct AudioSampleEntry {
    pub sample: AudioSampleId,
    pub is_sync: bool,
    pub decode_timestamp_ticks: i64,
    pub composition_timestamp_ticks: i64,
    pub duration_ticks: u64,
    pub timescale: u64,
    pub offset: u64,
    pub size: u64,
}

impl AudioSampleEntry {
    pub fn composition_timestamp(&self) -> Timestamp {
        Timestamp::from_ticks(self.composition_timestamp_ticks, self.timescale)
    }
}

/// Demuxed AAC (etc.) audio track — sample bytes fetched on demand from [`MediaSource`].
///
/// Sample table is interior-mutable so fMP4 mfra backfill can extend it after warm
/// open (`mediabunny: readFragment` grows tracks; we previously left audio at the
/// warm-moof sample count → silence near frame ~24).
pub struct Mp4AudioTrack {
    source: RefCell<Arc<dyn MediaSource>>,
    metadata: RefCell<AudioMetadata>,
    samples: RefCell<Vec<AudioSampleEntry>>,
    /// More moof audio may still arrive (warm open + bg backfill).
    expect_more: Cell<bool>,
}

impl Clone for Mp4AudioTrack {
    fn clone(&self) -> Self {
        Self {
            source: RefCell::new(Arc::clone(&self.source.borrow())),
            metadata: RefCell::new(self.metadata.borrow().clone()),
            samples: RefCell::new(self.samples.borrow().clone()),
            expect_more: Cell::new(self.expect_more.get()),
        }
    }
}

impl fmt::Debug for Mp4AudioTrack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mp4AudioTrack")
            .field("metadata", &*self.metadata.borrow())
            .field("samples", &self.samples.borrow().len())
            .field("expect_more", &self.expect_more.get())
            .finish()
    }
}

impl Mp4AudioTrack {
    pub fn metadata(&self) -> AudioMetadata {
        self.metadata.borrow().clone()
    }

    pub fn sample_count(&self) -> u64 {
        self.samples.borrow().len() as u64
    }

    pub fn source(&self) -> Arc<dyn MediaSource> {
        Arc::clone(&self.source.borrow())
    }

    pub(crate) fn set_source(&self, source: Arc<dyn MediaSource>) {
        *self.source.borrow_mut() = source;
    }

    /// Warm fMP4: pump must wait at EOF of the current table until backfill
    /// adopts a longer sample list (or clears this flag when complete).
    pub fn set_expect_more_samples(&self, more: bool) {
        self.expect_more.set(more);
    }

    pub fn expect_more_samples(&self) -> bool {
        self.expect_more.get()
    }

    /// Replace the sample table from a longer mfra parse (same codec config).
    pub fn adopt_fragment_samples(&self, fresh: &Mp4AudioTrack) {
        let new_len = fresh.samples.borrow().len();
        let old_len = self.samples.borrow().len();
        if new_len < old_len {
            return;
        }
        *self.samples.borrow_mut() = fresh.samples.borrow().clone();
        let mut meta = self.metadata.borrow_mut();
        let fresh_meta = fresh.metadata.borrow();
        meta.sample_count = fresh_meta.sample_count;
        meta.duration_ticks = fresh_meta.duration_ticks;
        meta.timescale = fresh_meta.timescale;
    }

    /// Append samples from a **delta** fragment parse (`readFragment` style).
    ///
    /// Unlike [`Self::adopt_fragment_samples`], `fresh` holds only the new moof's
    /// samples — not a full-file rebuild.
    pub fn append_fragment_samples(&self, fresh: &Mp4AudioTrack) {
        let mut add = fresh.samples.borrow().clone();
        if add.is_empty() {
            return;
        }
        let mut samples = self.samples.borrow_mut();
        let start_id = samples.len() as u64;
        for (i, s) in add.iter_mut().enumerate() {
            s.sample = start_id + i as u64;
        }
        samples.extend(add);
        let count = samples.len() as u64;
        let duration_ticks = samples.iter().map(|s| s.duration_ticks).sum();
        let timescale = samples
            .last()
            .map(|s| s.timescale)
            .unwrap_or_else(|| self.metadata.borrow().timescale);
        drop(samples);
        let mut meta = self.metadata.borrow_mut();
        meta.sample_count = count;
        meta.duration_ticks = duration_ticks;
        meta.timescale = timescale;
    }

    /// First sample whose composition time is ≥ `time_secs`, or the last sample.
    pub fn sample_at_or_after(&self, time_secs: f64) -> Option<AudioSampleId> {
        let samples = self.samples.borrow();
        if samples.is_empty() {
            return None;
        }
        let idx = samples.partition_point(|e| e.composition_timestamp().seconds < time_secs);
        Some(idx.min(samples.len().saturating_sub(1)) as AudioSampleId)
    }

    pub async fn encoded_sample(
        &self,
        sample: AudioSampleId,
    ) -> Result<EncodedAudioSample, MediaError> {
        let entry = {
            let samples = self.samples.borrow();
            samples
                .get(sample as usize)
                .cloned()
                .ok_or(MediaError::AudioSampleOutOfRange(
                    sample,
                    samples.len() as u64,
                ))?
        };
        let source = Arc::clone(&self.source.borrow());
        let data = source.read(entry.offset, entry.size as usize).await?;
        if data.len() != entry.size as usize {
            return Err(MediaError::Mp4(format!(
                "short audio sample read at {}: got {} want {}",
                entry.offset,
                data.len(),
                entry.size
            )));
        }
        Ok(EncodedAudioSample {
            sample: entry.sample,
            is_keyframe: entry.is_sync,
            decode_timestamp_ticks: entry.decode_timestamp_ticks,
            composition_timestamp_ticks: entry.composition_timestamp_ticks,
            duration_ticks: entry.duration_ticks,
            timescale: entry.timescale,
            data,
        })
    }

    /// Block until AAC (etc.) bytes through `time_secs` are in the playback cache.
    ///
    /// Same role as video [`crate::demux::Mp4VideoTrack::warm_sample_bytes`] —
    /// A/V lag together when only video `mdat` was warmed.
    pub async fn warm_bytes_through(&self, time_secs: f64) -> Result<(), MediaError> {
        let spans = {
            let samples = self.samples.borrow();
            if samples.is_empty() {
                return Ok(());
            }
            let end = samples.partition_point(|e| e.composition_timestamp().seconds <= time_secs);
            let end = end.max(1).min(samples.len());
            crate::demux::coalesce::coalesce_sample_spans(
                samples.iter().take(end).map(|e| (e.offset, e.size)),
                crate::demux::coalesce::SAMPLE_COALESCE_GAP,
            )
        };
        let source = Arc::clone(&self.source.borrow());
        let mut filled = 0usize;
        for (start, end) in spans {
            let mut off = start;
            while off < end && filled < WARM_MAX_BYTES {
                let chunk = ((end - off) as usize)
                    .min(WARM_TILE_BYTES)
                    .min(WARM_MAX_BYTES - filled);
                let _ = source.read(off, chunk).await?;
                off += chunk as u64;
                filled += chunk;
                crate::util::yield_now().await;
            }
            if filled >= WARM_MAX_BYTES {
                break;
            }
        }
        Ok(())
    }
}

/// Build the primary audio track from a parsed [`Mp4`], or `None` if absent.
///
/// `moov_bytes`: raw `moov` (with header) for PCM sound-description bodies
/// (`re_mp4` drops them on `StsdBoxContent::Unknown`). Optional for mfra deltas.
pub(crate) fn build_audio_track(
    mp4: &Mp4,
    source: Arc<dyn MediaSource>,
    moov_bytes: Option<&[u8]>,
) -> Result<Option<Mp4AudioTrack>, MediaError> {
    let Some((track_id, track)) = mp4
        .tracks()
        .iter()
        .find(|(_, t)| t.kind == Some(TrackKind::Audio))
        .map(|(id, t)| (*id, t))
    else {
        return Ok(None);
    };

    if track.samples.is_empty() {
        return Ok(None);
    }

    let stsd = &track.trak(mp4).mdia.minf.stbl.stsd.contents;
    let codec_meta = match stsd {
        StsdBoxContent::Mp4a(mp4a) => {
            let sample_rate = u32::from(mp4a.samplerate.value()).max(1);
            let number_of_channels = u32::from(mp4a.channelcount).max(1);

            let (profile, freq_index, chan_conf) = mp4a
                .esds
                .as_ref()
                .map(|esds| {
                    let d = &esds.es_desc.dec_config.dec_specific;
                    (d.profile, d.freq_index, d.chan_conf)
                })
                .unwrap_or((2, 4, number_of_channels.min(7) as u8));

            // Prefer ASC channel/rate when present (mediabunny updates from ASC).
            let number_of_channels = if chan_conf > 0 {
                u32::from(chan_conf)
            } else {
                number_of_channels
            };
            let sample_rate = freq_index_hz(freq_index).unwrap_or(sample_rate);

            let description = build_aac_asc(profile, freq_index, chan_conf.max(1));
            let codec_string = format!("mp4a.40.{profile}");
            AudioCodecConfig {
                codec: AudioCodec::Aac,
                codec_string: Some(codec_string),
                description,
                pcm: None,
            }
            .with_rate_channels(sample_rate, number_of_channels)
        }
        StsdBoxContent::Unknown(fourcc) if is_pcm_sample_entry(fourcc) => {
            let desc = moov_bytes
                .and_then(|moov| find_pcm_in_moov(moov, track_id))
                .or_else(|| {
                    // Fragment delta / no moov: only ulaw/alaw are unambiguous.
                    pcm_from_fourcc_only(fourcc).ok().flatten().map(|format| {
                        crate::audio::pcm_entry::PcmSoundDescription {
                            track_id,
                            fourcc: fourcc.value,
                            sample_rate: track.timescale.max(1) as u32,
                            number_of_channels: 2,
                            format,
                        }
                    })
                });
            let Some(desc) = desc else {
                // Need sound-description body for in24/etc. — ignore track.
                return Ok(None);
            };
            AudioCodecConfig {
                codec: AudioCodec::Pcm,
                codec_string: Some(desc.format.codec_string()),
                description: Vec::new(),
                pcm: Some(desc.format),
            }
            .with_rate_channels(desc.sample_rate, desc.number_of_channels)
        }
        _ => return Ok(None),
    };

    let samples: Vec<AudioSampleEntry> = track
        .samples
        .iter()
        .enumerate()
        .map(|(i, sample)| AudioSampleEntry {
            sample: i as AudioSampleId,
            is_sync: sample.is_sync,
            decode_timestamp_ticks: sample.decode_timestamp,
            composition_timestamp_ticks: sample.composition_timestamp,
            duration_ticks: sample.duration,
            timescale: sample.timescale,
            offset: sample.offset,
            size: sample.size,
        })
        .collect();

    let metadata = AudioMetadata {
        sample_rate: codec_meta.sample_rate,
        number_of_channels: codec_meta.number_of_channels,
        timescale: track.timescale,
        duration_ticks: track.duration,
        sample_count: samples.len() as u64,
        codec: codec_meta.config,
    };

    Ok(Some(Mp4AudioTrack {
        source: RefCell::new(source),
        metadata: RefCell::new(metadata),
        samples: RefCell::new(samples),
        expect_more: Cell::new(false),
    }))
}

struct CodecMeta {
    config: AudioCodecConfig,
    sample_rate: u32,
    number_of_channels: u32,
}

trait WithRateChannels {
    fn with_rate_channels(self, sample_rate: u32, number_of_channels: u32) -> CodecMeta;
}

impl WithRateChannels for AudioCodecConfig {
    fn with_rate_channels(self, sample_rate: u32, number_of_channels: u32) -> CodecMeta {
        CodecMeta {
            config: self,
            sample_rate,
            number_of_channels,
        }
    }
}

fn freq_index_hz(index: u8) -> Option<u32> {
    Some(match index {
        0 => 96000,
        1 => 88200,
        2 => 64000,
        3 => 48000,
        4 => 44100,
        5 => 32000,
        6 => 24000,
        7 => 22050,
        8 => 16000,
        9 => 12000,
        10 => 11025,
        11 => 8000,
        12 => 7350,
        _ => return None,
    })
}
