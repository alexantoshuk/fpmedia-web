//! Top-level ISOBMFF / QuickTime box scan for progressive open.
//!
//! `re_mp4` reads every top-level **header** before `skip_box` seeks past `mdat`.
//! Gaps filled with zeros look like `size=0` and abort the parse — so we materialize
//! headers for skipped boxes and full `ftyp`+`moov` (+ `moof` for fMP4) at
//! **file-absolute** offsets.
//!
//! mediabunny: `src/isobmff/isobmff-demuxer.ts` `readFragment` loads the entire
//! `moof` via `requestSlice(startPos, moofBoxInfo.totalSize)`.
//! For open, mediabunny `readMetadata` stops after `moov` then probes EOF `mfra`
//! (`lookForMfraBox`) — we still need every `moof` for a full frame index, but
//! use `mfra`/`tfra` to skip the per-fragment Range walk when present.

use std::io::{self, Read, Seek, SeekFrom};

use crate::error::MediaError;
use crate::source::MediaSource;

const HEADER_SIZE: usize = 8;
const EXTENDED_HEADER_SIZE: usize = 16;
/// Reject absurdly large `moov` (corruption / wrong container).
const MAX_MOOV_BYTES: u64 = 64 * 1024 * 1024;
/// Reject absurdly large `moof` (corruption). Bodies are small metadata tables.
const MAX_MOOF_BYTES: u64 = 16 * 1024 * 1024;
/// Cap `mfra` load (index only; corruption guard).
const MAX_MFRA_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopLevelBox {
    /// File-absolute start of the box (including header).
    pub offset: u64,
    /// Total box size including header.
    pub size: u64,
    /// 8 or 16 (largesize).
    pub header_len: u8,
    pub typ: [u8; 4],
}

impl TopLevelBox {
    pub fn type_str(&self) -> &str {
        std::str::from_utf8(&self.typ).unwrap_or("????")
    }

    pub fn is_ftyp(&self) -> bool {
        &self.typ == b"ftyp"
    }

    pub fn is_moov(&self) -> bool {
        &self.typ == b"moov"
    }

    pub fn is_moof(&self) -> bool {
        &self.typ == b"moof"
    }

    pub fn is_mdat(&self) -> bool {
        &self.typ == b"mdat"
    }

    pub fn is_mfra(&self) -> bool {
        &self.typ == b"mfra"
    }

    /// Full body required for progressive `re_mp4` parse (not header-only skip).
    pub fn needs_full_body(&self) -> bool {
        self.is_ftyp() || self.is_moov() || self.is_moof()
    }
}

/// Loaded bytes at a file-absolute offset (full box or header-only).
#[derive(Clone, Debug)]
pub struct LoadedBox {
    pub info: TopLevelBox,
    pub bytes: Vec<u8>,
}

/// Prefetch this many leading bytes so top-level headers (ftyp/free/mdat) need
/// one Blob round-trip instead of many tiny `array_buffer`s on large files.
const HEAD_PREFETCH: usize = 64 * 1024;
/// When scanning past the initial head, refill in this chunk size (network RTT).
/// 1 MiB covers ~2–3 typical 1080p GOP fragments so walk without `mfra` is fewer
/// Ranges than a 64 KiB window (GitHub Releases).
const SCAN_WINDOW: usize = 1024 * 1024;
/// Merge adjacent load ranges if the gap is at most this many bytes (fMP4 moof+mdat hdr).
const COALESCE_GAP: u64 = 256;
/// Cap a coalesced Range GET when packing across `mdat` gaps (full-file walk path).
const MAX_COALESCE_RUN: usize = 1024 * 1024;
/// Concurrent Range GETs. Demo `/proxy`→GitHub hangs under parallel waves —
/// keep at 1 (sequential) for reliable open; progress updates after each GET.
const COALESCE_FETCH_PARALLEL: usize = 1;

/// Walk all top-level boxes (`moov` required).
///
/// Loads **headers** for every top-level atom (`free` / `wide` / `mdat` / …).
/// Stopping at `moov` leaves later headers as sparse zeros; `re_mp4` then hits a
/// size-0 box and `skip_box(0)` infinite-loops (browser freeze on large files).
///
/// Prefer [`load_ftyp_moov`] which probes EOF `mfra` when present.
pub async fn scan_top_level(source: &dyn MediaSource) -> Result<Vec<TopLevelBox>, MediaError> {
    scan_top_level_from(source, 0, None).await
}

/// Cap provisional moof Range before the real size is known (bodies are small tables).
const MOOF_FETCH_CAP: usize = 64 * 1024;

#[derive(Clone, Copy)]
enum MfraNeedKind {
    Prefix(usize),
    Moof(usize),
}

struct MfraIndex {
    mfra_off: u64,
    mfra_size: u64,
    mfra_bytes: Vec<u8>,
    moof_offsets: Vec<u64>,
}

/// EOF `mfra` / `tfra` probe only (mediabunny: `lookForMfraBox`) — no fragment walk.
async fn probe_mfra(source: &dyn MediaSource) -> Result<Option<MfraIndex>, MediaError> {
    let file_size = source.size();
    if file_size < 16 {
        return Ok(None);
    }
    let tail = source.read(file_size - 4, 4).await?;
    if tail.len() < 4 {
        return Ok(None);
    }
    let mfra_size = u32::from_be_bytes(tail[0..4].try_into().unwrap()) as u64;
    if mfra_size < HEADER_SIZE as u64 || mfra_size > MAX_MFRA_BYTES || mfra_size > file_size {
        return Ok(None);
    }
    let mfra_off = file_size - mfra_size;
    let mfra_bytes = source.read(mfra_off, mfra_size as usize).await?;
    if mfra_bytes.len() < HEADER_SIZE || &mfra_bytes[4..8] != b"mfra" {
        return Ok(None);
    }
    let declared = u32::from_be_bytes(mfra_bytes[0..4].try_into().unwrap()) as u64;
    if declared != mfra_size && declared != 1 {
        return Ok(None);
    }
    let moof_offsets = parse_mfra_moof_offsets(&mfra_bytes)?;
    if moof_offsets.is_empty() || moof_offsets[0] >= mfra_off {
        return Ok(None);
    }
    Ok(Some(MfraIndex {
        mfra_off,
        mfra_size,
        mfra_bytes,
        moof_offsets,
    }))
}

