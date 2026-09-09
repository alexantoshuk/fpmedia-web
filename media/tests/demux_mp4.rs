//! Integration tests: demux + frame index against a short-GOP H.264 fixture.

use webmedia::{Player, VideoCodec, open_mp4_path};

fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/short_gop.mp4")
}

#[test]
fn demux_short_gop_metadata_and_frames() {
    let path = fixture_path();
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
    let win = player.window_at(14).expect("cascade");
    assert_eq!(win.target_frame, 14);
    assert!(win.samples.first().is_some_and(|s| s.is_keyframe));
}
