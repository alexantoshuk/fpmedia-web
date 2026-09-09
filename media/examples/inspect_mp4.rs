fn main() {
    let path = std::env::args().nth(1).expect("path");
    let track = webmedia::open_mp4_path(&path).expect("open");
    let meta = track.metadata();
    println!(
        "frames={} size={}x{} fps={:?} codec={:?} timescale={}",
        meta.frame_count,
        meta.width,
        meta.height,
        meta.fps,
        meta.codec.codec_string,
        meta.timescale
    );
    let idx = track.index();
    let syncs: Vec<_> = idx
        .frames()
        .iter()
        .filter(|e| e.is_sync)
        .map(|e| e.frame)
        .collect();
    println!(
        "sync_count={} first_syncs={:?}",
        syncs.len(),
        &syncs[..syncs.len().min(12)]
    );
    for f in [0u64, 1, 2, 3, 4, 15, 16, 23, 24] {
        if let Some(e) = idx.get(f) {
            println!(
                "frame {f}: sync={} dts={} cts={} decode_order={} window={:?}",
                e.is_sync,
                e.decode_timestamp_ticks,
                e.composition_timestamp_ticks,
                e.decode_order,
                idx.decode_window(f)
                    .map(|w| (w.len(), w.first().copied(), w.last().copied()))
            );
        } else {
            println!("frame {f}: MISSING");
        }
    }
    let mut none = 0usize;
    for f in 0..idx.len() as u64 {
        if idx.decode_window(f).is_none() {
            none += 1;
            if none <= 8 {
                println!("no window at {f}");
            }
        }
    }
    println!("decode_window none count={none}");
}