/// Concurrent Range GETs for mfra-listed moofs (mediabunny UrlSource
/// `DEFAULT_PARALLELISM = 2`; we use 4 — each payload is ≤64 KiB, not multi‑MiB).
const MFRA_MOOF_FETCH_PARALLEL: usize = 4;

/// Single-file fMP4 open via `mfra` / `tfra` moof offsets.
///
/// Loads **moof tables only** (+ `ftyp`/`moov`); synthesizes `mdat` headers.
/// Do **not** bulk-`read(0, mfra)`: a 41 MB plate freezes the WASM main thread on
/// `arrayBuffer`→`Vec` even after the network body finished (~4 s) — e2e could
/// not read `#status` for 30 s+. mediabunny: `UrlSource` streams bounded worker
/// windows; full pre-mfra dump is a local anti-pattern for our frame-index open.
async fn load_via_mfra(
    source: &dyn MediaSource,
    idx: MfraIndex,
    progress: &mut impl FnMut(&str),
) -> Result<Vec<LoadedBox>, MediaError> {
    let n = idx.moof_offsets.len();
    progress(&format!("mfra index: {n} fragments…"));
    load_via_mfra_per_moof(source, idx, progress).await
}

async fn load_via_mfra_per_moof(
    source: &dyn MediaSource,
    idx: MfraIndex,
    progress: &mut impl FnMut(&str),
) -> Result<Vec<LoadedBox>, MediaError> {
    let file_size = source.size();
    let n = idx.moof_offsets.len();

    let first_moof = idx.moof_offsets[0];
    let prefix = scan_top_level_from(source, 0, Some(first_moof)).await?;
    if !prefix.iter().any(|b| b.is_moov()) {
        return Err(MediaError::Mp4(
            "mfra path: no moov before first moof".into(),
        ));
    }

    // Provisional moof reads: up to MOOF_FETCH_CAP or the gap to the next moof.
    let mut work: Vec<(MfraNeedKind, u64, usize)> = Vec::new();
    for (i, info) in prefix.iter().enumerate() {
        if info.needs_full_body() {
            work.push((MfraNeedKind::Prefix(i), info.offset, info.size as usize));
        }
    }
    for (i, &moof_off) in idx.moof_offsets.iter().enumerate() {
        let next = idx.moof_offsets.get(i + 1).copied().unwrap_or(idx.mfra_off);
        let gap = next.saturating_sub(moof_off) as usize;
        if gap < HEADER_SIZE {
            return Err(MediaError::Mp4(format!(
                "mfra moof gap too small at {moof_off}"
            )));
        }
        let fetch = gap.clamp(HEADER_SIZE, MOOF_FETCH_CAP);
        work.push((MfraNeedKind::Moof(i), moof_off, fetch));
    }

    let needs: Vec<(usize, u64, usize)> = work
        .iter()
        .enumerate()
        .map(|(i, (_, off, len))| (i, *off, *len))
        .collect();

    let runs = coalesce_byte_runs_adjacent(&needs);
    let total = runs.len();
    progress(&format!("Fetching moof tables (0/{total})…"));

    let mut fetched: Vec<Option<Vec<u8>>> = vec![None; work.len()];
    fill_from_coalesce_runs(
        source,
        &runs,
        &mut fetched,
        Some(progress),
        MFRA_MOOF_FETCH_PARALLEL,
    )
    .await?;

    let mut prefix_bytes: Vec<Option<Vec<u8>>> = vec![None; prefix.len()];
    let mut moof_chunks: Vec<Option<Vec<u8>>> = vec![None; n];
    for (wi, (kind, _, _)) in work.iter().enumerate() {
        let bytes = fetched[wi]
            .take()
            .ok_or_else(|| MediaError::Mp4("mfra load missing chunk".into()))?;
        match *kind {
            MfraNeedKind::Prefix(i) => prefix_bytes[i] = Some(bytes),
            MfraNeedKind::Moof(i) => moof_chunks[i] = Some(bytes),
        }
    }

    let mut loaded = Vec::new();
    for (i, mut info) in prefix.into_iter().enumerate() {
        let mut bytes = if let Some(b) = prefix_bytes[i].take() {
            b
        } else {
            synthesize_box_header(&info)
        };
        if info.is_moov() {
            sanitize_moov_padding(&mut bytes)?;
            info.size = bytes.len() as u64;
            info.header_len = HEADER_SIZE as u8;
        }
        loaded.push(LoadedBox { info, bytes });
    }

    for (i, &moof_off) in idx.moof_offsets.iter().enumerate() {
        let chunk = moof_chunks[i]
            .take()
            .ok_or_else(|| MediaError::Mp4(format!("mfra moof missing at {moof_off}")))?;
        let (header_len, moof_size, typ) = parse_box_header_bytes(&chunk, moof_off, file_size)?;
        if &typ != b"moof" {
            return Err(MediaError::Mp4(format!(
                "mfra expected moof at {moof_off}, got {}",
                std::str::from_utf8(&typ).unwrap_or("????")
            )));
        }
        if moof_size > MAX_MOOF_BYTES {
            return Err(MediaError::Mp4(format!(
                "moof too large ({moof_size}) at {moof_off}"
            )));
        }
        if (moof_size as usize) > chunk.len() {
            let exact = source.read(moof_off, moof_size as usize).await?;
            if exact.len() != moof_size as usize {
                return Err(MediaError::Mp4(format!("short moof read at {moof_off}")));
            }
            loaded.push(LoadedBox {
                info: TopLevelBox {
                    offset: moof_off,
                    size: moof_size,
                    header_len: header_len as u8,
                    typ,
                },
                bytes: exact,
            });
        } else {
            loaded.push(LoadedBox {
                info: TopLevelBox {
                    offset: moof_off,
                    size: moof_size,
                    header_len: header_len as u8,
                    typ,
                },
                bytes: chunk[..moof_size as usize].to_vec(),
            });
        }

        let next_boundary = idx.moof_offsets.get(i + 1).copied().unwrap_or(idx.mfra_off);
        let mdat_off = moof_off.saturating_add(moof_size);
        if mdat_off > next_boundary {
            return Err(MediaError::Mp4(format!(
                "moof size overruns next fragment at {moof_off}"
            )));
        }
        if mdat_off < next_boundary {
            let mdat_size = next_boundary - mdat_off;
            let mdat = TopLevelBox {
                offset: mdat_off,
                size: mdat_size,
                header_len: if mdat_size > u32::MAX as u64 {
                    EXTENDED_HEADER_SIZE as u8
                } else {
                    HEADER_SIZE as u8
                },
                typ: *b"mdat",
            };
            let bytes = synthesize_box_header(&mdat);
            loaded.push(LoadedBox { info: mdat, bytes });
        }
    }

    loaded.push(LoadedBox {
        info: TopLevelBox {
            offset: idx.mfra_off,
            size: idx.mfra_size,
            header_len: HEADER_SIZE as u8,
            typ: *b"mfra",
        },
        bytes: idx.mfra_bytes,
    });

    Ok(loaded)
}

