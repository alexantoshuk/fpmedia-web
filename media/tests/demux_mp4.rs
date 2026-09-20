//! Integration tests: demux + frame index against short-GOP H.264 fixtures.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use webmedia::{
    GopProfile, MediaSource, MemorySource, OpenMp4Options, PacketType, Player, ReadFuture,
    VideoCodec, avc_nal_length_size, determine_avc_packet_type, determine_hevc_packet_type,
    hevc_nal_length_size, open_mp4_bytes, open_mp4_media_source_cached, open_mp4_path,
    open_mp4_source,
};

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

    let player = Player::from_track(track).expect("player");
    assert_eq!(player.state().frame, 0);
    let win = block_on(player.window_at(14)).expect("cascade");
    assert_eq!(win.target_frame, 14);
    assert!(win.samples.first().is_some_and(|s| s.is_keyframe));
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

    let player = Player::from_track(track).expect("player");
    let cascade = block_on(player.window_at(1)).expect("cascade");
    assert_eq!(cascade.target_frame, 1);
    assert_eq!(
        cascade.samples.iter().map(|s| s.frame).collect::<Vec<_>>(),
        vec![0, 3, 1]
    );
    assert!(cascade.samples[0].is_keyframe);
    assert!(!cascade.samples[1].is_keyframe);
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
    let source: Arc<dyn webmedia::MediaSource> = Arc::new(MemorySource::from_vec(bytes));
    let progressive = block_on(open_mp4_source(
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
    let source: Arc<dyn webmedia::MediaSource> = Arc::new(MemorySource::from_vec(bytes));
    let opened = block_on(open_mp4_media_source_cached(
        source,
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
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

/// testvideo fMP4 plate (AVC 1080p 24fps fragmented GOP12). Override with `WEBMEDIA_FMP4_URL`.
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
    let full = webmedia::open_mp4_bytes(bytes.clone()).expect("full open");
    let source: Arc<dyn webmedia::MediaSource> = Arc::new(MemorySource::from_vec(bytes));
    let progressive = block_on(open_mp4_source(
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
/// cargo test -p webmedia --test demux_mp4 progressive_fmp4_from_url -- --ignored --nocapture
/// ```
#[test]
#[ignore = "network: downloads fMP4 from testvideo Release (or WEBMEDIA_FMP4_URL)"]
fn progressive_fmp4_from_url() {
    let url = std::env::var("WEBMEDIA_FMP4_URL").unwrap_or_else(|_| DEFAULT_FMP4_URL.to_string());
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
    let t_open = Instant::now();
    let progressive = block_on(open_mp4_source(
        counted.clone() as Arc<dyn MediaSource>,
        OpenMp4Options {
            verify_avc_sync: false,
            defer_avc_sync: false,
        },
    ))
    .expect("progressive fMP4 open");
    let open_s = t_open.elapsed().as_secs_f64();
    let reads = counted.reads.load(Ordering::Relaxed);
    let max_len = counted.max_len.load(Ordering::Relaxed);
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
    let source: Arc<dyn webmedia::MediaSource> = Arc::new(MemorySource::from_vec(bytes));
    let track = block_on(open_mp4_source(
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
    use webmedia::{AudioCodec, open_mp4_media_bytes};

    let path = fixture("with_audio.mp4");
    if !path.exists() {
        eprintln!("skip: missing fixture {}", path.display());
        return;
    }
    let bytes = std::fs::read(&path).expect("read fixture");
    let opened = open_mp4_media_bytes(bytes).expect("open media");
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

/// testvideo HEVC plate — bitstream IRAP verify. Override with `WEBMEDIA_HEVC_URL`.
const DEFAULT_HEVC_URL: &str = "https://github.com/alexantoshuk/testvideo/releases/download/v0.1.0/hevc_1080p_24_gop12_bf0.mp4";

#[test]
#[ignore = "network: downloads HEVC from testvideo Release (or WEBMEDIA_HEVC_URL)"]
fn hevc_bitstream_sync_verify_irap_keys() {
    let url = std::env::var("WEBMEDIA_HEVC_URL").unwrap_or_else(|_| DEFAULT_HEVC_URL.into());
    let bytes = fetch_url_bytes(&url).unwrap_or_else(|e| panic!("download {url}: {e}"));
    let track = open_mp4_bytes(bytes).expect("open hevc");
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
