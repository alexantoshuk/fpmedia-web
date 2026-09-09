//! Simulate warm-decoder feed decisions for the first N presentation frames.

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "media/demo/ААААА.mp4".into());
    let track = webmedia::open_mp4_path(&path).expect("open");
    let idx = track.index();
    let n = 40u64.min(idx.len() as u64);

    println!("frame\tdo\tcts\twlen");
    for f in 0..n {
        let e = idx.get(f).unwrap();
        let w = idx.decode_window(f).unwrap();
        println!(
            "{f}\t{}\t{}\t{}",
            e.decode_order,
            e.composition_timestamp_ticks,
            w.len()
        );
    }

    println!("\n--- warm sim ---");
    let mut fed: Option<usize> = None;
    let mut key: Option<usize> = None;
    let mut buf: Vec<u64> = Vec::new();

    for present in 0..n {
        let e = idx.get(present).unwrap();
        let w = idx.decode_window(present).unwrap();
        let key_do = idx.get(w[0]).unwrap().decode_order;
        let same = key == Some(key_do);

        if same && buf.contains(&present) {
            buf.retain(|x| *x >= present);
            println!("p{present}: BUF        buf={buf:?}");
            continue;
        }

        let already = same && fed.is_some_and(|f| e.decode_order <= f);
        let need_restart = !same || (already && !buf.contains(&present));

        let mut to_feed: Vec<u64> = if need_restart {
            if already {
                println!("p{present}: RESTART do={} fed={fed:?}", e.decode_order);
            }
            key = Some(key_do);
            fed = None;
            buf.clear();
            w.clone()
        } else {
            let f0 = fed.unwrap_or(0);
            w.iter()
                .copied()
                .filter(|pf| idx.get(*pf).unwrap().decode_order > f0)
                .collect()
        };

        to_feed.sort_by_key(|pf| idx.get(*pf).unwrap().decode_order);
        for pf in &to_feed {
            let d = idx.get(*pf).unwrap().decode_order;
            fed = Some(fed.map(|x| x.max(d)).unwrap_or(d));
            if !buf.contains(pf) {
                buf.push(*pf);
            }
        }
        // present `present` now
        buf.retain(|x| *x != present && *x >= present);
        // actually after present we remove present from buf
        buf.retain(|x| *x > present);
        println!("p{present}: feed={to_feed:?} fed={fed:?} buf={buf:?}");
    }
}