async fn scan_top_level_from(
    source: &dyn MediaSource,
    start: u64,
    stop_before: Option<u64>,
) -> Result<Vec<TopLevelBox>, MediaError> {
    let file_size = source.size();
    if file_size == 0 {
        return Err(MediaError::Mp4("empty media source".into()));
    }
    let end = stop_before.unwrap_or(file_size).min(file_size);

    let mut win_off = start;
    let first_len = (end.saturating_sub(start) as usize).min(if start == 0 {
        HEAD_PREFETCH
    } else {
        SCAN_WINDOW
    });
    let mut win = if first_len == 0 {
        Vec::new()
    } else {
        source.read(start, first_len).await?
    };
    if start < end && win.len() < HEADER_SIZE {
        return Err(MediaError::Mp4(format!(
            "truncated file head at {start} (got {} bytes)",
            win.len()
        )));
    }

    let mut boxes = Vec::new();
    let mut offset = start;
    let mut saw_moov = start > 0; // prefix-only callers already have moov

    while offset < end {
        ensure_scan_window(
            source,
            &mut win_off,
            &mut win,
            offset,
            HEADER_SIZE,
            file_size,
        )
        .await?;
        let (header_len, box_size, typ) =
            read_box_header_at_window(&win, win_off, offset, file_size, source).await?;

        let entry = TopLevelBox {
            offset,
            size: box_size,
            header_len: header_len as u8,
            typ,
        };
        if entry.is_moov() {
            saw_moov = true;
            if box_size > MAX_MOOV_BYTES {
                return Err(MediaError::Mp4(format!(
                    "moov too large ({box_size} bytes)"
                )));
            }
        }
        if entry.is_moof() && box_size > MAX_MOOF_BYTES {
            return Err(MediaError::Mp4(format!(
                "moof too large ({box_size} bytes)"
            )));
        }
        let next = offset.saturating_add(box_size);
        if next > end {
            // Would cross stop_before — leave remainder to the caller.
            break;
        }
        boxes.push(entry);
        offset = next;
    }

    if stop_before.is_none() && !saw_moov {
        return Err(MediaError::Mp4(
            "no moov box found (fragmented / unsupported)".into(),
        ));
    }

    Ok(boxes)
}

fn parse_box_header_bytes(
    bytes: &[u8],
    offset: u64,
    file_size: u64,
) -> Result<(u64, u64, [u8; 4]), MediaError> {
    if bytes.len() < HEADER_SIZE {
        return Err(MediaError::Mp4(format!("truncated box header at {offset}")));
    }
    let size32 = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
    let typ: [u8; 4] = bytes[4..8].try_into().unwrap();
    let (header_len, box_size) = if size32 == 1 {
        if bytes.len() < EXTENDED_HEADER_SIZE {
            return Err(MediaError::Mp4(format!(
                "truncated largesize at offset {offset}"
            )));
        }
        let size64 = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
        if size64 < EXTENDED_HEADER_SIZE as u64 {
            return Err(MediaError::Mp4(format!(
                "invalid largesize {size64} at offset {offset}"
            )));
        }
        (EXTENDED_HEADER_SIZE as u64, size64)
    } else if size32 == 0 {
        (HEADER_SIZE as u64, file_size - offset)
    } else {
        if (size32 as u64) < HEADER_SIZE as u64 {
            return Err(MediaError::Mp4(format!(
                "invalid box size {size32} at offset {offset}"
            )));
        }
        (HEADER_SIZE as u64, u64::from(size32))
    };
    if offset.saturating_add(box_size) > file_size {
        return Err(MediaError::Mp4(format!(
            "box {} at {offset} size {box_size} exceeds file size {file_size}",
            std::str::from_utf8(&typ).unwrap_or("????")
        )));
    }
    Ok((header_len, box_size, typ))
}

/// Collect unique sorted `moof` offsets from all `tfra` children of `mfra`.
fn parse_mfra_moof_offsets(mfra: &[u8]) -> Result<Vec<u64>, MediaError> {
    if mfra.len() < HEADER_SIZE || &mfra[4..8] != b"mfra" {
        return Ok(Vec::new());
    }
    let mut offsets = Vec::new();
    let mut pos = HEADER_SIZE;
    while pos + HEADER_SIZE <= mfra.len() {
        let size32 = u32::from_be_bytes(mfra[pos..pos + 4].try_into().unwrap()) as usize;
        let typ = &mfra[pos + 4..pos + 8];
        if size32 < HEADER_SIZE || pos + size32 > mfra.len() {
            break;
        }
        if typ == b"tfra" {
            parse_tfra_moof_offsets(&mfra[pos..pos + size32], &mut offsets)?;
        }
        pos += size32;
    }
    offsets.sort_unstable();
    offsets.dedup();
    Ok(offsets)
}

