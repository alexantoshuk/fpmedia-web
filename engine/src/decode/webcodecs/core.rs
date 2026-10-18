//! Owned WebCodecs decoder state ([`DecoderCore`]) plus its config / buffers.
//!
//! Every method here is synchronous; the awaiting helpers live in
//! [`super::pump`]. Never keep a borrow across `.await`, `decoder.decode()`, or
//! a `Closure` call (`.ai/skills/media.md`).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use js_sys::Uint8Array;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    EncodedVideoChunk, EncodedVideoChunkInit, EncodedVideoChunkType, VideoDecoder,
    VideoDecoderConfig, VideoDecoderInit, VideoFrame,
};

use crate::bitstream::{
    avc_nal_length_size, has_hevc_rasl_picture, hevc_nal_length_size,
    sanitize_avc_packet_for_chromium, sanitize_hevc_packet_for_chromium,
};
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::error::MediaError;
use crate::types::{EncodedSample, FrameId, VideoMetadata};

use super::pump::{
    is_chromium, promise_race, sleep_promise_ms, ticks_to_micros, wake_waiters_deferred,
};

/// Restores [`DecoderCore::prune_floor_us`] when a consumer decode returns
/// (including cancel / early Ok).
pub(super) struct PruneFloorGuard {
    core: Rc<RefCell<DecoderCore>>,
    prev: Option<i64>,
}

impl PruneFloorGuard {
    pub(super) fn arm(core: &Rc<RefCell<DecoderCore>>, floor_us: i64) -> Self {
        let prev = core.borrow().prune_floor_us.replace(Some(floor_us));
        Self {
            core: Rc::clone(core),
            prev,
        }
    }
}

impl Drop for PruneFloorGuard {
    fn drop(&mut self) {
        if let Ok(c) = self.core.try_borrow() {
            c.prune_floor_us.set(self.prev);
        }
    }
}

/// WebCodecs is `SecureContext`-only. LAN `http://192.168.x.x` (phone → desktop
/// `trunk serve`) leaves `VideoDecoder` undefined; calling
/// `isConfigSupported` then panics the open task and freezes status on
/// "creating decoder…".
pub(super) fn ensure_webcodecs_available() -> Result<(), MediaError> {
    let Some(window) = web_sys::window() else {
        return Err(MediaError::WebCodecs(
            "no window (WebCodecs needs a browser)".into(),
        ));
    };
    if !window.is_secure_context() {
        return Err(MediaError::WebCodecsInsecureContext);
    }
    let global = js_sys::global();
    let decoder = js_sys::Reflect::get(&global, &JsValue::from_str("VideoDecoder"))
        .unwrap_or(JsValue::UNDEFINED);
    if decoder.is_undefined() || decoder.is_null() {
        return Err(MediaError::WebCodecs(
            "VideoDecoder API not available in this browser".into(),
        ));
    }
    Ok(())
}

/// Max in-flight encoded chunks when decoded frames are already buffered
/// (mediabunny `computeMaxQueueSize` when `decodedSampleQueueSize > 0`).
const MAX_DECODE_QUEUE_WITH_HELD: u32 = 8;
/// Cap when the held/output buffers are empty — some browsers need a deeper
/// encoded queue before emitting reordered frames.
const MAX_DECODE_QUEUE_EMPTY: u32 = 40;
/// Max delta packets to feed after the target's decode_order to coax B-frame emit
/// before flushing. Keep small so play/prefetch does not strand a deep decodeQ
/// when the consumer returns (PumpStopGuard). Seek still feeds to GOP end when
/// the target is not yet held (`holds_target` gate on early stop).
pub(super) const REORDER_FEED_SLACK: usize = 8;
/// Only salvage pruned frames within this PTS gap of the seek target. Farther
/// frames are closed immediately — keeping every key→N VideoFrame alive exhausts
/// Chromium's pool and wedges at `decodeQ≈41, held=0` (long-GOP seek/settle).
/// ~8 frames @24 fps / ~10 @30 fps; densify still fills the rest after exact.
const INTERIM_SALVAGE_MAX_BEHIND_US: i64 = 333_333;

