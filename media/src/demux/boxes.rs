//! Top-level ISOBMFF / QuickTime box scan for progressive open.
//!
//! `re_mp4` reads every top-level **header** before `skip_box` seeks past `mdat`.
//! Gaps filled with zeros look like `size=0` and abort the parse — so we materialize
//! headers for skipped boxes and full `ftyp`+`moov` (+ `moof` for fMP4) at
//! **file-absolute** offsets.
//!
//! mediabunny: `src/isobmff/isobmff-demuxer.ts` `readFragment` loads the entire
//! `moof` via `requestSlice(startPos, moofBoxInfo.totalSize)`.

use std::io::{self, Read, Seek, SeekFrom};

use crate::error::MediaError;
use crate::source::MediaSource;

const HEADER_SIZE: usize = 8;
const EXTENDED_HEADER_SIZE: usize = 16;
/// Reject absurdly large `moov` (corruption / wrong container).
const MAX_MOOV_BYTES: u64 = 64 * 1024 * 1024;
/// Reject absurdly large `moof` (corruption). Bodies are small metadata tables.
const MAX_MOOF_BYTES: u64 = 16 * 1024 * 1024;

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

/// Walk all top-level boxes (`moov` required).
///
/// Loads **headers** for every top-level atom (`free` / `wide` / `mdat` / …).
/// Stopping at `moov` leaves later headers as sparse zeros; `re_mp4` then hits a
/// size-0 box and `skip_box(0)` infinite-loops (browser freeze on large files).
pub async fn scan_top_level(source: &dyn MediaSource) -> Result<Vec<TopLevelBox>, MediaError> {
    let file_size = source.size();
    if file_size == 0 {
        return Err(MediaError::Mp4("empty media source".into()));
    }

    // One ranged read covers typical leading atoms; moov-at-end only needs this
    // to learn `mdat` size, then a second read loads moov.
    let head_len = (file_size as usize).min(HEAD_PREFETCH);
    let head = source.read(0, head_len).await?;
    if head.len() < HEADER_SIZE {
        return Err(MediaError::Mp4(format!(
            "truncated file head (got {} bytes)",
            head.len()
        )));
    }

    let mut boxes = Vec::new();
    let mut offset = 0u64;
    let mut saw_moov = false;

    while offset < file_size {
        let (header_len, box_size, typ) =
            read_box_header_at(source, &head, offset, file_size).await?;

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
        boxes.push(entry);

        offset = offset.saturating_add(box_size);
    }

    if !saw_moov {
        return Err(MediaError::Mp4(
            "no moov box found (fragmented / unsupported)".into(),
        ));
    }

    Ok(boxes)
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

async fn read_box_header_at(
    source: &dyn MediaSource,
    head: &[u8],
    offset: u64,
    file_size: u64,
) -> Result<(u64, u64, [u8; 4]), MediaError> {
    let header = if offset + HEADER_SIZE as u64 <= head.len() as u64 {
        head[offset as usize..offset as usize + HEADER_SIZE].to_vec()
    } else {
        source.read(offset, HEADER_SIZE).await?
    };
    if header.len() < HEADER_SIZE {
        return Err(MediaError::Mp4(format!(
            "truncated box header at offset {offset} (got {} bytes, file_size={file_size})",
            header.len()
        )));
    }

    let size32 = u32::from_be_bytes(header[0..4].try_into().unwrap());
    let typ: [u8; 4] = header[4..8].try_into().unwrap();

    let (header_len, box_size) = if size32 == 1 {
        let ext = if offset + EXTENDED_HEADER_SIZE as u64 <= head.len() as u64 {
            head[offset as usize + 8..offset as usize + 16].to_vec()
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
    progress("Scanning top-level boxes…");
    let boxes = scan_top_level(source).await?;
    let moof_count = boxes.iter().filter(|b| b.is_moof()).count();
    if moof_count > 0 {
        progress(&format!(
            "Fragmented MP4: loading {moof_count} moof box(es)…"
        ));
    }
    let mut loaded = Vec::new();

    for mut info in boxes {
        let need = if info.needs_full_body() {
            info.size as usize
        } else {
            usize::from(info.header_len)
        };
        if info.is_moov() {
            progress(&format!(
                "Reading moov @ {} ({:.1} KB)…",
                info.offset,
                need as f64 / 1024.0
            ));
            crate::util::yield_now().await;
        } else if info.is_ftyp() {
            progress("Reading ftyp…");
        } else if info.is_moof() {
            crate::util::yield_now().await;
        }
        let mut bytes = source.read(info.offset, need).await?;
        if bytes.len() != need {
            return Err(MediaError::Mp4(format!(
                "short {} read at {}: got {} want {}",
                info.type_str(),
                info.offset,
                bytes.len(),
                need
            )));
        }
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

    fn segment_at(&self, pos: u64) -> Option<&(u64, Vec<u8>)> {
        self.segments
            .iter()
            .find(|(off, bytes)| pos >= *off && pos < off + bytes.len() as u64)
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
            let next = self
                .segments
                .iter()
                .map(|(off, _)| *off)
                .find(|&off| off > self.pos)
                .unwrap_or(self.file_size);
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
}
