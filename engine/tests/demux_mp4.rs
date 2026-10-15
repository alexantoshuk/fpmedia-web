//! Integration tests: demux + frame index against short-GOP H.264 fixtures.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use fpmedia_web::{
    GopProfile, MediaSource, MemorySource, Mp4VideoTrack, OpenMp4Options, OpenPhase, OpenedMp4,
    PacketType, ReadFuture, VideoCodec, avc_nal_length_size, determine_avc_packet_type, open_mp4,
    open_mp4_bytes, open_mp4_path,
};

#[cfg(feature = "hevc")]
use fpmedia_web::{determine_hevc_packet_type, hevc_nal_length_size};

/// Progressive cached open, video only.
async fn open_video_source(
    source: Arc<dyn MediaSource>,
    options: OpenMp4Options,
) -> Result<Mp4VideoTrack, fpmedia_web::MediaError> {
    open_mp4(source, options, |_| {}).await?.require_video()
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn block_on<F: Future>(fut: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut fut = Box::pin(fut);
    for _ in 0..256 {
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(v) => return v,
            std::task::Poll::Pending => continue,
        }
    }
    panic!("block_on: future stayed pending");
}

#[test]
fn demux_short_gop_metadata_and_frames() {
    let path = fixture("short_gop.mp4");
    if !path.exists() {
        eprintln!("skip: missing fixture {}", path.display());
        return;
    }
    let track = open_mp4_path(&path).expect("open fixture");
    let meta = track.metadata();

    assert_eq!(meta.width, 320);
    assert_eq!(meta.height, 240);
    assert_eq!(meta.frame_count, 48);
    assert_eq!(meta.codec.codec, VideoCodec::Avc);
    assert!(
        meta.codec
            .codec_string
            .as_deref()
            .is_some_and(|s| s.starts_with("avc1")),
        "codec_string={:?}",
        meta.codec.codec_string
    );
    assert!(!meta.codec.description.is_empty());

    let fps = meta.fps.expect("fps");
    assert!((fps - 24.0).abs() < 0.01, "fps={fps}");

    let (syncs, nearest_checks, window) = {
        let index = track.index();
        let syncs: Vec<_> = index
            .frames()
            .iter()
            .filter(|e| e.is_sync)
            .map(|e| e.frame)
            .collect();
        assert_eq!(syncs, vec![0, 12, 24, 36]);
        assert_eq!(meta.gop_profile, GopProfile::ShortFixed { len: 12 });
        assert_eq!(meta.gop_profile.label(), "GOP 12");
        // 320×240 fits entirely under the default MiB budget.
        assert_eq!(meta.scrub_cache_frame_budget(), 48);

        let nearest_checks = (
            index.nearest_keyframe(0),
            index.nearest_keyframe(11),
            index.nearest_keyframe(12),
            index.nearest_keyframe(25),
        );
        let window = index.decode_window(14).expect("window");
        (syncs, nearest_checks, window)
    };

    // verifyKeyPackets: every remaining sync is a bitstream key (IDR / exact SEI).
    let length_size = avc_nal_length_size(&meta.codec.description).expect("avcC length");
    for &frame in &syncs {
        let sample = block_on(track.encoded_sample(frame)).expect("sample");
        assert_eq!(
            determine_avc_packet_type(&sample.data, length_size),
            Some(PacketType::Key),
            "sync frame {frame} must be bitstream key"
        );
    }

    assert_eq!(nearest_checks.0, Some(0));
    assert_eq!(nearest_checks.1, Some(0));
    assert_eq!(nearest_checks.2, Some(12));
    assert_eq!(nearest_checks.3, Some(24));

    assert_eq!(window.first().copied(), Some(12));
    assert_eq!(window.last().copied(), Some(14));
    assert_eq!(window.len(), 3);

    let samples = block_on(track.decode_cascade(14)).expect("cascade");
    assert_eq!(samples.last().map(|s| s.frame), Some(14));
    assert!(samples.first().is_some_and(|s| s.is_keyframe));
}