pub(super) fn compute_max_queue_size(decoded_sample_queue_size: u32) -> u32 {
    // mediabunny `computeMaxQueueSize`
    if decoded_sample_queue_size == 0 {
        MAX_DECODE_QUEUE_EMPTY
    } else {
        MAX_DECODE_QUEUE_WITH_HELD
    }
}

/// WebCodecs `hardwareAcceleration` hint.
///
/// Scrub/thumb use [`Self::PreferSoftware`] so they never steal the main HW
/// session (`no-preference` can still pick HW under load → Chrome
/// `Unsupported configuration` on main recreate).
/// mediabunny: `src/media-sink.ts` VideoDecoderWrapper alpha path
/// `hardwareAcceleration: 'prefer-software'`; default docs leave
/// `'no-preference'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HwAccel {
    NoPreference,
    PreferHardware,
    PreferSoftware,
}

impl HwAccel {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::NoPreference => "no-preference",
            Self::PreferHardware => "prefer-hardware",
            Self::PreferSoftware => "prefer-software",
        }
    }
}

#[derive(Clone)]
pub(super) struct DecoderConfig {
    pub(super) codec: String,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) description: Vec<u8>,
}

impl DecoderConfig {
    pub(super) fn from_meta(meta: &VideoMetadata) -> Result<Self, MediaError> {
        Ok(Self {
            codec: meta
                .codec
                .codec_string
                .clone()
                .ok_or(MediaError::UnsupportedCodec)?,
            width: meta.width,
            height: meta.height,
            description: meta.codec.description.clone(),
        })
    }

    /// Build a WebCodecs config.
    ///
    /// Main open probes [`HwAccel::NoPreference`] first — requiring
    /// `prefer-hardware` on the support check falsely fails AVC on some hosts.
    /// Prefer-hardware is applied only when the play policy asked for it **and**
    /// a separate HW support probe passed
    /// (mediabunny: `decoderOptions.hardwareAcceleration`, PR #406).
    pub(super) fn to_web_config(
        &self,
        optimize_for_latency: bool,
        hw: HwAccel,
    ) -> VideoDecoderConfig {
        let config = VideoDecoderConfig::new(&self.codec);
        config.set_coded_width(self.width);
        config.set_coded_height(self.height);
        config.set_description(&Uint8Array::from(self.description.as_slice()));
        config.set_optimize_for_latency(optimize_for_latency);
        match hw {
            HwAccel::NoPreference => {}
            HwAccel::PreferHardware => {
                config.set_hardware_acceleration(web_sys::HardwareAcceleration::PreferHardware);
            }
            HwAccel::PreferSoftware => {
                config.set_hardware_acceleration(web_sys::HardwareAcceleration::PreferSoftware);
            }
        }
        config
    }
}

pub(super) struct HeldFrame {
    pub(super) pts_us: i64,
    pub(super) frame: OwnedVideoFrame,
}

/// GOP densify drain target: PTS→id map plus the consumer callback.
pub(super) type GopSink<'a> = (
    &'a mut PtsToFrameIds,
    &'a mut dyn FnMut(FrameId, OwnedVideoFrame),
);

/// Decoder output PTS → one or more presentation [`FrameId`]s.
///
/// Duplicate composition timestamps in a GOP must not overwrite (HashMap
/// single-value insert dropped 1/N densify frames: gop_len=33 outputs=32).
/// `mediabunny: src/media-sink.ts` `inputTimestamps.shift()` — FIFO per stamp.
pub(super) type PtsToFrameIds = HashMap<i64, VecDeque<FrameId>>;

pub(super) fn pts_map_push(map: &mut PtsToFrameIds, pts_us: i64, id: FrameId) {
    map.entry(pts_us).or_default().push_back(id);
}

pub(super) fn pts_map_take(map: &mut PtsToFrameIds, pts_us: i64) -> Option<FrameId> {
    let pop = |map: &mut PtsToFrameIds, key: i64| -> Option<FrameId> {
        let q = map.get_mut(&key)?;
        let id = q.pop_front()?;
        if q.is_empty() {
            map.remove(&key);
        }
        Some(id)
    };
    pop(map, pts_us)
        .or_else(|| pop(map, pts_us.saturating_sub(1)))
        .or_else(|| pop(map, pts_us.saturating_add(1)))
}