fn parse_tfra_moof_offsets(tfra: &[u8], out: &mut Vec<u64>) -> Result<(), MediaError> {
    // FullBox: size(4) + 'tfra'(4) + version(1) + flags(3) + …
    if tfra.len() < 16 || &tfra[4..8] != b"tfra" {
        return Ok(());
    }
    let version = tfra[8];
    let body = &tfra[12..]; // after version/flags
    if body.len() < 16 {
        return Ok(());
    }
    let _track_id = u32::from_be_bytes(body[0..4].try_into().unwrap());
    let length = u32::from_be_bytes(body[4..8].try_into().unwrap());
    let traf_len = ((length >> 4) & 3) as usize + 1;
    let trun_len = ((length >> 2) & 3) as usize + 1;
    let sample_len = (length & 3) as usize + 1;
    let entry_count = u32::from_be_bytes(body[8..12].try_into().unwrap()) as usize;
    let mut p = 12;
    for _ in 0..entry_count {
        let moof_off = if version == 1 {
            if p + 16 > body.len() {
                return Err(MediaError::Mp4("truncated tfra entry (v1)".into()));
            }
            p += 8; // time
            let off = u64::from_be_bytes(body[p..p + 8].try_into().unwrap());
            p += 8;
            off
        } else {
            if p + 8 > body.len() {
                return Err(MediaError::Mp4("truncated tfra entry (v0)".into()));
            }
            p += 4; // time
            let off = u32::from_be_bytes(body[p..p + 4].try_into().unwrap()) as u64;
            p += 4;
            off
        };
        let skip = traf_len + trun_len + sample_len;
        if p + skip > body.len() {
            return Err(MediaError::Mp4("truncated tfra traf/trun/sample".into()));
        }
        p += skip;
        out.push(moof_off);
    }
    Ok(())
}

async fn ensure_scan_window(
    source: &dyn MediaSource,
    win_off: &mut u64,
    win: &mut Vec<u8>,
    pos: u64,
    need: usize,
    file_size: u64,
) -> Result<(), MediaError> {
    let need_u = need as u64;
    let win_end = win_off.saturating_add(win.len() as u64);
    if pos >= *win_off && pos.saturating_add(need_u) <= win_end {
        return Ok(());
    }
    let available = file_size.saturating_sub(pos) as usize;
    if available < need {
        return Err(MediaError::Mp4(format!(
            "truncated box header at offset {pos} (need {need}, have {available})"
        )));
    }
    let chunk = SCAN_WINDOW.min(available).max(need);
    *win = source.read(pos, chunk).await?;
    *win_off = pos;
    if win.len() < need {
        return Err(MediaError::Mp4(format!(
            "truncated scan window at {pos} (got {}, want {need})",
            win.len()
        )));
    }
    Ok(())
}

/// Parse a box header using the sliding window, fetching the 8-byte largesize
/// extension into the window when needed.
async fn read_box_header_at_window(
    win: &[u8],
    win_off: u64,
    offset: u64,
    file_size: u64,
    source: &dyn MediaSource,
) -> Result<(u64, u64, [u8; 4]), MediaError> {
    let rel = (offset - win_off) as usize;
    if rel + HEADER_SIZE > win.len() {
        return Err(MediaError::Mp4(format!(
            "scan window miss at {offset} (win_off={win_off}, len={})",
            win.len()
        )));
    }
    let header = &win[rel..rel + HEADER_SIZE];
    let size32 = u32::from_be_bytes(header[0..4].try_into().unwrap());
    let typ: [u8; 4] = header[4..8].try_into().unwrap();

    let (header_len, box_size) = if size32 == 1 {
        let ext = if rel + EXTENDED_HEADER_SIZE <= win.len() {
            win[rel + 8..rel + 16].to_vec()
        } else {
            source.read(offset + 8, 8).await?
        };
        if ext.len() < 8 {
            return Err(MediaError::Mp4(format!(
                "truncated largesize at offset {offset}"
            )));
        }
        let size64 = u64::from_be_bytes(ext[0..8].try_into().unwrap());
        if size64 < EXTENDED_HEADER_SIZE as u64 {
            return Err(MediaError::Mp4(format!(
                "invalid largesize {size64} at offset {offset}"
            )));
        }
        (EXTENDED_HEADER_SIZE as u64, size64)
    } else if size32 == 0 {
        (HEADER_SIZE as u64, file_size - offset)
    } else {
        if (size32 as u64) < HEADER_SIZE as u64 {
            return Err(MediaError::Mp4(format!(
                "invalid box size {size32} at offset {offset}"
            )));
        }
        (HEADER_SIZE as u64, u64::from(size32))
    };

    if box_size < header_len {
        return Err(MediaError::Mp4(format!(
            "box size {box_size} < header at offset {offset}"
        )));
    }
    if offset.saturating_add(box_size) > file_size {
        return Err(MediaError::Mp4(format!(
            "box {} at {offset} size {box_size} exceeds file size {file_size}",
            std::str::from_utf8(&typ).unwrap_or("????")
        )));
    }

    Ok((header_len, box_size, typ))
}

/// Truncate `moov` before trailing size-0 / short padding and patch the size field.
///
/// QuickTime often pads `moov` with zeros. After real children end, `re_mp4`
/// parses those as a size-0 box and `skip_box(0)` seeks back forever
/// (ISOBMFF: size 0 ⇒ extends to end of parent — stop, don't skip 0).
fn sanitize_moov_padding(bytes: &mut Vec<u8>) -> Result<(), MediaError> {
    if bytes.len() < HEADER_SIZE || &bytes[4..8] != b"moov" {
        return Ok(());
    }
    // 64-bit largesize moov: leave alone (rare; size field is not a u32 length).
    if bytes[0..4] == [0, 0, 0, 1] {
        return Ok(());
    }

    // Walk top-level children inside moov; stop before size-0 or truncated junk.
    let mut pos = HEADER_SIZE;
    let end = bytes.len();
    while pos < end {
        if end - pos < HEADER_SIZE {
            // Fewer than 8 bytes of padding — drop them.
            break;
        }
        let size32 = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap());
        if size32 == 0 {
            // size-0 "box" (padding) — truncate here.
            break;
        }
        let box_size = if size32 == 1 {
            if end - pos < EXTENDED_HEADER_SIZE {
                break;
            }
            let size64 = u64::from_be_bytes(bytes[pos + 8..pos + 16].try_into().unwrap());
            size64 as usize
        } else {
            size32 as usize
        };
        if box_size < HEADER_SIZE || pos.saturating_add(box_size) > end {
            // Declared size runs past moov — treat as corruption/padding.
            break;
        }
        pos += box_size;
    }
    bytes.truncate(pos);

    let new_size = u32::try_from(bytes.len())
        .map_err(|_| MediaError::Mp4("moov too large after padding trim".into()))?;
    bytes[0..4].copy_from_slice(&new_size.to_be_bytes());
    Ok(())
}