#[test]
fn demux_with_b_frames_reorder_windows() {
    let path = fixture("with_b_frames.mp4");
    if !path.exists() {
        eprintln!("skip: missing fixture {}", path.display());
        return;
    }
    let track = open_mp4_path(&path).expect("open fixture");
    {
        let index = track.index();
        assert_eq!(track.frame_count(), 48);

        // Presentation frame 1 is a B (decode after the upcoming P).
        let f1 = index.get(1).expect("frame 1");
        let f3 = index.get(3).expect("frame 3");
        assert!(
            f1.decode_order > f3.decode_order,
            "B before P in presentation"
        );
        assert_eq!(f1.decode_order, 2);
        assert_eq!(f3.decode_order, 1);

        // Window for B1 must include the later P (presentation id 3).
        let w1 = index.decode_window(1).expect("window 1");
        assert_eq!(w1, vec![0, 3, 1]);

        // Seeking to P alone only needs I + P.
        let w3 = index.decode_window(3).expect("window 3");
        assert_eq!(w3, vec![0, 3]);
    }

    let samples = block_on(track.decode_cascade(1)).expect("cascade");
    assert_eq!(
        samples.iter().map(|s| s.frame).collect::<Vec<_>>(),
        vec![0, 3, 1]
    );
    assert!(samples[0].is_keyframe);
    assert!(!samples[1].is_keyframe);
}

#[test]
fn progressive_source_open_matches_bytes_offsets() {
    let path = fixture("short_gop.mp4");
    if !path.exists() {
        eprintln!("skip: missing fixture {}", path.display());
        return;
    }
    let bytes = std::fs::read(&path).expect("read");
    let full = open_mp4_path(&path).expect("full open");
    let source: Arc<dyn fpmedia_web::MediaSource> = Arc::new(MemorySource::from_vec(bytes));
    let progressive = block_on(open_video_source(
        source,
        OpenMp4Options {
            verify_avc_sync: true,
            defer_avc_sync: false,
        },
    ))
    .expect("progressive open");

    assert_eq!(full.frame_count(), progressive.frame_count());
    for (a, b) in full
        .index()
        .frames()
        .iter()
        .zip(progressive.index().frames())
    {
        assert_eq!(a.offset, b.offset);
        assert_eq!(a.size, b.size);
        assert_eq!(a.is_sync, b.is_sync);
        assert_eq!(a.composition_timestamp_ticks, b.composition_timestamp_ticks);
    }

    let sample_full = block_on(full.encoded_sample(12)).expect("full sample");
    let sample_prog = block_on(progressive.encoded_sample(12)).expect("prog sample");
    assert_eq!(sample_full.data, sample_prog.data);
}

#[test]
fn progressive_cached_open_seeds_fetch_map_at_file_head() {
    let path = fixture("short_gop.mp4");
    if !path.exists() {
        eprintln!("skip: missing fixture {}", path.display());
        return;
    }
    let bytes = std::fs::read(&path).expect("read");
    let file_size = bytes.len() as u64;
    let source: Arc<dyn fpmedia_web::MediaSource> = Arc::new(MemorySource::from_vec(bytes));
    let opened = block_on(open_mp4(
        source,
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
        |_| {},
    ))
    .expect("cached progressive open");

    let fracs = opened.fetch_map.file_fractions(file_size);
    assert!(
        !fracs.is_empty(),
        "FetchMap must cover progressive-open ranges at open"
    );
    assert_eq!(
        fracs[0].0, 0.0,
        "gray strip must not leave a permanent hole at file start; got {fracs:?}"
    );
    assert!(fracs[0].1 > 0.0);
}

#[test]
fn progressive_fragmented_mp4_open_matches_bytes() {
    let path = fixture("fragmented_tiny.mp4");
    if !path.exists() {
        eprintln!(
            "skip: missing fixture {} (generate locally or run progressive_fmp4_from_url --ignored)",
            path.display()
        );
        return;
    }
    let bytes = std::fs::read(&path).expect("read");
    assert_progressive_fmp4_matches_full(bytes);
}