/// Hands pruned PTS `< target` frames to the host instead of closing them.
///
/// Settle/seek installs this so one key→N cascade both yields exact **and** warms
/// dense near the target (densify after exact is cheaper). Host must **not** blit —
/// only ingest into the scrub store (blit looked like play from GOP start).
///
/// Only frames within [`INTERIM_SALVAGE_MAX_BEHIND_US`] of `target_us` are kept;
/// farther ones are closed so the VideoFrame pool stays drained (mediabunny closes
/// early samples in the output callback).
pub(super) struct PruneInterim {
    pub(super) pts_to_id: PtsToFrameIds,
    pub(super) target_us: i64,
    pub(super) on_frame: Box<dyn FnMut(FrameId, OwnedVideoFrame)>,
}

/// Owned decoder state shared by the packet pump and the frame consumer.
///
/// Every method here is synchronous: awaits happen in the free async helpers
/// in [`super::pump`], which take `&Rc<RefCell<DecoderCore>>` and re-borrow per
/// turn.
pub(super) struct DecoderCore {
    pub(super) decoder: VideoDecoder,
    pub(super) config: DecoderConfig,
    pub(super) optimize_for_latency: bool,
    /// HW hint last used on configure (main may PreferHardware; scrub/thumb PreferSoftware).
    pub(super) hw_accel: HwAccel,
    /// Pump warm frames past consumer target
    /// ([`crate::policy::PlayPolicy::decode_warm_ahead`]).
    pub(super) warm_ahead: usize,
    /// Bumped on teardown so late browser callbacks become no-ops.
    pub(super) generation: Rc<Cell<u64>>,
    /// Bumped on detach/reset/invalidate/Drop so in-flight pumps stop feeding.
    pub(super) pump_epoch: Rc<Cell<u64>>,
    pub(super) frames: Rc<RefCell<Vec<OwnedVideoFrame>>>,
    pub(super) error: Rc<RefCell<Option<String>>>,
    /// Resolvers waiting for the next `output` callback (mediabunny-style).
    pub(super) output_waiters: Rc<RefCell<Vec<js_sys::Function>>>,
    /// Resolvers waiting for `dequeue` (decodeQueueSize decreased).
    pub(super) dequeue_waiters: Rc<RefCell<Vec<js_sys::Function>>>,
    /// Resolvers waiting for `held` to gain a remapped frame
    /// (mediabunny `onQueueNotEmpty`) — the pump can fill `held` while the
    /// consumer waits, and only this wakes it.
    pub(super) not_empty_waiters: Rc<RefCell<Vec<js_sys::Function>>>,
    pub(super) fed_through_order: Option<usize>,
    pub(super) key_order: Option<usize>,
    /// Sorted PTS (µs) of fed chunks — shifted onto outputs in presentation order.
    pub(super) input_timestamps: Vec<i64>,
    pub(super) held: Vec<HeldFrame>,
    /// Packets fed since last `configure` (mediabunny `currentPacketIndex`).
    pub(super) packets_fed: usize,
    /// HEVC: finished dropping post-CRA RASL (mediabunny `raslSkipped`).
    pub(super) rasl_skipped: bool,
    /// When true, every sample is a sync — soft key switch is OK (no long cascade).
    pub(super) all_intra: bool,
    /// Optional: salvage pruned early frames during key→N into the host dense cache.
    pub(super) prune_interim: RefCell<Option<PruneInterim>>,
    /// While a consumer waits for an exact aim PTS, never prune that PTS away
    /// even if the live play cursor races ahead (Chrome: `held=225 near=83333`
    /// while aim=`41666`). Soft floor = `min(prune_target, floor)`.
    pub(super) prune_floor_us: Cell<Option<i64>>,
    _on_output: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
    _on_dequeue: Closure<dyn FnMut(JsValue)>,
}