/// Load bytes needed for progressive `re_mp4` parse:
/// - full `ftyp` + `moov` (+ every `moof` for fragmented files)
/// - headers only for everything else (`mdat`/`free`/`mfra`/…) so `BoxHeader::read` works
pub async fn load_ftyp_moov(source: &dyn MediaSource) -> Result<Vec<LoadedBox>, MediaError> {
    load_ftyp_moov_with_progress(source, &mut |_| {}).await
}

async fn load_ftyp_moov_with_progress(
    source: &dyn MediaSource,
    progress: &mut impl FnMut(&str),
) -> Result<Vec<LoadedBox>, MediaError> {
    progress("Probing mfra index…");
    if let Some(idx) = probe_mfra(source).await? {
        match load_via_mfra(source, idx, progress).await {
            Ok(loaded) => return Ok(loaded),
            Err(e) => {
                progress(&format!("mfra open failed ({e}); scanning file…"));
            }
        }
    }

    progress("Scanning top-level boxes…");
    let boxes = scan_top_level(source).await?;
    let moof_count = boxes.iter().filter(|b| b.is_moof()).count();
    if moof_count > 0 {
        progress(&format!(
            "Fragmented MP4: loading {moof_count} moof box(es)…"
        ));
    }

    // Only Range-fetch boxes that need full bodies. Header-only atoms are
    // synthesized from the scan map (avoids one GET per mdat).
    let needs: Vec<(usize, u64, usize)> = boxes
        .iter()
        .enumerate()
        .filter_map(|(i, info)| {
            if info.needs_full_body() {
                Some((i, info.offset, info.size as usize))
            } else {
                None
            }
        })
        .collect();

    if let Some((_, off, need)) = needs.iter().find(|(i, _, _)| boxes[*i].is_moov()) {
        progress(&format!(
            "Reading moov @ {off} ({:.1} KB)…",
            *need as f64 / 1024.0
        ));
    }

    let runs = coalesce_byte_runs(&needs);
    if !needs.is_empty() {
        progress(&format!(
            "Fetching {} range(s) for {} box body(ies)…",
            runs.len(),
            needs.len()
        ));
    }

    let mut loaded_bytes: Vec<Option<Vec<u8>>> = vec![None; boxes.len()];
    fill_from_coalesce_runs(
        source,
        &runs,
        &mut loaded_bytes,
        Some(progress),
        COALESCE_FETCH_PARALLEL,
    )
    .await?;

    let mut loaded = Vec::with_capacity(boxes.len());
    for (i, mut info) in boxes.into_iter().enumerate() {
        let mut bytes = if let Some(b) = loaded_bytes[i].take() {
            b
        } else {
            synthesize_box_header(&info)
        };
        if bytes.len() >= 8 {
            let typ = &bytes[4..8];
            if typ != info.typ.as_slice() {
                return Err(MediaError::Mp4(format!(
                    "box type mismatch at {}: header said {:?}, payload {:?}",
                    info.offset,
                    info.type_str(),
                    std::str::from_utf8(typ).unwrap_or("????")
                )));
            }
        }
        if info.is_moov() {
            sanitize_moov_padding(&mut bytes)?;
            info.size = bytes.len() as u64;
            info.header_len = HEADER_SIZE as u8;
        }
        loaded.push(LoadedBox { info, bytes });
    }

    if !loaded.iter().any(|b| b.info.is_moov()) {
        return Err(MediaError::Mp4("moov missing after load".into()));
    }

    Ok(loaded)
}

/// Build an ISOBMFF box header matching [`TopLevelBox`] (body not included).
fn synthesize_box_header(info: &TopLevelBox) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(EXTENDED_HEADER_SIZE);
    if info.header_len as usize >= EXTENDED_HEADER_SIZE || info.size > u32::MAX as u64 {
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&info.typ);
        bytes.extend_from_slice(&info.size.to_be_bytes());
    } else {
        bytes.extend_from_slice(&(info.size as u32).to_be_bytes());
        bytes.extend_from_slice(&info.typ);
    }
    bytes
}

/// One box body to load: `(box index in scan, file offset, byte length)`.
type BoxLoadNeed = (usize, u64, usize);
/// One Range GET: `(start, length, members of this run)`.
type CoalesceRun = (u64, usize, Vec<BoxLoadNeed>);

/// Fetch coalesced runs in waves of `parallel` concurrent Range GETs
/// (`mediabunny: UrlSource.parallelism`).
///
/// `mfra` path: adjacent-only ≤64 KiB/moof + [`MFRA_MOOF_FETCH_PARALLEL`].
/// Full-file walk: may pack across `mdat` — keep [`COALESCE_FETCH_PARALLEL`] = 1
/// (multi‑MiB × parallel hung the demo proxy).
async fn fill_from_coalesce_runs(
    source: &dyn MediaSource,
    runs: &[CoalesceRun],
    out: &mut [Option<Vec<u8>>],
    mut progress: Option<&mut dyn FnMut(&str)>,
    parallel: usize,
) -> Result<(), MediaError> {
    use futures::future::join_all;

    let parallel = parallel.max(1);
    let total = runs.len();
    let mut i = 0;
    while i < runs.len() {
        let end = (i + parallel).min(runs.len());
        crate::util::yield_now().await;
        let futs: Vec<_> = runs[i..end]
            .iter()
            .map(|(run_off, run_len, _)| source.read(*run_off, *run_len))
            .collect();
        let results = join_all(futs).await;
        for (run, chunk) in runs[i..end].iter().zip(results) {
            let (run_off, run_len, members) = run;
            let chunk = chunk?;
            if chunk.len() < *run_len {
                return Err(MediaError::Mp4(format!(
                    "short coalesced read at {run_off}: got {} want {run_len}",
                    chunk.len()
                )));
            }
            for &(box_idx, box_off, box_len) in members {
                let start = (box_off - run_off) as usize;
                let slice_end = start + box_len;
                if slice_end > chunk.len() {
                    return Err(MediaError::Mp4(format!(
                        "coalesced slice OOB for box {box_idx} at {box_off}"
                    )));
                }
                if box_idx >= out.len() {
                    return Err(MediaError::Mp4(format!(
                        "coalesced box index {box_idx} OOB"
                    )));
                }
                out[box_idx] = Some(chunk[start..slice_end].to_vec());
            }
        }
        // Progress *after* the wave completes — "120/120" while still awaiting
        // the last GETs looked like a hang.
        if let Some(p) = progress.as_mut() {
            p(&format!("Fetching moof tables ({end}/{total})…"));
        }
        i = end;
    }
    Ok(())
}