/// testvideo fMP4 plate (AVC 1080p 24fps fragmented GOP12). Override with `FPMEDIA_WEB_FMP4_URL`.
const DEFAULT_FMP4_URL: &str = "https://github.com/alexantoshuk/testvideo/releases/download/v0.1.0/avc_1080p_24_fmp4_gop12_bf0.mp4";

fn fetch_url_bytes(url: &str) -> Result<Vec<u8>, String> {
    let output = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "180", url])
        .output()
        .map_err(|e| format!("curl spawn: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "curl failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if output.stdout.len() < 64 {
        return Err(format!(
            "short download ({} bytes) from {url}",
            output.stdout.len()
        ));
    }
    Ok(output.stdout)
}

/// Counts `read` calls / max `len` so mfra open stays moof-sized, not full-file.
struct CountingSource {
    inner: MemorySource,
    reads: AtomicUsize,
    max_len: AtomicUsize,
}

impl MediaSource for CountingSource {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.max_len.fetch_max(len, Ordering::Relaxed);
        self.inner.read(offset, len)
    }
}

fn assert_progressive_fmp4_matches_full(bytes: Vec<u8>) {
    let full = fpmedia_web::open_mp4_bytes(bytes.clone())
        .and_then(OpenedMp4::require_video)
        .expect("full open");
    let source: Arc<dyn fpmedia_web::MediaSource> = Arc::new(MemorySource::from_vec(bytes));
    let progressive = block_on(open_video_source(
        source,
        OpenMp4Options {
            verify_avc_sync: true,
            defer_avc_sync: false,
        },
    ))
    .expect("progressive fMP4 open");

    assert_eq!(full.frame_count(), progressive.frame_count());
    assert!(full.frame_count() > 0);
    for (a, b) in full
        .index()
        .frames()
        .iter()
        .zip(progressive.index().frames())
    {
        assert_eq!(a.offset, b.offset);
        assert_eq!(a.size, b.size);
        assert_eq!(a.is_sync, b.is_sync);
    }

    let sample_full = block_on(full.encoded_sample(0)).expect("full sample");
    let sample_prog = block_on(progressive.encoded_sample(0)).expect("prog sample");
    assert_eq!(sample_full.data, sample_prog.data);
    assert!(sample_full.is_keyframe);
}

/// Network fMP4 open — not run in default CI.
///
/// ```sh
/// cargo test -p fpmedia-web --test demux_mp4 progressive_fmp4_from_url -- --ignored --nocapture
/// ```
#[test]
#[ignore = "network: downloads fMP4 from testvideo Release (or FPMEDIA_WEB_FMP4_URL)"]
fn progressive_fmp4_from_url() {
    let url =
        std::env::var("FPMEDIA_WEB_FMP4_URL").unwrap_or_else(|_| DEFAULT_FMP4_URL.to_string());
    let t_dl = Instant::now();
    let bytes = fetch_url_bytes(&url).unwrap_or_else(|e| panic!("fetch {url}: {e}"));
    eprintln!(
        "download {:.1} MB in {:.2}s",
        bytes.len() as f64 / (1024.0 * 1024.0),
        t_dl.elapsed().as_secs_f64()
    );

    // Moof-only mfra path: many small reads (not one 41 MB dump into WASM).
    let counted = Arc::new(CountingSource {
        inner: MemorySource::from_vec(bytes.clone()),
        reads: AtomicUsize::new(0),
        max_len: AtomicUsize::new(0),
    });
    // Snapshot at `WarmingMediaBytes`: demux + backfill run on the raw source;
    // the playback warm that follows reads `CachedSource` windows by design.
    let demux_reads = std::cell::Cell::new(None);
    let t_open = Instant::now();
    let progressive = block_on(open_mp4(
        counted.clone() as Arc<dyn MediaSource>,
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
        |phase| {
            if phase == OpenPhase::WarmingMediaBytes && demux_reads.get().is_none() {
                demux_reads.set(Some((
                    counted.reads.load(Ordering::Relaxed),
                    counted.max_len.load(Ordering::Relaxed),
                )));
            }
        },
    ))
    .and_then(OpenedMp4::require_video)
    .expect("progressive fMP4 open");
    let open_s = t_open.elapsed().as_secs_f64();
    let (reads, max_len) = demux_reads.get().expect("open reached WarmingMediaBytes");
    eprintln!("progressive open {open_s:.3}s; reads={reads} max_len={max_len}");
    assert!(
        open_s < 5.0,
        "progressive open too slow ({open_s:.2}s) on MemorySource"
    );
    assert!(
        reads >= 100,
        "too few reads ({reads}); expected per-moof mfra path (~120 moofs)"
    );
    assert!(
        max_len <= 256 * 1024,
        "largest read {max_len} B — must not bulk-download mdat (WASM freeze)"
    );
    assert!(progressive.frame_count() > 0);

    assert_progressive_fmp4_matches_full(bytes);
}