impl DecoderCore {
    pub(super) fn from_config(
        config: DecoderConfig,
        optimize_for_latency: bool,
        hw_accel: HwAccel,
        warm_ahead: usize,
        pump_epoch: Rc<Cell<u64>>,
        all_intra: bool,
    ) -> Result<Self, MediaError> {
        let generation = Rc::new(Cell::new(0));
        let frames: Rc<RefCell<Vec<OwnedVideoFrame>>> = Rc::new(RefCell::new(Vec::new()));
        let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let output_waiters: Rc<RefCell<Vec<js_sys::Function>>> = Rc::new(RefCell::new(Vec::new()));
        let dequeue_waiters: Rc<RefCell<Vec<js_sys::Function>>> = Rc::new(RefCell::new(Vec::new()));
        let not_empty_waiters: Rc<RefCell<Vec<js_sys::Function>>> =
            Rc::new(RefCell::new(Vec::new()));

        let gen_cb = Rc::clone(&generation);
        let my_gen = gen_cb.get();
        let frames_cb = Rc::clone(&frames);
        let waiters_cb = Rc::clone(&output_waiters);
        let dequeue_on_output = Rc::clone(&dequeue_waiters);
        let on_output = Closure::wrap(Box::new(move |value: JsValue| {
            if gen_cb.get() != my_gen {
                if let Ok(frame) = value.dyn_into::<VideoFrame>() {
                    drop(OwnedVideoFrame::new(frame));
                }
                return;
            }
            if let Ok(frame) = value.dyn_into::<VideoFrame>() {
                frames_cb.borrow_mut().push(OwnedVideoFrame::new(frame));
            }
            let waiting = std::mem::take(&mut *waiters_cb.borrow_mut());
            // Defer resolves — sync wake from inside this Closure re-enters Rust
            // (decode → recreate → drop Closure) and panics with
            // "closure invoked recursively or after being dropped".
            wake_waiters_deferred(waiting);
            // mediabunny: `onQueueDequeue()` on every decoder output (even when
            // the sample will be closed) so the encode pump unblocks.
            let waiting = std::mem::take(&mut *dequeue_on_output.borrow_mut());
            wake_waiters_deferred(waiting);
        }) as Box<dyn FnMut(JsValue)>);

        let gen_err = Rc::clone(&generation);
        let my_gen_err = gen_err.get();
        let error_cb = Rc::clone(&error);
        let on_error = Closure::wrap(Box::new(move |value: JsValue| {
            if gen_err.get() != my_gen_err {
                return;
            }
            let msg = value
                .as_string()
                .or_else(|| {
                    js_sys::Reflect::get(&value, &"message".into())
                        .ok()
                        .and_then(|v| v.as_string())
                })
                .unwrap_or_else(|| format!("{value:?}"));
            *error_cb.borrow_mut() = Some(msg);
        }) as Box<dyn FnMut(JsValue)>);

        let gen_dq = Rc::clone(&generation);
        let my_gen_dq = gen_dq.get();
        let dequeue_cb = Rc::clone(&dequeue_waiters);
        let on_dequeue = Closure::wrap(Box::new(move |_value: JsValue| {
            if gen_dq.get() != my_gen_dq {
                return;
            }
            let waiting = std::mem::take(&mut *dequeue_cb.borrow_mut());
            wake_waiters_deferred(waiting);
        }) as Box<dyn FnMut(JsValue)>);

        let init = VideoDecoderInit::new(
            on_error.as_ref().unchecked_ref(),
            on_output.as_ref().unchecked_ref(),
        );
        let decoder = VideoDecoder::new(&init).map_err(MediaError::from)?;
        decoder.set_ondequeue(Some(on_dequeue.as_ref().unchecked_ref()));
        // HW support can vanish under dual-decoder load (W3C: resources exhausted).
        // Prefer-hardware recreate without re-probe → Chrome
        // `Unsupported configuration` + poison. Fall back to PreferSoftware once.
        let hw_accel = match decoder
            .configure(&config.to_web_config(optimize_for_latency, hw_accel))
        {
            Ok(()) => hw_accel,
            Err(e) if hw_accel == HwAccel::PreferHardware => {
                web_sys::console::warn_1(
                    &format!(
                        "fpmedia-web: prefer-hardware configure failed ({e:?}); prefer-software fallback"
                    )
                    .into(),
                );
                let soft = HwAccel::PreferSoftware;
                decoder
                    .configure(&config.to_web_config(optimize_for_latency, soft))
                    .map_err(MediaError::from)?;
                soft
            }
            Err(e) => return Err(MediaError::from(e)),
        };

        Ok(Self {
            decoder,
            config,
            optimize_for_latency,
            hw_accel,
            warm_ahead: warm_ahead.max(1),
            generation,
            pump_epoch,
            frames,
            error,
            output_waiters,
            dequeue_waiters,
            not_empty_waiters,
            fed_through_order: None,
            key_order: None,
            input_timestamps: Vec::new(),
            held: Vec::new(),
            packets_fed: 0,
            rasl_skipped: false,
            all_intra,
            prune_interim: RefCell::new(None),
            prune_floor_us: Cell::new(None),
            _on_output: on_output,
            _on_error: on_error,
            _on_dequeue: on_dequeue,
        })
    }

