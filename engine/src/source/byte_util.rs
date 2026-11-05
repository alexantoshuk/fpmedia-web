//! Shared byte-source helpers (native-testable; used by Url/Blob/Cached).

/// Readable length from `offset` given total `size`, clamped to `usize`.
///
/// On wasm32, a raw `(size - offset) as usize` truncates — when the remainder
/// is a multiple of 2³² the cast becomes 0 and reads stop early on ≥4 GiB
/// media. Prefer this everywhere a read budget is derived from `u64` sizes.
pub(crate) fn available_len(size: u64, offset: u64) -> usize {
    if offset >= size {
        return 0;
    }
    usize::try_from(size - offset).unwrap_or(usize::MAX)
}

/// Parsed `Content-Range` (HTTP partial response).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(all(target_arch = "wasm32", feature = "webcodecs")),
    allow(dead_code)
)]
pub(crate) struct ContentRange {
    pub start: u64,
    pub end: Option<u64>,
    pub total: Option<u64>,
}

/// Parse `Content-Range: bytes START-END/TOTAL` (TOTAL may be `*`).
///
/// Returns `None` when the header is missing the `bytes` unit or START.
#[cfg_attr(
    not(all(target_arch = "wasm32", feature = "webcodecs")),
    allow(dead_code)
)]
pub(crate) fn parse_content_range(cr: &str) -> Option<ContentRange> {
    let s = cr.trim();
    let rest = s
        .strip_prefix("bytes ")
        .or_else(|| s.strip_prefix("bytes="))?;
    let (range, total_part) = rest.split_once('/')?;
    let total = if total_part == "*" {
        None
    } else {
        Some(total_part.parse().ok()?)
    };
    if range == "*" {
        // `bytes */TOTAL` — no start (not useful for offset checks).
        return None;
    }
    let (start_s, end_s) = range.split_once('-')?;
    let start: u64 = start_s.parse().ok()?;
    let end = if end_s.is_empty() {
        None
    } else {
        Some(end_s.parse().ok()?)
    };
    Some(ContentRange { start, end, total })
}

/// Truncate a URL for error messages without slicing mid-UTF-8.
#[cfg_attr(
    not(all(target_arch = "wasm32", feature = "webcodecs")),
    allow(dead_code)
)]
pub(crate) fn shorten_url(url: &str, max_bytes: usize) -> String {
    if url.len() <= max_bytes {
        return url.to_string();
    }
    let mut end = max_bytes.min(url.len());
    while end > 0 && !url.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &url[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_len_clamps_past_usize_max() {
        assert_eq!(available_len(100, 0), 100);
        assert_eq!(available_len(100, 100), 0);
        assert_eq!(available_len(100, 101), 0);
        // Exactly 4 GiB remaining would truncate to 0 with a raw `as usize` on
        // wasm32; try_from yields usize::MAX (or the value on 64-bit hosts).
        let four_gib = 1u64 << 32;
        let got = available_len(four_gib, 0);
        assert!(got > 0, "4 GiB available must not become 0");
        assert_eq!(got, usize::try_from(four_gib).unwrap_or(usize::MAX));
    }

    #[test]
    fn parse_content_range_start_and_total() {
        assert_eq!(
            parse_content_range("bytes 0-0/1234"),
            Some(ContentRange {
                start: 0,
                end: Some(0),
                total: Some(1234),
            })
        );
        assert_eq!(
            parse_content_range("bytes 100-199/1000"),
            Some(ContentRange {
                start: 100,
                end: Some(199),
                total: Some(1000),
            })
        );
        assert_eq!(
            parse_content_range("bytes 50-99/*"),
            Some(ContentRange {
                start: 50,
                end: Some(99),
                total: None,
            })
        );
        assert_eq!(parse_content_range("bytes */1000"), None);
        assert_eq!(parse_content_range("invalid"), None);
    }

    #[test]
    fn shorten_url_respects_utf8_boundaries() {
        // "й" is 2 bytes in UTF-8 — cutting at byte 1 must not panic.
        let url = "https://example.com/".to_string() + &"й".repeat(80);
        let s = shorten_url(&url, 96);
        assert!(s.ends_with('…'));
        assert!(s.len() <= 96 + "…".len());
        // Prefix before ellipsis is valid UTF-8 (construction did not panic).
        let stem = s.strip_suffix('…').expect("ellipsis");
        assert!(std::str::from_utf8(stem.as_bytes()).is_ok());
    }

    #[test]
    fn shorten_url_short_unchanged() {
        assert_eq!(shorten_url("http://a.test/x", 96), "http://a.test/x");
    }
}