/// Merge only near-adjacent needs ([`COALESCE_GAP`]) — no packing across large
/// `mdat` (used by `mfra` moof body load).
fn coalesce_byte_runs_adjacent(needs: &[BoxLoadNeed]) -> Vec<CoalesceRun> {
    let mut sorted = needs.to_vec();
    sorted.sort_by_key(|(_, off, _)| *off);
    let mut runs: Vec<CoalesceRun> = Vec::new();
    for &(idx, off, len) in &sorted {
        if len == 0 {
            continue;
        }
        let need_end = off.saturating_add(len as u64);
        if let Some((run_off, run_len, members)) = runs.last_mut() {
            let run_end = *run_off + *run_len as u64;
            if off <= run_end.saturating_add(COALESCE_GAP) {
                *run_len = (need_end.max(run_end) - *run_off) as usize;
                members.push((idx, off, len));
                continue;
            }
        }
        runs.push((off, len, vec![(idx, off, len)]));
    }
    runs
}

/// Merge sorted `(box_idx, offset, len)` into fewer Range GETs.
///
/// Adjacent / near needs merge via [`COALESCE_GAP`]. On fMP4 full-file walk,
/// also pack into ≤[`MAX_COALESCE_RUN`] windows so one RTT covers several
/// fragments (extra media bytes are read then discarded when slicing).
fn coalesce_byte_runs(needs: &[BoxLoadNeed]) -> Vec<CoalesceRun> {
    let mut sorted = needs.to_vec();
    sorted.sort_by_key(|(_, off, _)| *off);
    let mut runs: Vec<CoalesceRun> = Vec::new();
    for &(idx, off, len) in &sorted {
        if len == 0 {
            continue;
        }
        let need_end = off.saturating_add(len as u64);
        if let Some((run_off, run_len, members)) = runs.last_mut() {
            let run_end = *run_off + *run_len as u64;
            let gap_ok = off <= run_end.saturating_add(COALESCE_GAP);
            let packed_end = need_end.max(run_end);
            let pack_ok = packed_end.saturating_sub(*run_off) as usize <= MAX_COALESCE_RUN
                && off < run_off.saturating_add(MAX_COALESCE_RUN as u64);
            if gap_ok || pack_ok {
                *run_len = (packed_end - *run_off) as usize;
                members.push((idx, off, len));
                continue;
            }
        }
        runs.push((off, len, vec![(idx, off, len)]));
    }
    runs
}

/// Sparse reader: real bytes for loaded segments; unread gaps are zeros.
/// `re_mp4` only **reads** headers of skipped boxes, then **seeks** past bodies.
pub struct SparseBoxReader {
    file_size: u64,
    pos: u64,
    /// Sorted by offset; non-overlapping.
    segments: Vec<(u64, Vec<u8>)>,
    /// After the last [`Read::read`] — used to detect `skip_box(0)` livelock.
    after_read_pos: u64,
}

impl SparseBoxReader {
    pub fn new(file_size: u64, loaded: Vec<LoadedBox>) -> Self {
        let mut segments: Vec<(u64, Vec<u8>)> = loaded
            .into_iter()
            .map(|b| (b.info.offset, b.bytes))
            .collect();
        segments.sort_by_key(|(off, _)| *off);
        Self {
            file_size,
            pos: 0,
            segments,
            after_read_pos: 0,
        }
    }

    /// O(log n) — linear scan over ~240 fMP4 segments froze WASM debug parse for 30 s+.
    fn segment_at(&self, pos: u64) -> Option<&(u64, Vec<u8>)> {
        let i = self.segments.partition_point(|(off, _)| *off <= pos);
        if i == 0 {
            return None;
        }
        let seg = &self.segments[i - 1];
        let (off, bytes) = seg;
        if pos < *off + bytes.len() as u64 {
            Some(seg)
        } else {
            None
        }
    }

    fn next_segment_offset_after(&self, pos: u64) -> u64 {
        let i = self.segments.partition_point(|(off, _)| *off <= pos);
        self.segments
            .get(i)
            .map(|(off, _)| *off)
            .unwrap_or(self.file_size)
    }

    /// Peek up to `len` bytes at `pos` without moving the cursor.
    fn peek_at(&self, pos: u64, len: usize) -> Option<Vec<u8>> {
        if len == 0 || pos >= self.file_size {
            return None;
        }
        let want = len.min((self.file_size - pos) as usize);
        if let Some((off, bytes)) = self.segment_at(pos) {
            let local = (pos - off) as usize;
            let avail = bytes.len() - local;
            let n = want.min(avail);
            return Some(bytes[local..local + n].to_vec());
        }
        // Gap — sparse zeros.
        Some(vec![0u8; want])
    }
}