    /// Close the browser decoder and retire callbacks without dropping Closures
    /// that JS may still invoke (would panic inside wasm-bindgen).
    pub(super) fn detach_decoder(&mut self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        // Any pump still awaiting a sample read must not feed the next decoder.
        self.pump_epoch.set(self.pump_epoch.get().wrapping_add(1));
        self.decoder.set_ondequeue(None);
        let _ = self.decoder.close();
        for frame in self.frames.borrow_mut().drain(..) {
            drop(frame);
        }
        self.clear_held();
        self.input_timestamps.clear();
        self.packets_fed = 0;
        self.rasl_skipped = false;
        let waiting = std::mem::take(&mut *self.output_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
        let waiting = std::mem::take(&mut *self.dequeue_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
        let waiting = std::mem::take(&mut *self.not_empty_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
        *self.error.borrow_mut() = None;
    }

    pub(super) fn recreate(&mut self) -> Result<(), MediaError> {
        // Keep dense-salvage sink across hard reset (All-Intra / wedge recovery).
        let interim = self.prune_interim.borrow_mut().take();
        let all_intra = self.all_intra;
        self.detach_decoder();
        let config = self.config.clone();
        let optimize_for_latency = self.optimize_for_latency;
        let hw_accel = self.hw_accel;
        let warm_ahead = self.warm_ahead;
        let pump_epoch = Rc::clone(&self.pump_epoch);
        // Retire Closures before `*self = …` Drop — JS may still invoke them.
        let old_output = std::mem::replace(&mut self._on_output, Closure::new(|_| {}));
        let old_error = std::mem::replace(&mut self._on_error, Closure::new(|_| {}));
        let old_dequeue = std::mem::replace(&mut self._on_dequeue, Closure::new(|_| {}));
        std::mem::forget(old_output);
        std::mem::forget(old_error);
        std::mem::forget(old_dequeue);

        *self = Self::from_config(
            config,
            optimize_for_latency,
            hw_accel,
            warm_ahead,
            pump_epoch,
            all_intra,
        )?;
        *self.prune_interim.borrow_mut() = interim;
        Ok(())
    }

    /// Sticky PreferSoftware + recreate (All-Intra / mobile: HW configure OK but
    /// never emits). Keeps the same config description.
    /// mediabunny: `src/media-sink.ts` alpha `hardwareAcceleration: prefer-software`.
    pub(super) fn force_prefer_software(&mut self) -> Result<(), MediaError> {
        if self.hw_accel == HwAccel::PreferSoftware {
            return Ok(());
        }
        web_sys::console::warn_1(
            &format!(
                "fpmedia-web: {} produced no frame; sticky prefer-software",
                self.hw_accel.label()
            )
            .into(),
        );
        self.hw_accel = HwAccel::PreferSoftware;
        self.recreate()
    }

    /// Start a new key→… cascade.
    ///
    /// - `force_hard` / non-adjacent Long-GOP jump → full [`Self::recreate`].
    /// - **Sequential** next sync (Play across GOP) or All-Intra → soft switch
    ///   when idle (mediabunny range pump feeds the next IDR without recreate).
    /// - Do **not** hard-recreate solely because `busy` when soft is OK — Play
    ///   warm-ahead often still has decodeQ when the next GOP key is needed;
    ///   recreate every GOP was ~1 underrun hold/GOP (~0.93× on 1080p).
    ///   Caller awaits idle first (`run_key_cascade`).
    pub(super) fn begin_key_cascade(
        &mut self,
        key_do: usize,
        force_hard: bool,
        sequential_next_key: bool,
    ) -> Result<(), MediaError> {
        // Pull any already-delivered outputs before deciding idle vs hard reset.
        let _ = self.collect_outputs_into_held();
        self.clear_held();
        for frame in self.frames.borrow_mut().drain(..) {
            drop(frame);
        }

        let pending_remap = !self.input_timestamps.is_empty();
        let busy = self.decode_queue_size() > 0 || pending_remap;
        let key_changed = matches!(self.key_order, Some(prev) if prev != key_do);
        let soft_ok = self.all_intra || sequential_next_key || !key_changed;
        // Soft paths must not recreate on busy alone (Play GOP seam + warm-ahead).
        if force_hard || (key_changed && !soft_ok) || (busy && !soft_ok) {
            self.recreate()?;
        } else {
            self.input_timestamps.clear();
            self.fed_through_order = None;
            self.key_order = None;
            self.packets_fed = 0;
            self.rasl_skipped = false;
        }
        self.key_order = Some(key_do);
        Ok(())
    }

    fn clear_held(&mut self) {
        self.held.clear();
    }

    /// mediabunny `onQueueDequeue` after consumer take / early sample close.
    fn wake_dequeue_waiters(&self) {
        let waiting = std::mem::take(&mut *self.dequeue_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
    }

    /// mediabunny `onQueueNotEmpty` — `held` gained a presentation-order frame.
    pub(super) fn wake_not_empty_waiters(&self) {
        let waiting = std::mem::take(&mut *self.not_empty_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
    }

    fn push_input_timestamp(&mut self, pts_us: i64) {
        let i = self.input_timestamps.partition_point(|&t| t < pts_us);
        self.input_timestamps.insert(i, pts_us);
    }

    pub(super) fn collect_outputs_into_held(&mut self) -> Result<(), MediaError> {
        let before = self.held.len();
        let pending: Vec<_> = self.frames.borrow_mut().drain(..).collect();
        for frame in pending {
            let Some(pts_us) = self.assign_output_pts(&frame) else {
                // Late output after flush/cancel/recreate race — drop; do not
                // poison the session (was: "decoded frame without queued input timestamp").
                drop(frame);
                continue;
            };
            self.held.push(HeldFrame { pts_us, frame });
        }
        if self.held.len() > before {
            self.wake_not_empty_waiters();
        }
        Ok(())
    }

    /// Next earliest fed stamp (FIFO). Ignores browser `VideoFrame.timestamp`.
    ///
    /// `mediabunny: src/media-sink.ts sampleHandler` non-WebKit —
    /// `inputTimestamps.shift()` then overwrite sample timestamp. Parks our fed
    /// µs on `held` so Chromium HW ±1 µs regenerations still match `take_held`.
    fn assign_output_pts(&mut self, _frame: &OwnedVideoFrame) -> Option<i64> {
        let pts = self.input_timestamps.first().copied()?;
        self.input_timestamps.remove(0);
        Some(pts)
    }

    pub(super) fn take_held(&mut self, target_us: i64) -> Option<OwnedVideoFrame> {
        let idx = self
            .held
            .iter()
            .position(|h| h.pts_us == target_us)
            .or_else(|| {
                // Belt for any path that parked Chrome's rounded PTS as-is.
                self.held
                    .iter()
                    .position(|h| (h.pts_us - target_us).abs() <= 1)
            })?;
        let frame = self.held.remove(idx).frame;
        // Consumer took a sample — unblock the packet pump (mediabunny
        // `onQueueDequeue`; the dual-async split is what makes this reachable).
        self.wake_dequeue_waiters();
        Some(frame)
    }

    /// Remap outputs and close anything strictly before the seek target in the
    /// same pass (mediabunny closes early samples in the output callback before
    /// they enter `sampleQueue`). Park-then-prune a turn later left `decodeQ`
    /// elevated with `held=0` mid key→N and starved the VideoFrame pool.
    ///
    /// Keep PTS `>= target` in `held` — playback/prefetch relies on warm ahead
    /// frames surviving `take_held(target)`. Dropping post-target forced a fresh
    /// key→N (or `flush/idle`) on every sequential frame (long-GOP play lag / skip).
    /// Seek still closes early frames; `optimizeForLatency: false` covers long
    /// long-GOP key→N without needing to discard ahead.
    ///
    /// When [`Self::prune_interim`] is set, pruned PTS `< target` go to the host
    /// dense cache instead of close — one cascade warms near-target cache.
    pub(super) fn collect_and_prune_held(&mut self, target_us: i64) -> Result<(), MediaError> {
        // Live cursor may sit ahead of a still-waiting consumer aim — never drop
        // that aim (Chrome race: prune@83333 wiped 41666 → held≈N-2 near=83333).
        let drop_before = match self.prune_floor_us.get() {
            Some(floor) => target_us.min(floor),
            None => target_us,
        };
        let before = self.held.len();
        let mut dropped = false;
        let pending: Vec<_> = self.frames.borrow_mut().drain(..).collect();
        for frame in pending {
            let Some(pts_us) = self.assign_output_pts(&frame) else {
                drop(frame);
                continue;
            };
            if pts_us < drop_before {
                self.deliver_or_drop_pruned(pts_us, frame);
                dropped = true;
            } else {
                self.held.push(HeldFrame { pts_us, frame });
            }
        }
        if self.held.len() > before {
            self.wake_not_empty_waiters();
        }
        // Drop any previously held frames that are now before the (floored) target.
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].pts_us < drop_before {
                let h = self.held.swap_remove(i);
                self.deliver_or_drop_pruned(h.pts_us, h.frame);
                dropped = true;
            } else {
                i += 1;
            }
        }
        if dropped {
            self.wake_dequeue_waiters();
        }
        Ok(())
    }

    fn deliver_or_drop_pruned(&self, pts_us: i64, frame: OwnedVideoFrame) {
        let mut slot = self.prune_interim.borrow_mut();
        if let Some(interim) = slot.as_mut() {
            let behind = interim.target_us.saturating_sub(pts_us);
            if behind <= INTERIM_SALVAGE_MAX_BEHIND_US
                && let Some(id) = pts_map_take(&mut interim.pts_to_id, pts_us)
            {
                (interim.on_frame)(id, frame);
                return;
            }
        }
        drop(frame);
    }

    /// Move remapped `held` frames to the consumer so `held` stays empty
    /// for backpressure (max queue 40 vs 8).
    pub(super) fn flush_held_into_gop(
        &mut self,
        on_frame: &mut dyn FnMut(FrameId, OwnedVideoFrame),
        pts_to_id: &mut PtsToFrameIds,
    ) {
        if self.held.is_empty() {
            return;
        }
        for h in self.held.drain(..) {
            if let Some(id) = pts_map_take(pts_to_id, h.pts_us) {
                on_frame(id, h.frame);
            } else {
                drop(h.frame);
            }
        }
        // Consumer drained sampleQueue — mediabunny `onQueueDequeue`.
        self.wake_dequeue_waiters();
    }

    pub(super) fn decode_queue_size(&self) -> u32 {
        self.decoder.decode_queue_size()
    }

    pub(super) fn raw_outputs_pending(&self) -> bool {
        !self.frames.borrow().is_empty()
    }

    pub(super) fn decode_encoded_sample(
        &mut self,
        sample: &EncodedSample,
        force_key: bool,
    ) -> Result<(), MediaError> {
        let hevc = self.config.codec.starts_with("hvc1") || self.config.codec.starts_with("hev1");
        // mediabunny: drop RASL after CRA / open-GOP key until past them.
        if hevc && self.packets_fed > 0 && !self.rasl_skipped {
            if let Some(len_size) = hevc_nal_length_size(&self.config.description)
                && has_hevc_rasl_picture(&sample.data, len_size)
            {
                // Advance pump cursor without decode / packets_fed bump.
                self.fed_through_order = Some(sample.decode_order);
                return Ok(());
            }
            self.rasl_skipped = true;
        }

        let chunk_type = if force_key || sample.is_keyframe {
            EncodedVideoChunkType::Key
        } else {
            EncodedVideoChunkType::Delta
        };
        let timestamp_us = ticks_to_micros(sample.composition_timestamp_ticks, sample.timescale);
        self.push_input_timestamp(timestamp_us);
        let timestamp_i32 = timestamp_us.clamp(i32::MIN as i64, i32::MAX as i64) as i32;

        // mediabunny: Chromium first-packet AVC AUD / HEVC NAL-order sanitize.
        let sanitized = if self.packets_fed == 0 && is_chromium() {
            if self.config.codec.starts_with("avc1") || self.config.codec.starts_with("avc3") {
                avc_nal_length_size(&self.config.description)
                    .and_then(|len_size| sanitize_avc_packet_for_chromium(&sample.data, len_size))
            } else if hevc {
                hevc_nal_length_size(&self.config.description)
                    .and_then(|len_size| sanitize_hevc_packet_for_chromium(&sample.data, len_size))
            } else {
                None
            }
        } else {
            None
        };
        if let Some(ref cleaned) = sanitized {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(
                &format!(
                    "fpmedia-web: chromium {} sanitize first packet {}→{} B",
                    if hevc { "HEVC" } else { "AVC" },
                    sample.data.len(),
                    cleaned.len()
                )
                .into(),
            );
        }
        let bytes = sanitized.as_deref().unwrap_or(sample.data.as_ref());

        // EncodedVideoChunk::new copies the buffer synchronously (mediabunny
        // keeps JS Uint8Array bytes). `view` avoids an extra WASM→JS copy of
        // the sample payload before that copy.
        let view = unsafe { Uint8Array::view(bytes) };
        let init = EncodedVideoChunkInit::new(&view, timestamp_i32, chunk_type);
        let duration_us = ticks_to_micros(sample.duration_ticks as i64, sample.timescale);
        if duration_us > 0 {
            init.set_duration(duration_us.clamp(0, u32::MAX as i64) as u32);
        }
        let chunk = EncodedVideoChunk::new(&init).map_err(MediaError::from)?;
        self.decoder.decode(&chunk).map_err(MediaError::from)?;
        self.packets_fed = self.packets_fed.saturating_add(1);
        self.fed_through_order = Some(sample.decode_order);
        self.check_error()
    }

    /// `flush()` raced with a timeout — a wedged Chromium decoder never settles.
    /// Returns the race promise so the caller can await **without** a live borrow.
    pub(super) fn flush_race_promise(&self, ms: i32) -> js_sys::Promise {
        let flush: js_sys::Promise = self.decoder.flush().unchecked_into();
        promise_race(flush, sleep_promise_ms(ms))
    }

    pub(super) fn check_error(&self) -> Result<(), MediaError> {
        if let Some(msg) = self.error.borrow_mut().take() {
            Err(MediaError::from_codec_message(msg))
        } else {
            Ok(())
        }
    }
}

impl Drop for DecoderCore {
    fn drop(&mut self) {
        self.detach_decoder();
        // Leak Closures — same reason as `recreate` (late JS invokes after close).
        let output = std::mem::replace(&mut self._on_output, Closure::new(|_| {}));
        let error = std::mem::replace(&mut self._on_error, Closure::new(|_| {}));
        let dequeue = std::mem::replace(&mut self._on_dequeue, Closure::new(|_| {}));
        std::mem::forget(output);
        std::mem::forget(error);
        std::mem::forget(dequeue);
    }
}
