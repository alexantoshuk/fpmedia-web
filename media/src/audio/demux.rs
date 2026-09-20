//! MP4 audio track demux (`mp4a` / AAC via `re_mp4`).

use std::fmt;
use std::sync::Arc;

use re_mp4::{Mp4, StsdBoxContent, TrackKind};

use crate::error::MediaError;
use crate::source::MediaSource;
use crate::types::{
    AudioCodec, AudioCodecConfig, AudioMetadata, EncodedAudioSample, Timestamp, build_aac_asc,
};

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
#[derive(Clone)]
pub struct Mp4AudioTrack {
    source: Arc<dyn MediaSource>,
    metadata: AudioMetadata,
    samples: Vec<AudioSampleEntry>,
}

impl fmt::Debug for Mp4AudioTrack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mp4AudioTrack")
            .field("metadata", &self.metadata)
            .field("samples", &self.samples.len())
            .finish()
    }
}

impl Mp4AudioTrack {
    pub fn metadata(&self) -> &AudioMetadata {
        &self.metadata
    }

    pub fn sample_count(&self) -> u64 {
        self.samples.len() as u64
    }

    pub fn samples(&self) -> &[AudioSampleEntry] {
        &self.samples
    }

    pub fn source(&self) -> &Arc<dyn MediaSource> {
        &self.source
    }

    pub(crate) fn set_source(&mut self, source: Arc<dyn MediaSource>) {
        self.source = source;
    }

    /// First sample whose composition time is ≥ `time_secs`, or the last sample.
    pub fn sample_at_or_after(&self, time_secs: f64) -> Option<AudioSampleId> {
        if self.samples.is_empty() {
            return None;
        }
        let idx = self
            .samples
            .partition_point(|e| e.composition_timestamp().seconds < time_secs);
        Some(idx.min(self.samples.len().saturating_sub(1)) as AudioSampleId)
    }

    pub async fn encoded_sample(
        &self,
        sample: AudioSampleId,
    ) -> Result<EncodedAudioSample, MediaError> {
        let entry = self
            .samples
            .get(sample as usize)
            .ok_or(MediaError::AudioSampleOutOfRange(
                sample,
                self.sample_count(),
            ))?;
        let data = self.source.read(entry.offset, entry.size as usize).await?;
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
}

/// Build the primary audio track from a parsed [`Mp4`], or `None` if absent.
pub(crate) fn build_audio_track(
    mp4: &Mp4,
    source: Arc<dyn MediaSource>,
) -> Result<Option<Mp4AudioTrack>, MediaError> {
    let Some(track) = mp4
        .tracks()
        .values()
        .find(|t| t.kind == Some(TrackKind::Audio))
    else {
        return Ok(None);
    };

    if track.samples.is_empty() {
        return Ok(None);
    }

    let stsd = &track.trak(mp4).mdia.minf.stbl.stsd.contents;
    let StsdBoxContent::Mp4a(mp4a) = stsd else {
        return Err(MediaError::UnsupportedCodec);
    };

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

    // Prefer ASC channel/rate when present (mediabunny updates from AudioSpecificConfig).
    let number_of_channels = if chan_conf > 0 {
        u32::from(chan_conf)
    } else {
        number_of_channels
    };
    let sample_rate = freq_index_hz(freq_index).unwrap_or(sample_rate);

    let description = build_aac_asc(profile, freq_index, chan_conf.max(1));
    let codec_string = format!("mp4a.40.{profile}");

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
        sample_rate,
        number_of_channels,
        timescale: track.timescale,
        duration_ticks: track.duration,
        sample_count: samples.len() as u64,
        codec: AudioCodecConfig {
            codec: AudioCodec::Aac,
            codec_string: Some(codec_string),
            description,
        },
    };

    Ok(Some(Mp4AudioTrack {
        source,
        metadata,
        samples,
    }))
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