impl Read for SparseBoxReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.file_size || buf.is_empty() {
            return Ok(0);
        }

        let remaining = (self.file_size - self.pos) as usize;
        let want = buf.len().min(remaining);

        if let Some((off, bytes)) = self.segment_at(self.pos) {
            let local = (self.pos - off) as usize;
            let avail = bytes.len() - local;
            let n = want.min(avail);
            buf[..n].copy_from_slice(&bytes[local..local + n]);
            self.pos += n as u64;
            self.after_read_pos = self.pos;
            Ok(n)
        } else {
            let next = self.next_segment_offset_after(self.pos);
            let gap = (next - self.pos) as usize;
            let n = want.min(gap);
            buf[..n].fill(0);
            self.pos += n as u64;
            self.after_read_pos = self.pos;
            Ok(n)
        }
    }
}

impl Seek for SparseBoxReader {
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        let next = match style {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.file_size as i64 + n,
            SeekFrom::Current(n) => self.pos as i64 + n,
        };
        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        let next = next as u64;
        // re_mp4 `skip_box(0)` on QT padding: after reading an 8-byte size-0 header
        // at `next`, seeks back to `next` → infinite loop. Only refuse that rewind
        // when the header at `next` is actually size 0 (do not break RawBox re-reads
        // of small legitimate boxes).
        if next < self.pos
            && self.pos - next == HEADER_SIZE as u64
            && self.after_read_pos == self.pos
            && let Some(hdr) = self.peek_at(next, HEADER_SIZE)
            && hdr.len() >= 4
            && u32::from_be_bytes(hdr[0..4].try_into().unwrap()) == 0
        {
            return Ok(self.pos);
        }
        self.pos = next;
        Ok(self.pos)
    }
}

/// Progressive parse with optional UI progress hooks.
pub async fn parse_mp4_progressive(source: &dyn MediaSource) -> Result<re_mp4::Mp4, MediaError> {
    parse_mp4_progressive_with_progress(source, |_| {}).await
}

/// Like [`parse_mp4_progressive`], calling `progress` before slow steps.
pub async fn parse_mp4_progressive_with_progress(
    source: &dyn MediaSource,
    mut progress: impl FnMut(&str),
) -> Result<re_mp4::Mp4, MediaError> {
    let file_size = source.size();
    if file_size == 0 {
        return Err(MediaError::Mp4("empty media source".into()));
    }
    progress(&format!(
        "Reading file head (file {} MB)…",
        file_size / (1024 * 1024)
    ));
    #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
    web_sys::console::log_1(&format!("webmedia: scan start file_size={file_size}").into());

    let loaded = load_ftyp_moov_with_progress(source, &mut progress).await?;
    let moov_offset = loaded
        .iter()
        .find(|b| b.info.is_moov())
        .map(|b| b.info.offset)
        .ok_or_else(|| MediaError::Mp4("moov missing".into()))?;
    let moov_size = loaded
        .iter()
        .find(|b| b.info.is_moov())
        .map(|b| b.info.size)
        .unwrap_or(0);
    let loaded_bytes: usize = loaded.iter().map(|b| b.bytes.len()).sum();
    let moof_count = loaded.iter().filter(|b| b.info.is_moof()).count();
    let ftyp_note = loaded
        .iter()
        .find(|b| b.info.is_ftyp())
        .map(|b| format!("ftyp@{}", b.info.offset))
        .unwrap_or_else(|| "no-ftyp".into());
    #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
    web_sys::console::log_1(
        &format!(
            "webmedia: boxes loaded ({ftyp_note}, moov@{moov_offset} size={moov_size}, moofs={moof_count}, {loaded_bytes}B) — parsing"
        )
        .into(),
    );

    progress(&format!(
        "Parsing moov/moof tables ({:.1} KB, may take a moment)…",
        moov_size as f64 / 1024.0
    ));
    crate::util::yield_now().await;
    crate::util::yield_now().await;

    let mut reader = SparseBoxReader::new(file_size, loaded);
    re_mp4::Mp4::read(&mut reader, file_size).map_err(|e| {
        MediaError::Mp4(format!(
            "{e} (progressive: file_size={file_size}, {ftyp_note}, moov@{moov_offset} size={moov_size}, moofs={moof_count}, loaded={loaded_bytes}B)"
        ))
    })
}

