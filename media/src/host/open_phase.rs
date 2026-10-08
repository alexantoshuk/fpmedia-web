//! Progressive open lifecycle phases (UI status + future WorkloadMode::Open).
//!
//! Replaces free-form `progress("&str")` strings along demux / host open.
//! See `.ai/architecture.md` § State machines over flags.
//!
//! mediabunny: `src/isobmff/isobmff-demuxer.ts` — metadata after `moov`, then
//! `lookForMfraBox` / `readFragment` for fragments. Host `Input` + track discovery
//! is “ready for sinks”; our phases name the same pipeline stages without changing
//! Range / warm / sync policy.

use std::fmt;

/// Where the open path is probing bytes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenSourceKind {
    Blob,
    Url,
    Progressive {
        size_kb: u64,
    },
    FullDownload {
        /// Human size label (e.g. `12 MB`).
        size_label: String,
    },
}

/// Typed open pipeline stage.
#[derive(Clone, Debug, PartialEq)]
pub enum OpenPhase {
    Idle,
    /// Host: start of URL/blob resolve.
    ProbingSource(OpenSourceKind),
    ProbingHttpRange,
    /// Host fell back after non-206 (before blob fetch completes).
    NoHttpRangeDownload {
        /// Optional ` (~12 MB)` suffix.
        size_hint: String,
    },
    OpeningUrl {
        url: String,
    },
    StartingDemux,
    ReadingFileHead {
        file_mb: u64,
    },
    ProbingMfra,
    MfraFallbackScan {
        reason: String,
    },
    ScanningBoxes,
    LoadingFragmentedMoofs {
        count: usize,
    },
    ReadingMoov {
        offset: u64,
        kb: f64,
    },
    MfraIndex {
        fragments: u32,
    },
    WarmingFragments {
        warm: u32,
        total: u32,
    },
    FetchingMoofTables {
        start: u32,
        total: u32,
    },
    IndexingFragments {
        done: u32,
        total: u32,
    },
    RebuildingIndex,
    ParsingMoov {
        kb: f64,
    },
    FetchingBoxBodies {
        ranges: usize,
        boxes: usize,
    },
    IndexingBackfill,
    VerifyingSync {
        done: usize,
        total: usize,
    },
    VerifyingSyncDeferred {
        candidates: usize,
    },
    WarmingMediaBytes,
    WarmingAudioBytes,
    CreatingDecoder {
        frames: u64,
        has_audio: bool,
    },
    DecodingStart,
    Ready,
    Failed,
    Cancelled,
    SeekStartFailed,
}

impl OpenPhase {
    /// UI status line (keeps prior copy where practical).
    pub fn label(&self) -> String {
        match self {
            Self::Idle => "Idle.".into(),
            Self::ProbingSource(OpenSourceKind::Blob) => "Opening blob…".into(),
            Self::ProbingSource(OpenSourceKind::Url) => "Opening…".into(),
            Self::ProbingSource(OpenSourceKind::Progressive { size_kb }) => {
                format!("Opening (progressive, ~{size_kb} KB)…")
            }
            Self::ProbingSource(OpenSourceKind::FullDownload { size_label }) => {
                format!("Opening (full download, ~{size_label})…")
            }
            Self::ProbingHttpRange => "Probing HTTP Range…".into(),
            Self::NoHttpRangeDownload { size_hint } => {
                format!("No HTTP Range — downloading full file{size_hint}…")
            }
            Self::OpeningUrl { url } => format!("Opening {url}…"),
            Self::StartingDemux => "Starting demux…".into(),
            Self::ReadingFileHead { file_mb } => {
                format!("Reading file head (file {file_mb} MB)…")
            }
            Self::ProbingMfra => "Probing mfra index…".into(),
            Self::MfraFallbackScan { reason } => {
                format!("mfra open failed ({reason}); scanning file…")
            }
            Self::ScanningBoxes => "Scanning top-level boxes…".into(),
            Self::LoadingFragmentedMoofs { count } => {
                format!("Fragmented MP4: loading {count} moof box(es)…")
            }
            Self::ReadingMoov { offset, kb } => {
                format!("Reading moov @ {offset} ({kb:.1} KB)…")
            }
            Self::MfraIndex { fragments } => format!("mfra index: {fragments} fragments…"),
            Self::WarmingFragments { warm, total } => {
                format!("Warming first {warm}/{total} fragments for playback…")
            }
            Self::FetchingMoofTables { start, total } => {
                format!("Fetching moof tables ({start}/{total})…")
            }
            Self::FetchingBoxBodies { ranges, boxes } => {
                format!("Fetching {ranges} range(s) for {boxes} box body(ies)…")
            }
            Self::IndexingFragments { done, total } => {
                format!("Indexing fragments ({done}/{total})…")
            }
            Self::RebuildingIndex => "Rebuilding frame index…".into(),
            Self::ParsingMoov { kb } => {
                format!("Parsing moov/moof tables ({kb:.1} KB, may take a moment)…")
            }
            Self::IndexingBackfill => "Indexing remaining fragments…".into(),
            Self::VerifyingSync { done, total } => {
                format!("Verifying keyframes ({done}/{total})…")
            }
            Self::VerifyingSyncDeferred { candidates } => {
                format!("Keyframe verify deferred ({candidates} candidates, background)…")
            }
            Self::WarmingMediaBytes => "Warming media bytes…".into(),
            Self::WarmingAudioBytes => "Warming audio bytes…".into(),
            Self::CreatingDecoder { frames, has_audio } => format!(
                "moov ok — {frames} frames{}, creating decoder…",
                if *has_audio { " + audio" } else { "" }
            ),
            Self::DecodingStart => "Decoding frame 0…".into(),
            Self::Ready => "Ready.".into(),
            Self::Failed => "Open failed.".into(),
            Self::Cancelled => "Open cancelled.".into(),
            Self::SeekStartFailed => "Seek 0 failed.".into(),
        }
    }
}

impl fmt::Display for OpenPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warming_bytes_label() {
        assert_eq!(OpenPhase::WarmingMediaBytes.label(), "Warming media bytes…");
        assert_eq!(OpenPhase::WarmingAudioBytes.label(), "Warming audio bytes…");
    }

    #[test]
    fn creating_decoder_label() {
        let s = OpenPhase::CreatingDecoder {
            frames: 240,
            has_audio: true,
        }
        .label();
        assert!(s.contains("240"));
        assert!(s.contains("audio"));
        assert!(s.contains("creating decoder"));
    }

    #[test]
    fn verify_progress_label() {
        assert_eq!(
            OpenPhase::VerifyingSync { done: 4, total: 20 }.label(),
            "Verifying keyframes (4/20)…"
        );
    }

    #[test]
    fn host_terminal_labels() {
        assert_eq!(OpenPhase::Ready.label(), "Ready.");
        assert_eq!(OpenPhase::Cancelled.label(), "Open cancelled.");
        assert_eq!(OpenPhase::Failed.label(), "Open failed.");
    }
}
