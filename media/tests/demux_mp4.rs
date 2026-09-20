//! Integration tests: demux + frame index against short-GOP H.264 fixtures.

use std::future::Future;
use std::sync::Arc;

use webmedia::{
    GopProfile, MemorySource, OpenMp4Options, PacketType, Player, VideoCodec, avc_nal_length_size,
    determine_avc_packet_type, open_mp4_path, open_mp4_source,
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

    assert_eq!(index.nearest_keyframe(0), Some(0));
    assert_eq!(index.nearest_keyframe(11), Some(0));
    assert_eq!(index.nearest_keyframe(12), Some(12));
    assert_eq!(index.nearest_keyframe(25), Some(24));

    let window = index.decode_window(14).expect("window");
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

fn assert_progressive_fmp4_matches_full(bytes: Vec<u8>) {
    let full = webmedia::open_mp4_bytes(bytes.clone()).expect("full open");
    let source: Arc<dyn webmedia::MediaSource> = Arc::new(MemorySource::from_vec(bytes));
    let progressive = block_on(open_mp4_source(
        source,
        OpenMp4Options {
            verify_avc_sync: true,
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
    let bytes = fetch_url_bytes(&url).unwrap_or_else(|e| panic!("fetch {url}: {e}"));
    assert_progressive_fmp4_matches_full(bytes);
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