#[test]
fn deferred_avc_sync_verify_on_demand() {
    let path = fixture("short_gop.mp4");
    if !path.exists() {
        eprintln!("skip: missing fixture {}", path.display());
        return;
    }
    let bytes = std::fs::read(&path).expect("read");
    let source: Arc<dyn fpmedia_web::MediaSource> = Arc::new(MemorySource::from_vec(bytes));
    let track = block_on(open_video_source(
        source,
        OpenMp4Options {
            verify_avc_sync: true,
            defer_avc_sync: true,
        },
    ))
    .expect("deferred open");

    assert!(!track.sync_verify_complete());
    assert!(track.sync_verify_pending() > 0);

    let key = block_on(track.ensure_key_verified(14)).expect("ensure key");
    assert_eq!(key, 12);
    assert!(track.sync_verify_pending() < 4);

    block_on(track.await_sync_verify_complete()).expect("finish verify");
    assert!(track.sync_verify_complete());
    assert_eq!(track.sync_verify_pending(), 0);
    assert_eq!(
        track.metadata().gop_profile,
        GopProfile::ShortFixed { len: 12 }
    );
}

#[test]
fn demux_with_audio_track() {
    use fpmedia_web::{AudioCodec, open_mp4_bytes};

    let path = fixture("with_audio.mp4");
    if !path.exists() {
        eprintln!("skip: missing fixture {}", path.display());
        return;
    }
    let bytes = std::fs::read(&path).expect("read fixture");
    let opened = open_mp4_bytes(bytes).expect("open media");
    let video = opened.video.expect("video");
    let audio = opened.audio.expect("audio");

    assert!(video.frame_count() > 0);
    assert_eq!(audio.metadata().codec.codec, AudioCodec::Aac);
    assert!(
        audio
            .metadata()
            .codec
            .codec_string
            .as_deref()
            .is_some_and(|s| s.starts_with("mp4a.40.")),
        "codec_string={:?}",
        audio.metadata().codec.codec_string
    );
    assert_eq!(audio.metadata().codec.description.len(), 2);
    assert!(audio.metadata().sample_rate >= 8000);
    assert!(audio.metadata().number_of_channels >= 1);
    assert!(audio.sample_count() > 0);

    let sample = block_on(audio.encoded_sample(0)).expect("audio sample 0");
    assert!(!sample.data.is_empty());
    assert!(sample.composition_timestamp().seconds >= 0.0);
}

/// Ranged native file reads for large QT masters (FileSource still deferred).
struct NativeFileSource {
    path: std::path::PathBuf,
    size: u64,
}

impl NativeFileSource {
    fn open(path: impl Into<std::path::PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let size = std::fs::metadata(&path)?.len();
        Ok(Self { path, size })
    }
}