/// Contiguous `ftyp`+`moov` only (debug / legacy). Prefer [`parse_mp4_progressive`].
pub async fn read_ftyp_moov_stub(source: &dyn MediaSource) -> Result<Vec<u8>, MediaError> {
    let loaded = load_ftyp_moov(source).await?;
    let mut stub = Vec::new();
    for b in loaded {
        if b.info.is_ftyp() || b.info.is_moov() {
            stub.extend_from_slice(&b.bytes);
        }
    }
    Ok(stub)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;
    use std::path::PathBuf;

    fn block_on_read<F: std::future::Future>(fut: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut fut = Box::pin(fut);
        for _ in 0..64 {
            match fut.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(v) => return v,
                std::task::Poll::Pending => continue,
            }
        }
        panic!("future stayed pending");
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn sanitize_moov_drops_trailing_size_zero_padding() {
        // moov with one fake child (8-byte "mdat"-like) + size-0 padding + short junk.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&28u32.to_be_bytes()); // will be patched
        bytes.extend_from_slice(b"moov");
        bytes.extend_from_slice(&8u32.to_be_bytes());
        bytes.extend_from_slice(b"mvhd"); // fake 8-byte child
        bytes.extend_from_slice(&[0u8; 8]); // size-0 trap
        bytes.extend_from_slice(&[0u8; 4]); // short padding
        assert_eq!(bytes.len(), 28);
        sanitize_moov_padding(&mut bytes).expect("sanitize");
        assert_eq!(bytes.len(), 16); // header + one child
        assert_eq!(&bytes[0..4], &16u32.to_be_bytes());
        assert_eq!(&bytes[4..8], b"moov");
        assert_eq!(&bytes[8..16], &[0, 0, 0, 8, b'm', b'v', b'h', b'd']);
    }

    #[test]
    fn progressive_parse_matches_full_file_offsets() {
        let path = fixture("short_gop.mp4");
        if !path.exists() {
            eprintln!("skip: missing fixture");
            return;
        }
        let bytes = std::fs::read(&path).expect("read fixture");
        let source = MemorySource::from_vec(bytes.clone());

        let full = re_mp4::Mp4::read_bytes(&bytes).expect("full parse");
        let progressive = block_on_read(parse_mp4_progressive(&source)).expect("progressive");

        let full_track = full
            .tracks()
            .values()
            .find(|t| t.kind == Some(re_mp4::TrackKind::Video))
            .expect("video");
        let prog_track = progressive
            .tracks()
            .values()
            .find(|t| t.kind == Some(re_mp4::TrackKind::Video))
            .expect("video");

        assert_eq!(full_track.samples.len(), prog_track.samples.len());
        for (a, b) in full_track.samples.iter().zip(prog_track.samples.iter()) {
            assert_eq!(a.offset, b.offset, "sample offsets must stay file-absolute");
            assert_eq!(a.size, b.size);
            assert_eq!(a.is_sync, b.is_sync);
        }
    }

    #[test]
    fn progressive_parse_loads_moof_for_fragmented_mp4() {
        let path = fixture("fragmented_tiny.mp4");
        if !path.exists() {
            eprintln!("skip: missing fixture");
            return;
        }
        let bytes = std::fs::read(&path).expect("read fixture");
        let source = MemorySource::from_vec(bytes.clone());

        let full = re_mp4::Mp4::read_bytes(&bytes).expect("full parse");
        assert!(
            !full.moofs.is_empty(),
            "fixture must be fragmented (moof present)"
        );

        let progressive = block_on_read(parse_mp4_progressive(&source)).expect("progressive fMP4");
        assert_eq!(full.moofs.len(), progressive.moofs.len());

        let full_track = full
            .tracks()
            .values()
            .find(|t| t.kind == Some(re_mp4::TrackKind::Video))
            .expect("video");
        let prog_track = progressive
            .tracks()
            .values()
            .find(|t| t.kind == Some(re_mp4::TrackKind::Video))
            .expect("video");

        assert!(!full_track.samples.is_empty());
        assert_eq!(full_track.samples.len(), prog_track.samples.len());
        for (a, b) in full_track.samples.iter().zip(prog_track.samples.iter()) {
            assert_eq!(a.offset, b.offset);
            assert_eq!(a.size, b.size);
            assert_eq!(a.is_sync, b.is_sync);
        }
    }

    #[test]
    fn sparse_reader_serves_mdat_header_then_seek() {
        // mdat header @40 size field says 100-byte box; body not loaded.
        let loaded = vec![LoadedBox {
            info: TopLevelBox {
                offset: 40,
                size: 100,
                header_len: 8,
                typ: *b"mdat",
            },
            bytes: {
                let mut v = Vec::new();
                v.extend_from_slice(&100u32.to_be_bytes());
                v.extend_from_slice(b"mdat");
                v
            },
        }];
        let mut r = SparseBoxReader::new(200, loaded);
        r.seek(SeekFrom::Start(40)).unwrap();
        let mut hdr = [0u8; 8];
        assert_eq!(r.read(&mut hdr).unwrap(), 8);
        assert_eq!(&hdr[4..8], b"mdat");
        // After header, gap zeros until we seek.
        r.seek(SeekFrom::Start(40 + 100)).unwrap();
        assert_eq!(r.stream_position().unwrap(), 140);
    }

    #[test]
    fn parse_tfra_collects_moof_offsets() {
        // Minimal mfra with one tfra v1, two entries (hand-rolled).
        let mut tfra = Vec::new();
        // placeholder size
        tfra.extend_from_slice(&0u32.to_be_bytes());
        tfra.extend_from_slice(b"tfra");
        tfra.push(1); // version
        tfra.extend_from_slice(&[0, 0, 0]); // flags
        tfra.extend_from_slice(&1u32.to_be_bytes()); // track_ID
        tfra.extend_from_slice(&0u32.to_be_bytes()); // length sizes all 0 → 1-byte fields
        tfra.extend_from_slice(&2u32.to_be_bytes()); // entry count
        for moof in [1000u64, 5000u64] {
            tfra.extend_from_slice(&0u64.to_be_bytes()); // time
            tfra.extend_from_slice(&moof.to_be_bytes());
            tfra.push(1); // traf
            tfra.push(1); // trun
            tfra.push(1); // sample
        }
        let tfra_size = tfra.len() as u32;
        tfra[0..4].copy_from_slice(&tfra_size.to_be_bytes());

        let mut mfra = Vec::new();
        mfra.extend_from_slice(&0u32.to_be_bytes());
        mfra.extend_from_slice(b"mfra");
        mfra.extend_from_slice(&tfra);
        let mfra_size = mfra.len() as u32;
        mfra[0..4].copy_from_slice(&mfra_size.to_be_bytes());

        let offs = parse_mfra_moof_offsets(&mfra).expect("parse");
        assert_eq!(offs, vec![1000, 5000]);
    }

    #[test]
    fn synthesize_header_roundtrip_size() {
        let info = TopLevelBox {
            offset: 40,
            size: 100,
            header_len: 8,
            typ: *b"mdat",
        };
        let bytes = synthesize_box_header(&info);
        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[4..8], b"mdat");
        assert_eq!(u32::from_be_bytes(bytes[0..4].try_into().unwrap()), 100);
    }

    #[test]
    fn coalesce_packs_fmp4_style_gaps_into_mb_runs() {
        // moof @0 (400), mdat hdr @400 (8), moof @200_000 (400) — second moof
        // sits inside a 1 MiB window from the first run start.
        let needs = vec![(0usize, 0u64, 400usize), (1, 400, 8), (2, 200_000, 400)];
        let runs = coalesce_byte_runs(&needs);
        assert_eq!(runs.len(), 1, "expected one packed run, got {}", runs.len());
        assert_eq!(runs[0].0, 0);
        assert_eq!(runs[0].1, 200_400);
        assert_eq!(runs[0].2.len(), 3);
    }

    #[test]
    fn coalesce_splits_when_over_max_run() {
        let needs = vec![
            (0usize, 0u64, 100usize),
            (1, (MAX_COALESCE_RUN as u64) + 10, 100),
        ];
        let runs = coalesce_byte_runs(&needs);
        assert_eq!(runs.len(), 2);
    }
}