impl MediaSource for NativeFileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        let path = self.path.clone();
        let size = self.size;
        Box::pin(async move {
            use std::io::{Read, Seek, SeekFrom};
            if offset > size {
                return Err(fpmedia_web::MediaError::Mp4(format!(
                    "read offset {offset} past EOF {size}"
                )));
            }
            let want = (len as u64).min(size.saturating_sub(offset)) as usize;
            let mut file = std::fs::File::open(&path).map_err(|e| {
                fpmedia_web::MediaError::Mp4(format!("open {}: {e}", path.display()))
            })?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| fpmedia_web::MediaError::Mp4(format!("seek: {e}")))?;
            let mut buf = vec![0u8; want];
            let mut filled = 0usize;
            while filled < want {
                let n = file
                    .read(&mut buf[filled..])
                    .map_err(|e| fpmedia_web::MediaError::Mp4(format!("read: {e}")))?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            buf.truncate(filled);
            Ok(bytes::Bytes::from(buf))
        })
    }
}

fn assert_pcm_track(audio: &fpmedia_web::Mp4AudioTrack, label: &str) {
    use fpmedia_web::AudioCodec;

    let meta = audio.metadata();
    assert_eq!(
        meta.codec.codec,
        AudioCodec::Pcm,
        "{label}: expected PCM, got {:?}",
        meta.codec
    );
    let codec = meta
        .codec
        .codec_string
        .as_deref()
        .expect("pcm codec_string");
    assert!(
        codec.starts_with("pcm-") || codec == "ulaw" || codec == "alaw",
        "{label}: codec_string={codec}"
    );
    assert!(
        meta.codec.pcm.is_some(),
        "{label}: missing pcm format on codec config"
    );
    assert!(
        meta.sample_rate >= 8000,
        "{label}: rate={}",
        meta.sample_rate
    );
    assert!(
        meta.number_of_channels >= 1,
        "{label}: ch={}",
        meta.number_of_channels
    );
    assert!(
        audio.sample_count() > 0 && audio.sample_count() < 100_000,
        "{label}: sample_count={} (expect chunk-coalesced, not per-PCM-frame)",
        audio.sample_count()
    );

    let sample = block_on(audio.encoded_sample(0)).expect("pcm sample 0");
    assert!(!sample.data.is_empty(), "{label}: empty sample 0");
    let format = meta.codec.pcm.expect("pcm format");
    let planar = format
        .decode_interleaved_f32(&sample.data, meta.number_of_channels)
        .expect("pcm decode");
    assert_eq!(planar.len(), meta.number_of_channels as usize);
    assert!(!planar[0].is_empty());
    let peak = planar
        .iter()
        .flat_map(|ch| ch.iter())
        .map(|s| s.abs())
        .fold(0.0f32, f32::max);
    eprintln!(
        "fpmedia-web pcm probe [{label}]: codec={codec} rate={} ch={} packets={} sample0_bytes={} peak={peak:.4}",
        meta.sample_rate,
        meta.number_of_channels,
        audio.sample_count(),
        sample.data.len(),
    );
    // Near-silent plates still have quantisation / dither; fail only if all-zero.
    assert!(
        peak > 0.0 || sample.data.iter().any(|&b| b != 0),
        "{label}: all-zero PCM payload"
    );
}

/// testvideo QT PCM plate — download to `public/` or set `WEBMEDIA_PCM_MOV`.
#[test]
fn demux_qt_pcm24_plate() {
    let path = std::env::var_os("WEBMEDIA_PCM_MOV")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../public/avc_1080p_25_mov_pcm24_gop12_bf0.mov")
        });
    if !path.exists() {
        eprintln!(
            "skip: missing {} (curl testvideo v0.3.0 avc_1080p_25_mov_pcm24_gop12_bf0.mov)",
            path.display()
        );
        return;
    }
    let t0 = Instant::now();
    let bytes = std::fs::read(&path).expect("read pcm plate");
    let opened = open_mp4_bytes(bytes).expect("open pcm plate");
    let audio = opened.audio.expect("pcm audio track");
    let _video = opened.video.expect("video");
    assert_pcm_track(&audio, "testvideo-pcm24");
    assert_eq!(
        audio.metadata().codec.codec_string.as_deref(),
        Some("pcm-s24"),
        "ffmpeg plate is pcm_s24le; enda lives under wave"
    );
    eprintln!("open+probe in {:?}", t0.elapsed());
}

/// Local QT PCM master (e.g. ChG) — progressive ranged open, not full slurp.
/// Set `WEBMEDIA_CHG_MOV` to the `.mov` path.
#[test]
fn demux_chg_pcm_master_progressive() {
    let Some(path) = std::env::var_os("WEBMEDIA_CHG_MOV").map(std::path::PathBuf::from) else {
        eprintln!("skip: set WEBMEDIA_CHG_MOV to a QT PCM .mov path");
        return;
    };
    if !path.exists() {
        eprintln!("skip: missing {}", path.display());
        return;
    }
    let t0 = Instant::now();
    let source: Arc<dyn MediaSource> =
        Arc::new(NativeFileSource::open(&path).expect("open ChG file"));
    let opened = block_on(open_mp4(
        source,
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: true,
        },
        |_| {},
    ))
    .expect("progressive ChG open");
    let audio = opened.audio.expect("ChG pcm audio");
    let video = opened.video.expect("ChG video");
    eprintln!(
        "ChG video {}x{} frames={} open {:?}",
        video.metadata().width,
        video.metadata().height,
        video.frame_count(),
        t0.elapsed()
    );
    assert_pcm_track(&audio, "ChG");
    assert_eq!(
        audio.metadata().codec.codec_string.as_deref(),
        Some("pcm-s24"),
        "ChG ffprobe is pcm_s24le"
    );
}

/// testvideo HEVC plate — bitstream IRAP verify. Override with `WEBMEDIA_HEVC_URL`.
/// Requires `--features hevc`.
#[cfg(feature = "hevc")]
const DEFAULT_HEVC_URL: &str = "https://github.com/alexantoshuk/testvideo/releases/download/v0.1.0/hevc_1080p_24_gop12_bf0.mp4";

#[cfg(feature = "hevc")]
#[test]
#[ignore = "network: downloads HEVC from testvideo Release (or WEBMEDIA_HEVC_URL)"]
fn hevc_bitstream_sync_verify_irap_keys() {
    let url = std::env::var("WEBMEDIA_HEVC_URL").unwrap_or_else(|_| DEFAULT_HEVC_URL.into());
    let bytes = fetch_url_bytes(&url).unwrap_or_else(|e| panic!("download {url}: {e}"));
    let track = open_mp4_bytes(bytes)
        .and_then(OpenedMp4::require_video)
        .expect("open hevc");
    let meta = track.metadata();
    assert_eq!(meta.codec.codec, VideoCodec::Hevc);
    let length_size =
        hevc_nal_length_size(&meta.codec.description).expect("hvcC lengthSizeMinusOne");
    let syncs = track.index().sync_frame_ids();
    assert!(
        !syncs.is_empty(),
        "expected stss/IRAP sync candidates after verify"
    );
    for &frame in &syncs {
        let sample = block_on(track.encoded_sample(frame)).expect("sample");
        assert_eq!(
            determine_hevc_packet_type(&sample.data, length_size),
            Some(PacketType::Key),
            "sync frame {frame} must be HEVC IRAP bitstream key"
        );
    }
    // GOP12 plate: keys roughly every 12 frames.
    if let (Some(&a), Some(&b)) = (syncs.first(), syncs.get(1)) {
        let gap = b.saturating_sub(a);
        assert!(
            (8..=16).contains(&gap),
            "unexpected key gap {gap} between {a} and {b}"
        );
    }
}
