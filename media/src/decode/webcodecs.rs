//! WebCodecs `VideoDecoder` wrapper.
//!
//! Playback follows mediabunny's model (`mediabunny: src/media-sink.ts`
//! `mediaSamplesInRange`): a **pump** task feeds packets in decode order while a
//! **consumer** task pulls the target frame out of the presentation buffer. The
//! two run concurrently, so `decodeQueueSize` backpressure unblocks the way
//! mediabunny expects — `onQueueDequeue` fires on a consumer take, not only on
//! decoder output. Prefer feeding further ahead in the GOP over `flush()`.
//!
//! All decoder state lives in [`DecoderCore`] behind `Rc<RefCell<…>>` so pump and
//! consumer can hold it alternately; never keep a borrow across `.await`,
//! `decoder.decode()`, or a `Closure` call (`.ai/skills/media.md`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::Uint8Array;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    EncodedVideoChunk, EncodedVideoChunkInit, EncodedVideoChunkType, VideoDecoder,
    VideoDecoderConfig, VideoDecoderInit, VideoFrame,
};

use crate::bitstream::{
    avc_nal_length_size, has_hevc_rasl_picture, hevc_nal_length_size,
    sanitize_avc_packet_for_chromium, sanitize_hevc_packet_for_chromium,
};
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::demux::Mp4VideoTrack;
use crate::error::MediaError;
use crate::types::{EncodedSample, FrameId, GopProfile, PlayPolicy, VideoMetadata};

/// WebCodecs is `SecureContext`-only. LAN `http://192.168.x.x` (phone → desktop
/// `cargo leptos watch`) leaves `VideoDecoder` undefined; calling
/// `isConfigSupported` then panics the open task and freezes status on
/// "creating decoder…".
fn ensure_webcodecs_available() -> Result<(), MediaError> {
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
const REORDER_FEED_SLACK: usize = 8;
/// Presentation-order frames to keep ahead of the current target in `held`
/// while the Play/prefetch pump runs (cross-GOP). Too small → underrun hold
/// every GOP on 1080p (~0.93×). Hd default = [`crate::PLAY_WARM_AHEAD_HD`]; live
/// value is [`DecoderCore::warm_ahead`] from [`PlayPolicy`].
#[allow(dead_code)]
const DECODE_WARM_AHEAD: usize = crate::PLAY_WARM_AHEAD_HD;
/// Only salvage pruned frames within this PTS gap of the seek target. Farther
/// frames are closed immediately — keeping every key→N VideoFrame alive exhausts
/// Chromium's pool and wedges at `decodeQ≈41, held=0` (long-GOP seek/settle).
/// ~8 frames @24 fps / ~10 @30 fps; densify still fills the rest after exact.
const INTERIM_SALVAGE_MAX_BEHIND_US: i64 = 333_333;
/// Pump safety valve: `decodeQueueSize == 0` means no `ondequeue` can ever fire,
/// so only a consumer take / prune can shrink `held`. With the dual-async split a
/// take is expected, but the consumer may be waiting for a frame that is still
/// unfed (deep B-pyramid) — feed anyway after this many idle turns instead of
/// deadlocking.
const PUMP_STARVED_SPINS: u32 = 240;
/// Hard backstop for the consumer pull loop when no clock is available.
const CONSUMER_MAX_SPINS: u32 = 20_000;
/// Consumer gives up when nothing observable changes for this long
/// (`held`, `decodeQueueSize`, raw outputs, fed packets, pump heartbeat).
const CONSUMER_STALL_MS: f64 = 5_000.0;

fn compute_max_queue_size(decoded_sample_queue_size: u32) -> u32 {
    // mediabunny `computeMaxQueueSize`
    if decoded_sample_queue_size == 0 {
        MAX_DECODE_QUEUE_EMPTY
    } else {
        MAX_DECODE_QUEUE_WITH_HELD
    }
}

#[derive(Clone)]
struct DecoderConfig {
    codec: String,
    width: u32,
    height: u32,
    description: Vec<u8>,
}

impl DecoderConfig {
    fn from_meta(meta: &VideoMetadata) -> Result<Self, MediaError> {
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
    /// Do **not** set `hardwareAcceleration: prefer-hardware` on the open
    /// support check — many browsers report `supported: false` even when
    /// software (or opportunistic HW) decode works, which blocks open.
    /// Prefer-hardware is applied only at configure when the play policy
    /// asked for it **and** a separate HW support probe passed
    /// (mediabunny: `decoderOptions.hardwareAcceleration`, PR #406).
    fn to_web_config(
        &self,
        optimize_for_latency: bool,
        prefer_hardware: bool,
    ) -> VideoDecoderConfig {
        let config = VideoDecoderConfig::new(&self.codec);
        config.set_coded_width(self.width);
        config.set_coded_height(self.height);
        config.set_description(&Uint8Array::from(self.description.as_slice()));
        config.set_optimize_for_latency(optimize_for_latency);
        if prefer_hardware {
            config.set_hardware_acceleration(web_sys::HardwareAcceleration::PreferHardware);
        }
        config
    }
}

struct HeldFrame {
    pts_us: i64,
    frame: OwnedVideoFrame,
}

/// GOP densify drain target: PTS→id map plus the consumer callback.
type GopSink<'a> = (
    &'a mut HashMap<i64, FrameId>,
    &'a mut dyn FnMut(FrameId, OwnedVideoFrame),
);

/// Hands pruned PTS `< target` frames to the host instead of closing them.
///
/// Settle/seek installs this so one key→N cascade both yields exact **and** warms
/// dense near the target (densify after exact is cheaper). Host must **not** blit —
/// only ingest into the scrub store (blit looked like play from GOP start).
///
/// Only frames within [`INTERIM_SALVAGE_MAX_BEHIND_US`] of `target_us` are kept;
/// farther ones are closed so the VideoFrame pool stays drained (mediabunny closes
/// early samples in the output callback).
struct PruneInterim {
    pts_to_id: HashMap<i64, FrameId>,
    target_us: i64,
    on_frame: Box<dyn FnMut(FrameId, OwnedVideoFrame)>,
}

/// Owned decoder state shared by the packet pump and the frame consumer.
///
/// Every method here is synchronous: awaits happen in the free async helpers
/// below, which take `&Rc<RefCell<DecoderCore>>` and re-borrow per turn.
struct DecoderCore {
    decoder: VideoDecoder,
    config: DecoderConfig,
    optimize_for_latency: bool,
    /// `prefer-hardware` on configure (Uhd main only after HW support probe).
    prefer_hardware: bool,
    /// Pump warm frames past consumer target ([`PlayPolicy::decode_warm_ahead`]).
    warm_ahead: usize,
    /// Bumped on teardown so late browser callbacks become no-ops.
    generation: Rc<Cell<u64>>,
    /// Bumped on detach/reset/invalidate/Drop so in-flight pumps stop feeding.
    pump_epoch: Rc<Cell<u64>>,
    frames: Rc<RefCell<Vec<OwnedVideoFrame>>>,
    error: Rc<RefCell<Option<String>>>,
    /// Resolvers waiting for the next `output` callback (mediabunny-style).
    output_waiters: Rc<RefCell<Vec<js_sys::Function>>>,
    /// Resolvers waiting for `dequeue` (decodeQueueSize decreased).
    dequeue_waiters: Rc<RefCell<Vec<js_sys::Function>>>,
    /// Resolvers waiting for `held` to gain a remapped frame
    /// (mediabunny `onQueueNotEmpty`) — the pump can fill `held` while the
    /// consumer waits, and only this wakes it.
    not_empty_waiters: Rc<RefCell<Vec<js_sys::Function>>>,
    fed_through_order: Option<usize>,
    key_order: Option<usize>,
    /// Sorted PTS (µs) of fed chunks — shifted onto outputs in presentation order.
    input_timestamps: Vec<i64>,
    held: Vec<HeldFrame>,
    /// Packets fed since last `configure` (mediabunny `currentPacketIndex`).
    packets_fed: usize,
    /// HEVC: finished dropping post-CRA RASL (mediabunny `raslSkipped`).
    rasl_skipped: bool,
    /// When true, every sample is a sync — soft key switch is OK (no long cascade).
    all_intra: bool,
    /// Optional: salvage pruned early frames during key→N into the host dense cache.
    prune_interim: RefCell<Option<PruneInterim>>,
    _on_output: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
    _on_dequeue: Closure<dyn FnMut(JsValue)>,
}

impl DecoderCore {
    fn from_config(
        config: DecoderConfig,
        optimize_for_latency: bool,
        prefer_hardware: bool,
        warm_ahead: usize,
        pump_epoch: Rc<Cell<u64>>,
        all_intra: bool,
    ) -> Result<Self, MediaError> {
        let web_config = config.to_web_config(optimize_for_latency, prefer_hardware);

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
        decoder.configure(&web_config).map_err(MediaError::from)?;

        Ok(Self {
            decoder,
            config,
            optimize_for_latency,
            prefer_hardware,
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
            _on_output: on_output,
            _on_error: on_error,
            _on_dequeue: on_dequeue,
        })
    }

    /// Close the browser decoder and retire callbacks without dropping Closures
    /// that JS may still invoke (would panic inside wasm-bindgen).
    fn detach_decoder(&mut self) {
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

    fn recreate(&mut self) -> Result<(), MediaError> {
        // Keep dense-salvage sink across hard reset (All-Intra / wedge recovery).
        let interim = self.prune_interim.borrow_mut().take();
        let all_intra = self.all_intra;
        self.detach_decoder();
        let config = self.config.clone();
        let optimize_for_latency = self.optimize_for_latency;
        let prefer_hardware = self.prefer_hardware;
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
            prefer_hardware,
            warm_ahead,
            pump_epoch,
            all_intra,
        )?;
        *self.prune_interim.borrow_mut() = interim;
        Ok(())
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
    fn begin_key_cascade(
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
    fn wake_not_empty_waiters(&self) {
        let waiting = std::mem::take(&mut *self.not_empty_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
    }

    fn push_input_timestamp(&mut self, pts_us: i64) {
        let i = self.input_timestamps.partition_point(|&t| t < pts_us);
        self.input_timestamps.insert(i, pts_us);
    }

    fn collect_outputs_into_held(&mut self) -> Result<(), MediaError> {
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

    /// PTS for a decoded frame: prefer the chunk timestamp retained on the
    /// `VideoFrame` when it matches a fed stamp; otherwise FIFO (mediabunny
    /// non-WebKit). Do not drop the FIFO head on an unmatched chunk PTS.
    fn assign_output_pts(&mut self, frame: &OwnedVideoFrame) -> Option<i64> {
        let chunk_pts = {
            let ts = frame.as_web_sys().timestamp();
            if ts.is_finite() {
                Some(ts.round() as i64)
            } else {
                None
            }
        };
        if let Some(pts) = chunk_pts
            && let Some(i) = self.input_timestamps.iter().position(|&t| t == pts)
        {
            self.input_timestamps.remove(i);
            return Some(pts);
        }
        let pts = self.input_timestamps.first().copied()?;
        self.input_timestamps.remove(0);
        Some(pts)
    }

    fn take_held(&mut self, target_us: i64) -> Option<OwnedVideoFrame> {
        let idx = self.held.iter().position(|h| h.pts_us == target_us)?;
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
    fn collect_and_prune_held(&mut self, target_us: i64) -> Result<(), MediaError> {
        let before = self.held.len();
        let mut dropped = false;
        let pending: Vec<_> = self.frames.borrow_mut().drain(..).collect();
        for frame in pending {
            let Some(pts_us) = self.assign_output_pts(&frame) else {
                drop(frame);
                continue;
            };
            if pts_us < target_us {
                self.deliver_or_drop_pruned(pts_us, frame);
                dropped = true;
            } else {
                self.held.push(HeldFrame { pts_us, frame });
            }
        }
        if self.held.len() > before {
            self.wake_not_empty_waiters();
        }
        // Drop any previously held frames that are now before the (new) target.
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].pts_us < target_us {
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
                && let Some(id) = interim.pts_to_id.remove(&pts_us)
            {
                (interim.on_frame)(id, frame);
                return;
            }
        }
        drop(frame);
    }

    /// Move remapped `held` frames to the consumer so `held` stays empty
    /// for backpressure (max queue 40 vs 8).
    fn flush_held_into_gop(
        &mut self,
        on_frame: &mut dyn FnMut(FrameId, OwnedVideoFrame),
        pts_to_id: &mut HashMap<i64, FrameId>,
    ) {
        if self.held.is_empty() {
            return;
        }
        for h in self.held.drain(..) {
            if let Some(id) = pts_to_id.remove(&h.pts_us) {
                on_frame(id, h.frame);
            } else {
                drop(h.frame);
            }
        }
        // Consumer drained sampleQueue — mediabunny `onQueueDequeue`.
        self.wake_dequeue_waiters();
    }

    fn decode_queue_size(&self) -> u32 {
        self.decoder.decode_queue_size()
    }

    fn raw_outputs_pending(&self) -> bool {
        !self.frames.borrow().is_empty()
    }

    fn decode_encoded_sample(
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
                    "webmedia: chromium {} sanitize first packet {}→{} B",
                    if hevc { "HEVC" } else { "AVC" },
                    sample.data.len(),
                    cleaned.len()
                )
                .into(),
            );
        }
        let bytes = sanitized.as_deref().unwrap_or(sample.data.as_slice());

        let init = EncodedVideoChunkInit::new(&Uint8Array::from(bytes), timestamp_i32, chunk_type);
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
    fn flush_race_promise(&self, ms: i32) -> js_sys::Promise {
        let flush: js_sys::Promise = self.decoder.flush().unchecked_into();
        promise_race(flush, sleep_promise_ms(ms))
    }

    fn check_error(&self) -> Result<(), MediaError> {
        if let Some(msg) = self.error.borrow_mut().take() {
            Err(MediaError::WebCodecs(msg))
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

/// Decode presentation frames via WebCodecs with a warm decode-order pump.
///
/// Thin shell over [`DecoderCore`]: the spawned packet pump keeps its own `Rc`
/// to the core plus `pump_epoch`, so a reset/detach/Drop here stops it even
/// though it outlives this value's borrow.
pub struct WebCodecsDecoder {
    core: Rc<RefCell<DecoderCore>>,
    pump_epoch: Rc<Cell<u64>>,
}

impl WebCodecsDecoder {
    /// Main play/seek decoder — knobs from [`VideoMetadata::play_policy`].
    pub async fn create(meta: &VideoMetadata) -> Result<Self, MediaError> {
        Self::create_with_policy(meta, meta.play_policy()).await
    }

    /// Scrub densify decoder — always [`PlayPolicy::hd`] knobs (no prefer-hardware)
    /// so Uhd play can keep the sole HW session (mediabunny dual-HW stall).
    pub async fn create_scrub(meta: &VideoMetadata) -> Result<Self, MediaError> {
        Self::create_with_policy(meta, PlayPolicy::hd()).await
    }

    async fn create_with_policy(
        meta: &VideoMetadata,
        policy: PlayPolicy,
    ) -> Result<Self, MediaError> {
        ensure_webcodecs_available()?;
        let config = DecoderConfig::from_meta(meta)?;
        // Prefer throughput configure — `optimizeForLatency: true` wedges Chromium
        // on sustained play/deep key→N (`backpressure q≈41` / `flush/idle`).
        // mediabunny leaves this optional; we set false for VOD scrub+play.
        let optimize_for_latency = false;
        // Open gate: never require prefer-hardware (FHD-safe).
        let probe = config.to_web_config(optimize_for_latency, false);
        web_sys::console::log_1(
            &format!(
                "webmedia: VideoDecoder isConfigSupported ({}, {}x{}, play={})…",
                config.codec,
                config.width,
                config.height,
                policy.label()
            )
            .into(),
        );
        let support = JsFuture::from(VideoDecoder::is_config_supported(&probe))
            .await
            .map_err(MediaError::from)?;
        let supported = js_sys::Reflect::get(&support, &"supported".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !supported {
            return Err(MediaError::CodecUnsupported(config.codec.clone()));
        }

        let mut prefer_hardware = false;
        if policy.prefer_hardware {
            let hw_probe = config.to_web_config(optimize_for_latency, true);
            let hw_support = JsFuture::from(VideoDecoder::is_config_supported(&hw_probe))
                .await
                .map_err(MediaError::from)?;
            prefer_hardware = js_sys::Reflect::get(&hw_support, &"supported".into())
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if prefer_hardware {
                web_sys::console::log_1(&"webmedia: VideoDecoder prefer-hardware ok".into());
            } else {
                web_sys::console::warn_1(
                    &"webmedia: prefer-hardware unsupported; software configure".into(),
                );
            }
        }

        let pump_epoch = Rc::new(Cell::new(0));
        let all_intra = matches!(meta.gop_profile, GopProfile::AllIntra);
        let core = DecoderCore::from_config(
            config,
            optimize_for_latency,
            prefer_hardware,
            policy.decode_warm_ahead,
            Rc::clone(&pump_epoch),
            all_intra,
        )?;
        web_sys::console::log_1(
            &format!(
                "webmedia: VideoDecoder configured (warm={}, hw={prefer_hardware})",
                policy.decode_warm_ahead
            )
            .into(),
        );
        Ok(Self {
            core: Rc::new(RefCell::new(core)),
            pump_epoch,
        })
    }

    pub fn invalidate(&mut self) {
        // Prefer a hard decoder reset — soft clear of timestamps/held while the
        // browser still has in-flight chunks desyncs PTS remap.
        let _ = self.reset();
    }

    /// Close and recreate the VideoDecoder; clear warm GOP / held / timestamps.
    /// Stops any in-flight pump (`pump_epoch` bump inside `detach_decoder`).
    pub fn reset(&mut self) -> Result<(), MediaError> {
        self.core.borrow_mut().recreate()
    }

    /// In-flight encoded chunks waiting in the browser decoder.
    pub fn browser_decode_queue_size(&self) -> u32 {
        self.core.borrow().decode_queue_size()
    }

    /// Decoded frames parked for presentation-order pull (B-frame reorder).
    pub fn held_len(&self) -> usize {
        self.core.borrow().held.len()
    }

    /// Decode until `frame` is available, feeding decode-order packets as needed.
    ///
    /// `is_cancelled` — host op epoch (rapid PREV/seek); bail with [`MediaError::Cancelled`].
    pub async fn decode_frame(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<OwnedVideoFrame, MediaError> {
        self.decode_frame_inner(track, frame, is_cancelled).await
    }

    /// Like [`Self::decode_frame`], but salvages pruned PTS `< target` frames to
    /// `on_interim` (dense insert only — do not blit). Avoids a second full-GOP
    /// densify pass after long-GOP settle/seek.
    pub async fn decode_frame_with_interim(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
        on_interim: Box<dyn FnMut(FrameId, OwnedVideoFrame)>,
    ) -> Result<OwnedVideoFrame, MediaError> {
        let key_id = track.ensure_key_verified(frame).await?;
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let target_us = {
            let index = track.index();
            let entry = index
                .get(frame)
                .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
            ticks_to_micros(entry.composition_timestamp_ticks, entry.timescale)
        };
        let gop_ids = track.index().gop_frame_ids(key_id).unwrap_or_default();
        let mut pts_to_id: HashMap<i64, FrameId> = HashMap::with_capacity(gop_ids.len());
        {
            let index = track.index();
            for id in &gop_ids {
                let e = index
                    .get(*id)
                    .ok_or(MediaError::FrameOutOfRange(*id, track.frame_count()))?;
                pts_to_id.insert(
                    ticks_to_micros(e.composition_timestamp_ticks, e.timescale),
                    *id,
                );
            }
        }
        *self.core.borrow().prune_interim.borrow_mut() = Some(PruneInterim {
            pts_to_id,
            target_us,
            on_frame: on_interim,
        });
        let result = self.decode_frame_inner(track, frame, is_cancelled).await;
        *self.core.borrow().prune_interim.borrow_mut() = None;
        result
    }

    async fn decode_frame_inner(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<OwnedVideoFrame, MediaError> {
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let core = Rc::clone(&self.core);
        // On-demand bitstream verify (deferred open). mediabunny: verifyKeyPackets on retrieve.
        let key_id = track.ensure_key_verified(frame).await?;
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let (target_us, target_do, key_do) = {
            let index = track.index();
            let entry = index
                .get(frame)
                .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
            let key_do = index
                .get(key_id)
                .map(|e| e.decode_order)
                .ok_or(MediaError::FrameOutOfRange(key_id, track.frame_count()))?;
            (
                ticks_to_micros(entry.composition_timestamp_ticks, entry.timescale),
                entry.decode_order,
                key_do,
            )
        };

        {
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                let ahead = c.held.iter().filter(|h| h.pts_us > target_us).count();
                let want = c.warm_ahead;
                drop(c);
                // Hit in held — still keep a cross-GOP pump warm so Play does not
                // drain the buffer dry between takes (was: return with no feeder).
                if ahead < want {
                    self.spawn_warm_pump(Rc::clone(track), key_do, target_do, target_us);
                }
                return Ok(ready);
            }
        }

        // Warm feed-ahead may have fed past this target while outputs are still
        // in flight. Poll briefly; if the PTS never appears, hard-recreate from
        // the key (do not flush — that demands a key on the next delta and still
        // misses when Chromium has wedged mid-GOP).
        let fed_order = core.borrow().fed_through_order;
        if fed_order.is_some_and(|fed| target_do <= fed) {
            for _ in 0..24 {
                if is_cancelled() {
                    return Err(MediaError::Cancelled);
                }
                {
                    let mut c = core.borrow_mut();
                    c.collect_and_prune_held(target_us)?;
                    if let Some(ready) = c.take_held(target_us) {
                        return Ok(ready);
                    }
                    let idle = c.decode_queue_size() == 0
                        && !c.raw_outputs_pending()
                        && c.input_timestamps.is_empty();
                    if idle {
                        break;
                    }
                }
                await_outputs_if_pending(&core).await?;
                wait_for_frame_progress(&core).await;
            }
            if !holds_target(&core, target_us)
                && fed_order.is_some_and(|fed| fed.saturating_sub(target_do) >= REORDER_FEED_SLACK)
            {
                self.run_key_cascade(
                    track,
                    frame,
                    key_id,
                    key_do,
                    target_us,
                    true,
                    false,
                    is_cancelled,
                )
                .await?;
                let mut c = core.borrow_mut();
                c.collect_and_prune_held(target_us)?;
                if let Some(ready) = c.take_held(target_us) {
                    return Ok(ready);
                }
            }
        }

        // Restart when the GOP changes, or when the target was already emitted and
        // discarded (e.g. presentation queue cleared after prefetch) — WebCodecs
        // cannot re-emit a consumed frame without cascading from the key again.
        let (needs_cascade, target_consumed, sequential_next) = {
            let c = core.borrow();
            let consumed = c
                .fed_through_order
                .is_some_and(|fed| target_do <= fed && !c.input_timestamps.contains(&target_us));
            let sequential = c
                .key_order
                .is_some_and(|prev| is_sequential_next_key(track, prev, key_do));
            (
                c.key_order != Some(key_do) || consumed,
                consumed,
                sequential,
            )
        };
        if needs_cascade {
            self.run_key_cascade(
                track,
                frame,
                key_id,
                key_do,
                target_us,
                target_consumed,
                sequential_next,
                is_cancelled,
            )
            .await?;
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                return Ok(ready);
            }
        }

        // Dual async (mediabunny: src/media-sink.ts `mediaSamplesInRange`): the
        // pump feeds decode-order packets in its own task while this task pulls
        // the target from `held`. Backpressure can now unblock on a consumer
        // take, which is what lets the pump wait at `decodeQueueSize == 0`.
        //
        // Bump epoch first so a prior background warm-fill / stray pump stops
        // before this range starts (only one feeder on the decoder).
        self.pump_epoch.set(self.pump_epoch.get().wrapping_add(1));
        let pump_epoch_now = self.pump_epoch.get();
        let signals = Rc::new(PumpSignals::default());
        let stop_on_return = PumpStopGuard(Rc::clone(&signals));
        spawn_local(
            FramePump {
                core: Rc::clone(&core),
                pump_epoch: Rc::clone(&self.pump_epoch),
                epoch: pump_epoch_now,
                track: Rc::clone(track),
                key_do,
                target_do,
                target_us,
                signals: Rc::clone(&signals),
                warm_only: false,
            }
            .run(),
        );

        let mut spins = 0u32;
        let mut last_sig = progress_signature(&core, &signals);
        let mut last_change_ms = now_ms();
        loop {
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            // Bind out of the RefCell first — no guard across the body.
            let pump_error = signals.error.borrow_mut().take();
            if let Some(err) = pump_error {
                return Err(err);
            }
            let ready = {
                let mut c = core.borrow_mut();
                c.check_error()?;
                c.collect_and_prune_held(target_us)?;
                c.take_held(target_us)
            };
            if let Some(ready) = ready {
                // Return the frame immediately so presentation can present /
                // clear underrun. Keep the range pump alive in the background
                // until warm-ahead is parked — immediate `stop` on take raced
                // backpressure and forced a cold pump every 1–2 frames
                // (long-GOP ~0.75×). mediabunny keeps decoding while the consumer
                // pulls (`src/media-sink.ts` mediaSamplesInRange).
                let core_bg = Rc::clone(&core);
                let signals_bg = Rc::clone(&signals);
                let pump_epoch_bg = Rc::clone(&self.pump_epoch);
                spawn_local(async move {
                    let want_ahead = core_bg.borrow().warm_ahead;
                    for _ in 0..80 {
                        if pump_epoch_bg.get() != pump_epoch_now || signals_bg.stop.get() {
                            return;
                        }
                        let can_stop = {
                            let mut c = core_bg.borrow_mut();
                            if c.collect_and_prune_held(target_us).is_err() {
                                signals_bg.stop.set(true);
                                return;
                            }
                            let ahead = c.held.iter().filter(|h| h.pts_us > target_us).count();
                            let idle = c.decode_queue_size() == 0
                                && !c.raw_outputs_pending()
                                && c.input_timestamps.is_empty();
                            ahead >= want_ahead || (signals_bg.done.get() && idle)
                        };
                        if can_stop {
                            break;
                        }
                        wait_for_frame_progress(&core_bg).await;
                    }
                    if pump_epoch_bg.get() != pump_epoch_now {
                        return;
                    }
                    signals_bg.stop.set(true);
                    for _ in 0..12 {
                        if pump_epoch_bg.get() != pump_epoch_now {
                            return;
                        }
                        if await_outputs_if_pending(&core_bg).await.is_err() {
                            return;
                        }
                        let idle = {
                            let mut c = core_bg.borrow_mut();
                            let _ = c.collect_and_prune_held(target_us);
                            c.decode_queue_size() == 0 && !c.raw_outputs_pending()
                        };
                        if idle {
                            break;
                        }
                        wait_for_frame_progress(&core_bg).await;
                    }
                });
                // Background owns stop — do not kill the pump on return.
                std::mem::forget(stop_on_return);
                return Ok(ready);
            }
            if signals.done.get() {
                // No more packets are coming — drain, then flush once.
                if let Some(ready) = finish_gop_for_target(&core, target_us, is_cancelled).await? {
                    return Ok(ready);
                }
                return Err(MediaError::WebCodecs(format!(
                    "flush/idle without pts={target_us}µs (held={}, q={}, fed={:?}, stamps={})",
                    core.borrow().held.len(),
                    core.borrow().decode_queue_size(),
                    core.borrow().fed_through_order,
                    core.borrow().input_timestamps.len(),
                )));
            }

            let sig = progress_signature(&core, &signals);
            if sig != last_sig {
                last_sig = sig;
                last_change_ms = now_ms();
            } else if now_ms() - last_change_ms > CONSUMER_STALL_MS {
                return Err(MediaError::WebCodecs(format!(
                    "decode pump stalled waiting for pts={target_us}µs (q={}, held={})",
                    core.borrow().decode_queue_size(),
                    core.borrow().held.len()
                )));
            }
            spins += 1;
            if spins > CONSUMER_MAX_SPINS {
                return Err(MediaError::WebCodecs(format!(
                    "decode pump gave up waiting for pts={target_us}µs"
                )));
            }
            wait_for_frame_progress(&core).await;
        }
    }

    /// Feed the GOP key (plus the All-Intra coax / hard-retry cascade) so the
    /// pump can start from a configured decoder.
    ///
    /// Single-task on purpose: the browser needs the key chunk before any
    /// further packet is useful, and the retry ladder recreates the decoder.
    ///
    /// `sequential_next_key`: Play advancing into the adjacent GOP — soft switch
    /// when idle (no recreate). Scrub jumps keep this false.
    #[allow(clippy::too_many_arguments)]
    async fn run_key_cascade(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        frame: FrameId,
        key_id: FrameId,
        key_do: usize,
        target_us: i64,
        force_hard: bool,
        sequential_next_key: bool,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(), MediaError> {
        let core = Rc::clone(&self.core);
        // Soft Play seam: wait out warm-ahead decodeQ before soft switch so we
        // never hard-recreate on busy (`begin_key_cascade`). Also covers the
        // case where `finish_gop` cleared `key_order` (sequential flag false but
        // soft via !key_changed). mediabunny: `src/media-sink.ts`
        // mediaSamplesInRange keeps the same decoder across adjacent IDRs.
        if !force_hard {
            let soft_wait = {
                let c = core.borrow();
                let key_changed = matches!(c.key_order, Some(prev) if prev != key_do);
                c.all_intra || sequential_next_key || !key_changed
            };
            if soft_wait {
                for _ in 0..64 {
                    if is_cancelled() {
                        return Err(MediaError::Cancelled);
                    }
                    {
                        let mut c = core.borrow_mut();
                        c.check_error()?;
                        let _ = c.collect_and_prune_held(target_us);
                        // Already have the next-GOP target from a prior pump — done.
                        if c.held.iter().any(|h| h.pts_us == target_us) {
                            c.key_order = Some(key_do);
                            return Ok(());
                        }
                        let idle = c.decode_queue_size() == 0
                            && !c.raw_outputs_pending()
                            && c.input_timestamps.is_empty();
                        if idle {
                            break;
                        }
                    }
                    await_outputs_if_pending(&core).await?;
                    wait_for_frame_progress(&core).await;
                }
            }
        }
        // Soft key switch when idle — All-Intra / sequential Play GOP boundary.
        // Hard recreate only if force_hard or non-adjacent Long-GOP jump.
        core.borrow_mut()
            .begin_key_cascade(key_do, force_hard, sequential_next_key)?;
        // Soft clear wiped held — if we already fed this key in a prior range,
        // still must re-feed after soft wipe (fed_through cleared).
        let key_sample = track.encoded_sample(key_id).await?;
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        feed_sample(&core, &key_sample, true, target_us, is_cancelled).await?;
        // Poll with macrotasks (see `poll_output_or_macrotask`) — microtask-only
        // never saw HW output on long-GOP and forced flush.
        if poll_for_target(&core, target_us, 32, is_cancelled).await? {
            return Ok(());
        }
        if key_id != frame {
            return Ok(());
        }

        // Prefer feeding one more GOP packet over flush — flush forces the
        // next decode() to be a key (Chromium: "needs a key chunk").
        // All-Intra: GOP is one sample — coax is empty; hard-retry instead of
        // a multi-second flush that pins the decoder slot (→ `decoder busy`).
        let coax = track.samples_after_in_gop(key_do, key_do, 1).await?;
        if let Some(sample) = coax.first() {
            feed_sample(&core, sample, false, target_us, is_cancelled).await?;
            if poll_for_target(&core, target_us, 32, is_cancelled).await? {
                return Ok(());
            }
        } else if !holds_target(&core, target_us) {
            // Sole sync (All-Intra): one hard reset + re-feed, no long flush.
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            core.borrow_mut().recreate()?;
            core.borrow_mut().key_order = Some(key_do);
            let key_sample = track.encoded_sample(key_id).await?;
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            feed_sample(&core, &key_sample, true, target_us, is_cancelled).await?;
            if poll_for_target(&core, target_us, 48, is_cancelled).await? {
                return Ok(());
            }
        }
        if !holds_target(&core, target_us) {
            // Last resort — short flush (not 2s) then clear warm state.
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            let _ = flush_with_timeout_ms(&core, 250).await;
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            c.fed_through_order = None;
            c.key_order = None;
            c.input_timestamps.clear();
        }
        Ok(())
    }

    /// Background packet pump to refill `held` after a warm take (cross-GOP).
    ///
    /// Early `take_held` used to return with no feeder — Play then drained held
    /// and underran at the next GOP. Bumps `pump_epoch` so only one feeder runs.
    fn spawn_warm_pump(
        &self,
        track: Rc<Mp4VideoTrack>,
        key_do: usize,
        target_do: usize,
        target_us: i64,
    ) {
        self.pump_epoch.set(self.pump_epoch.get().wrapping_add(1));
        let pump_epoch_now = self.pump_epoch.get();
        let signals = Rc::new(PumpSignals::default());
        spawn_local(
            FramePump {
                core: Rc::clone(&self.core),
                pump_epoch: Rc::clone(&self.pump_epoch),
                epoch: pump_epoch_now,
                track,
                key_do,
                target_do,
                target_us,
                signals,
                warm_only: true,
            }
            .run(),
        );
    }

    /// Decode every presentation frame in the GOP starting at sync `key_frame`.
    ///
    /// Delivers remapped frames to `on_frame` as they emerge so `held` stays
    /// empty (mediabunny drains `sampleQueue` as the consumer pulls) and the
    /// dense-cache strip can paint each tick without waiting for the whole GOP.
    /// Parking the whole GOP in `held` forced `MAX_DECODE_QUEUE_WITH_HELD` (8)
    /// and wedged at `q≈9, held=16` → backpressure timeout.
    ///
    /// Stays a **single** task: `flush_held_into_gop` drains `held` on every
    /// backpressure turn, so the consumer-take wake the dual-async
    /// [`Self::decode_frame`] path needs has no equivalent here.
    ///
    /// On cancel: hard-reset. Frames already delivered to `on_frame` stay with
    /// the consumer (partial densify is OK; holes re-arm fill).
    pub async fn decode_gop(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        key_frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
        on_frame: &mut dyn FnMut(FrameId, OwnedVideoFrame),
    ) -> Result<(), MediaError> {
        if is_cancelled() {
            return self.abort_gop_partial();
        }
        let core = Rc::clone(&self.core);
        // Densify may start before bg verify finishes — verify this key on demand.
        let key_frame = track.ensure_key_verified(key_frame).await?;
        if is_cancelled() {
            return self.abort_gop_partial();
        }
        let (key_do, gop_ids) = {
            let index = track.index();
            let key_entry = index
                .get(key_frame)
                .ok_or(MediaError::FrameOutOfRange(key_frame, track.frame_count()))?;
            if !key_entry.is_sync {
                return Err(MediaError::WebCodecs(format!(
                    "decode_gop requires sync key (frame={key_frame})"
                )));
            }
            let key_do = key_entry.decode_order;
            let gop_ids = index
                .gop_frame_ids(key_frame)
                .ok_or(MediaError::FrameOutOfRange(key_frame, track.frame_count()))?;
            (key_do, gop_ids)
        };
        let mut pts_to_id: HashMap<i64, FrameId> = HashMap::with_capacity(gop_ids.len());
        {
            let index = track.index();
            for id in &gop_ids {
                let e = index
                    .get(*id)
                    .ok_or(MediaError::FrameOutOfRange(*id, track.frame_count()))?;
                pts_to_id.insert(
                    ticks_to_micros(e.composition_timestamp_ticks, e.timescale),
                    *id,
                );
            }
        }

        core.borrow_mut().begin_key_cascade(key_do, false, false)?;

        let key_sample = track.encoded_sample(key_frame).await?;
        if is_cancelled() {
            return self.abort_gop_partial();
        }
        match feed_sample_gop(
            &core,
            &key_sample,
            true,
            &mut pts_to_id,
            on_frame,
            is_cancelled,
        )
        .await
        {
            Ok(()) => {}
            Err(MediaError::Cancelled) => return self.abort_gop_partial(),
            Err(e) => return Err(e),
        }
        await_outputs_if_pending(&core).await?;
        {
            let mut c = core.borrow_mut();
            c.collect_outputs_into_held()?;
            c.flush_held_into_gop(on_frame, &mut pts_to_id);
        }
        // Macrotask so paused rAF can refresh the ready strip mid-GOP.
        let _ = JsFuture::from(sleep_promise_ms(0)).await;

        let max_feeds = gop_ids.len().saturating_add(8).max(16);
        for _ in 0..max_feeds {
            if is_cancelled() {
                return self.abort_gop_partial();
            }
            let fed = core.borrow().fed_through_order.unwrap_or(key_do);
            let next = track.samples_after_in_gop(key_do, fed, 1).await?;
            if next.is_empty() {
                break;
            }
            match feed_sample_gop(
                &core,
                &next[0],
                false,
                &mut pts_to_id,
                on_frame,
                is_cancelled,
            )
            .await
            {
                Ok(()) => {}
                Err(MediaError::Cancelled) => return self.abort_gop_partial(),
                Err(e) => return Err(e),
            }
            await_outputs_if_pending(&core).await?;
            {
                let mut c = core.borrow_mut();
                c.collect_outputs_into_held()?;
                c.flush_held_into_gop(on_frame, &mut pts_to_id);
            }
            // Macrotask yield — microtask-only spins starve rAF / strip updates.
            let _ = JsFuture::from(sleep_promise_ms(0)).await;
        }

        for _ in 0..32 {
            if is_cancelled() {
                return self.abort_gop_partial();
            }
            await_outputs_if_pending(&core).await?;
            let idle = {
                let mut c = core.borrow_mut();
                c.collect_outputs_into_held()?;
                c.flush_held_into_gop(on_frame, &mut pts_to_id);
                c.decode_queue_size() == 0 && !c.raw_outputs_pending()
            };
            if idle {
                break;
            }
            let _ = JsFuture::from(sleep_promise_ms(0)).await;
        }
        let pending = {
            let c = core.borrow();
            !c.input_timestamps.is_empty() || c.decode_queue_size() > 0
        };
        if pending {
            // Settle abort must not wait out a 2s flush while exact owns HW.
            if is_cancelled() {
                return self.abort_gop_partial();
            }
            let _ = flush_with_timeout(&core).await;
            let mut c = core.borrow_mut();
            c.collect_outputs_into_held()?;
            c.flush_held_into_gop(on_frame, &mut pts_to_id);
        }

        let mut c = core.borrow_mut();
        c.fed_through_order = None;
        c.key_order = None;
        c.input_timestamps.clear();
        Ok(())
    }

    /// Cancel path: hard-reset so in-flight browser outputs cannot desync PTS remap.
    fn abort_gop_partial(&mut self) -> Result<(), MediaError> {
        let _ = self.reset();
        Err(MediaError::Cancelled)
    }
}

impl Drop for WebCodecsDecoder {
    fn drop(&mut self) {
        // Stop any pump that still holds an `Rc` to the core, then tear the
        // browser decoder down even if that pump keeps the core alive a turn.
        self.pump_epoch.set(self.pump_epoch.get().wrapping_add(1));
        if let Ok(mut core) = self.core.try_borrow_mut() {
            core.detach_decoder();
        }
    }
}

/// Cross-task flags between the packet pump and the frame consumer
/// (mediabunny keeps these as closure-scoped `promiseWithResolvers` pairs).
#[derive(Default)]
struct PumpSignals {
    /// Consumer finished (frame, cancel, or error) — pump must stop feeding.
    stop: Cell<bool>,
    /// Pump has no more packets to feed (GOP end / reorder slack / error).
    done: Cell<bool>,
    /// Incremented per fed packet so the consumer can tell "slow" from "stuck".
    beats: Cell<u64>,
    error: RefCell<Option<MediaError>>,
}

/// Stops the pump on every consumer exit path (`?`, cancel, success).
struct PumpStopGuard(Rc<PumpSignals>);

impl Drop for PumpStopGuard {
    fn drop(&mut self) {
        self.0.stop.set(true);
    }
}

/// Packet pump for [`WebCodecsDecoder::decode_frame`] (mediabunny:
/// `src/media-sink.ts` — the `(async () => { … })()` that keeps decoding packets
/// while the iterator consumer pulls samples).
struct FramePump {
    core: Rc<RefCell<DecoderCore>>,
    pump_epoch: Rc<Cell<u64>>,
    epoch: u64,
    track: Rc<Mp4VideoTrack>,
    key_do: usize,
    target_do: usize,
    target_us: i64,
    signals: Rc<PumpSignals>,
    /// Consumer already took `target_us` — only refill ahead (do not wait for
    /// target in `held`, or we feed to EOF and pin thousands of frames).
    warm_only: bool,
}

impl FramePump {
    async fn run(self) {
        let stopped = || self.signals.stop.get() || self.pump_epoch.get() != self.epoch;
        // Feeding a little past the target (B/P reorder) is intentional; do not
        // drain the entire GOP — that amplified PTS desync on rapid PREV.
        // Cross-GOP warm: keep going until `warm_ahead` presentation frames
        // sit ahead of the target (Play seam). mediabunny mediaSamplesInRange
        // does not stop at the next IDR.
        let max_feeds = (self.track.frame_count() as usize)
            .saturating_add(32)
            .max(64);
        let mut saw_target = self.warm_only;
        let warm_ahead = self.core.borrow().warm_ahead;
        for _ in 0..max_feeds {
            if stopped() {
                break;
            }
            let (fed, ahead, has_target) = {
                let c = self.core.borrow();
                let fed = c.fed_through_order.unwrap_or(self.key_do);
                let ahead = c.held.iter().filter(|h| h.pts_us > self.target_us).count();
                let has_target = c.held.iter().any(|h| h.pts_us == self.target_us);
                (fed, ahead, has_target)
            };
            if has_target {
                saw_target = true;
            }
            // After the target has appeared in `held` (or was taken), park
            // `warm_ahead` frames past it — may cross the next GOP.
            // Requiring `has_target` forever made the pump run to EOF once the
            // consumer took the frame (1080p held≈3200). Stopping before
            // `saw_target` would break seek (B-frames with pts>target first).
            if saw_target && ahead >= warm_ahead {
                break;
            }
            if saw_target
                && fed
                    >= self
                        .target_do
                        .saturating_add(REORDER_FEED_SLACK + warm_ahead)
            {
                break;
            }
            let next = match self.track.samples_after_decode(fed, 1).await {
                Ok(next) => next,
                Err(e) => {
                    *self.signals.error.borrow_mut() = Some(e);
                    break;
                }
            };
            let Some(sample) = next.into_iter().next() else {
                break; // End of track — consumer flushes.
            };
            if stopped() {
                break;
            }
            // Prune before/during backpressure so a long key→target cascade (PREV)
            // does not pin dozens of VideoFrames and wedge decodeQueueSize.
            match await_decode_capacity(
                &self.core,
                Some(self.target_us),
                None,
                Some(&stopped),
                true,
            )
            .await
            {
                Ok(()) => {}
                Err(MediaError::Cancelled) => break,
                Err(e) => {
                    *self.signals.error.borrow_mut() = Some(e);
                    break;
                }
            }
            if stopped() {
                break;
            }
            let is_key = sample.is_keyframe;
            let sample_do = sample.decode_order;
            let fed = self.core.borrow_mut().decode_encoded_sample(&sample, false);
            if let Err(e) = fed {
                *self.signals.error.borrow_mut() = Some(e);
                break;
            }
            if is_key {
                self.core.borrow_mut().key_order = Some(sample_do);
            }
            self.signals
                .beats
                .set(self.signals.beats.get().wrapping_add(1));
            // Give WebCodecs a turn to emit (B-frame reorder) before the next
            // feed — same as the pre-dual-async pump loop.
            if let Err(e) = await_outputs_if_pending(&self.core).await {
                *self.signals.error.borrow_mut() = Some(e);
                break;
            }
        }
        self.signals.done.set(true);
        // Consumer may be parked on `not_empty` — let it run the flush path.
        self.core.borrow().wake_not_empty_waiters();
    }
}

/// Observable decode progress — used to tell a slow pump from a wedged one.
fn progress_signature(
    core: &Rc<RefCell<DecoderCore>>,
    signals: &Rc<PumpSignals>,
) -> (Option<usize>, usize, usize, u32, usize, u64) {
    let c = core.borrow();
    (
        c.fed_through_order,
        c.input_timestamps.len(),
        c.held.len(),
        c.decode_queue_size(),
        c.frames.borrow().len(),
        signals.beats.get(),
    )
}

fn holds_target(core: &Rc<RefCell<DecoderCore>>, target_us: i64) -> bool {
    core.borrow().held.iter().any(|h| h.pts_us == target_us)
}

/// True when `new_key_do` is the sync that immediately follows `prev_key_do`
/// (Play crossing a GOP boundary). Scrub jumps to a non-adjacent key are false.
fn is_sequential_next_key(track: &Mp4VideoTrack, prev_key_do: usize, new_key_do: usize) -> bool {
    if new_key_do <= prev_key_do {
        return false;
    }
    let Some(prev_key_frame) = track
        .index()
        .frames()
        .iter()
        .find(|e| e.is_sync && e.decode_order == prev_key_do)
        .map(|e| e.frame)
    else {
        return false;
    };
    track
        .index()
        .next_keyframe_after(prev_key_frame)
        .and_then(|nk| track.index().get(nk).map(|e| e.decode_order))
        .is_some_and(|d| d == new_key_do)
}

/// Drain outputs for up to `turns` macrotasks; `true` once the target is held.
async fn poll_for_target(
    core: &Rc<RefCell<DecoderCore>>,
    target_us: i64,
    turns: u32,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<bool, MediaError> {
    for _ in 0..turns {
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        await_outputs_if_pending(core).await?;
        let idle = {
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if c.held.iter().any(|h| h.pts_us == target_us) {
                return Ok(true);
            }
            // Only stop early when nothing is in-flight — empty queue alone can
            // race the output callback (All-Intra then fell into 2s flush).
            c.decode_queue_size() == 0 && !c.raw_outputs_pending() && c.input_timestamps.is_empty()
        };
        if idle {
            break;
        }
    }
    Ok(false)
}

/// End-of-input / reorder-slack: drain outputs, then flush once.
///
/// Chrome HW often still has `decodeQueueSize > 0` when the packet pump hits
/// GOP end (long-GOP frame 49: `fed=49, q=5, stamps=9`). A short fixed spin then
/// `flush()` raced to timeout and reported `flush/idle` while work remained.
/// mediabunny: `await decoder.flush()` after packets exhaust — wait for real
/// drain/emit first (`src/media-sink.ts` mediaSamplesInRange).
async fn finish_gop_for_target(
    core: &Rc<RefCell<DecoderCore>>,
    target_us: i64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<OwnedVideoFrame>, MediaError> {
    /// Brief drain for in-flight decodeQ at GOP end. Was 400ms — blocked
    /// prefetch long enough for presentation (cap 6 ≈250ms) to underrun →
    /// ~0.5s Play freeze every GOP on 1080p. mediabunny awaits emit without
    /// a long pre-flush sleep (`src/media-sink.ts` mediaSamplesInRange).
    const DRAIN_BEFORE_FLUSH_MS: f64 = 80.0;
    /// Extra wait after a timed-out flush while q/stamps remain.
    const DRAIN_AFTER_FLUSH_MS: f64 = 800.0;

    if let Some(ready) =
        drain_for_target(core, target_us, DRAIN_BEFORE_FLUSH_MS, is_cancelled).await?
    {
        return Ok(Some(ready));
    }

    let pending = {
        let c = core.borrow();
        !c.input_timestamps.is_empty() || c.decode_queue_size() > 0
    };
    if pending {
        // mediabunny awaits flush to completion; Chromium HW may need a few
        // seconds for the last GOP frames — but do **not** spin 5s *before*
        // flush (that starved play vs the audio clock on long-GOP: skipped≈242).
        let flushed = flush_with_timeout_ms(core, 3_000).await?;
        {
            let mut c = core.borrow_mut();
            c.check_error()?;
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                c.fed_through_order = None;
                c.key_order = None;
                c.input_timestamps.clear();
                return Ok(Some(ready));
            }
        }
        if !flushed {
            // Timer won while q still full — keep draining; do not clear warm yet.
            if let Some(ready) =
                drain_for_target(core, target_us, DRAIN_AFTER_FLUSH_MS, is_cancelled).await?
            {
                let mut c = core.borrow_mut();
                c.fed_through_order = None;
                c.key_order = None;
                c.input_timestamps.clear();
                return Ok(Some(ready));
            }
        }
        {
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                c.fed_through_order = None;
                c.key_order = None;
                c.input_timestamps.clear();
                return Ok(Some(ready));
            }
            // Chromium requires a key chunk after flush — drop warm state even on
            // miss so the next cascade recreates/feeds a key.
            if flushed || c.decode_queue_size() == 0 {
                c.fed_through_order = None;
                c.key_order = None;
                c.input_timestamps.clear();
                c.packets_fed = 0;
                c.rasl_skipped = false;
            }
        }
    }
    Ok(None)
}

/// Wait up to `budget_ms` for `target_us` to appear in `held`, or for the
/// decoder to go fully idle.
async fn drain_for_target(
    core: &Rc<RefCell<DecoderCore>>,
    target_us: i64,
    budget_ms: f64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<OwnedVideoFrame>, MediaError> {
    let deadline = now_ms() + budget_ms;
    loop {
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        await_outputs_if_pending(core).await?;
        {
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                return Ok(Some(ready));
            }
            let idle = c.decode_queue_size() == 0
                && !c.raw_outputs_pending()
                && c.input_timestamps.is_empty();
            if idle || now_ms() >= deadline {
                break;
            }
        }
        wait_for_frame_progress(core).await;
    }
    let mut c = core.borrow_mut();
    c.collect_and_prune_held(target_us)?;
    Ok(c.take_held(target_us))
}

async fn feed_sample(
    core: &Rc<RefCell<DecoderCore>>,
    sample: &EncodedSample,
    force_key: bool,
    prune_before_us: i64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), MediaError> {
    // Prune before/during backpressure so a long key→target cascade (PREV)
    // does not pin dozens of VideoFrames and wedge decodeQueueSize.
    await_decode_capacity(core, Some(prune_before_us), None, Some(is_cancelled), false).await?;
    if is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    core.borrow_mut().decode_encoded_sample(sample, force_key)
}

/// Like [`feed_sample`] but drains remapped frames to `on_frame` during
/// backpressure waits so `held` stays empty (GOP densify / scrub fill).
async fn feed_sample_gop(
    core: &Rc<RefCell<DecoderCore>>,
    sample: &EncodedSample,
    force_key: bool,
    pts_to_id: &mut HashMap<i64, FrameId>,
    on_frame: &mut dyn FnMut(FrameId, OwnedVideoFrame),
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), MediaError> {
    await_decode_capacity(
        core,
        None,
        Some((pts_to_id, on_frame)),
        Some(is_cancelled),
        false,
    )
    .await?;
    if is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    core.borrow_mut().decode_encoded_sample(sample, force_key)
}

async fn flush_with_timeout(core: &Rc<RefCell<DecoderCore>>) -> Result<bool, MediaError> {
    flush_with_timeout_ms(core, 2_000).await
}

/// `Ok(false)` = the race timer won (wedged Chromium decoder); callers keep
/// draining instead of hard-failing. Real `flush()` rejections still error.
async fn flush_with_timeout_ms(
    core: &Rc<RefCell<DecoderCore>>,
    ms: i32,
) -> Result<bool, MediaError> {
    // Build the race promise under a short borrow — never await while borrowed.
    let race = core.borrow().flush_race_promise(ms);
    match JsFuture::from(race).await {
        Ok(v) if v.as_string().as_deref() == Some("timeout") => Ok(false),
        Ok(_) => Ok(true),
        Err(e) => Err(MediaError::from(e)),
    }
}

/// Backpressure before `decode()` — mediabunny `media-sink.ts`:
/// `sampleQueue.length + decodeQueueSize > computeMaxQueueSize(sampleQueue.length)`
/// then `await queueDequeue` (resolved on each output **or** consumer take).
///
/// `drop_before_us`: close held frames strictly before the seek target so
/// `held` does not stay non-empty forever during a long key→N cascade (that
/// kept max=8 and stalled the browser queue — rapid PREV timeout).
///
/// `gop`: when densifying a full GOP, drain remapped frames to the consumer
/// each spin so `held` stays empty (consumer take → max queue 40, not 8).
///
/// `wait_for_consumer`: set by the dual-async packet pump, which has a real
/// concurrent consumer. Single-task callers (key cascade, `decode_gop`) must
/// keep the `decodeQueueSize == 0` escape — with no other task able to take
/// from `held`, waiting there is a guaranteed deadlock
/// (`backpressure timed out (q=0, held=N)` on 1080p B-frames).
///
/// Do **not** treat already-buffered `frames` alone as capacity progress
/// (that spun to timeout while `decodeQueueSize` never dropped).
async fn await_decode_capacity(
    core: &Rc<RefCell<DecoderCore>>,
    drop_before_us: Option<i64>,
    mut gop: Option<GopSink<'_>>,
    is_cancelled: Option<&dyn Fn() -> bool>,
    wait_for_consumer: bool,
) -> Result<(), MediaError> {
    let mut spins = 0u32;
    let mut starved = 0u32;
    loop {
        if is_cancelled.is_some_and(|f| f()) {
            return Err(MediaError::Cancelled);
        }
        let (has_capacity, q, held) = {
            let mut c = core.borrow_mut();
            c.check_error()?;
            // Remap + prune before measuring queue pressure.
            if let Some(us) = drop_before_us {
                c.collect_and_prune_held(us)?;
            } else {
                c.collect_outputs_into_held()?;
            }
            if let Some((pts_to_id, on_frame)) = gop.as_mut() {
                c.flush_held_into_gop(*on_frame, pts_to_id);
            }
            let decoded = c.held.len() as u32;
            let max = compute_max_queue_size(decoded);
            let q = c.decode_queue_size();
            // mediabunny: sampleQueue.length + decodeQueueSize > maxQueueSize
            (decoded + q <= max, q, decoded)
        };
        if has_capacity {
            break;
        }
        if q == 0 {
            if !wait_for_consumer {
                // No concurrent taker: only take/prune/gop-drain can shrink
                // `held`, and both already ran above.
                break;
            }
            // Dual-async helps only when the consumer can actually take soon
            // (target already remapped into `held`). Otherwise waiting is the
            // old single-thread deadlock — feed ahead like mediabunny does when
            // the iterator has not yet pulled the needed sample.
            let target_ready =
                drop_before_us.is_some_and(|us| core.borrow().held.iter().any(|h| h.pts_us == us));
            if !target_ready {
                break;
            }
            // Target is buffered — yield so the consumer `take_held` can run and
            // `onQueueDequeue` us. Bail only if it never does (deep stall).
            starved += 1;
            if starved > PUMP_STARVED_SPINS {
                break;
            }
            // Nudge the consumer in case it is parked on `not_empty`.
            core.borrow().wake_not_empty_waiters();
        } else {
            starved = 0;
        }
        spins += 1;
        if spins > 600 {
            return Err(MediaError::WebCodecs(format!(
                "decodeQueueSize backpressure timed out (q={q}, held={held})"
            )));
        }
        wait_for_capacity_progress(core).await;
    }
    core.borrow().check_error()
}

/// After a feed: give WebCodecs a macrotask to emit; never park forever.
async fn await_outputs_if_pending(core: &Rc<RefCell<DecoderCore>>) -> Result<(), MediaError> {
    {
        let c = core.borrow();
        c.check_error()?;
        if c.raw_outputs_pending() || c.input_timestamps.is_empty() || c.decode_queue_size() == 0 {
            return Ok(());
        }
    }
    poll_output_or_macrotask(core).await;
    core.borrow().check_error()
}

/// mediabunny: await progress until an output arrives (or a short timer).
async fn poll_output_or_macrotask(core: &Rc<RefCell<DecoderCore>>) {
    let (frames, output_waiters) = {
        let c = core.borrow();
        (Rc::clone(&c.frames), Rc::clone(&c.output_waiters))
    };
    let output = js_sys::Promise::new(&mut |resolve, _reject| {
        if !frames.borrow().is_empty() {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        }
        output_waiters.borrow_mut().push(resolve);
    });
    // Macrotask — HW decode often will not finish in a microtask-only spin
    // (that path forced `flush` on long-GOP frame 0 and broke Play).
    let tick = sleep_promise_ms(0);
    let _ = JsFuture::from(promise_race(output, tick)).await;
}

/// Wait for `decodeQueueSize` to drop: `ondequeue`, new `output`, or macrotask.
/// Do **not** resolve early when `frames` is already non-empty — that is not
/// queue progress and caused false backpressure timeouts (mediabunny waits on
/// real `onQueueDequeue` from a new sample **or** a consumer take).
async fn wait_for_capacity_progress(core: &Rc<RefCell<DecoderCore>>) {
    let (output_waiters, dequeue_waiters) = {
        let c = core.borrow();
        (Rc::clone(&c.output_waiters), Rc::clone(&c.dequeue_waiters))
    };
    let output = js_sys::Promise::new(&mut |resolve, _reject| {
        output_waiters.borrow_mut().push(resolve);
    });
    let dequeue = js_sys::Promise::new(&mut |resolve, _reject| {
        dequeue_waiters.borrow_mut().push(resolve);
    });
    let tick = sleep_promise_ms(0);
    let _ = JsFuture::from(js_sys::Promise::race(&js_sys::Array::of3(
        &output, &dequeue, &tick,
    )))
    .await;
}

/// Consumer side of the dual async: wake on a new remapped `held` frame
/// (mediabunny `queueNotEmpty`), a raw decoder output, or a macrotask.
async fn wait_for_frame_progress(core: &Rc<RefCell<DecoderCore>>) {
    let (frames, output_waiters, not_empty_waiters) = {
        let c = core.borrow();
        (
            Rc::clone(&c.frames),
            Rc::clone(&c.output_waiters),
            Rc::clone(&c.not_empty_waiters),
        )
    };
    let output = js_sys::Promise::new(&mut |resolve, _reject| {
        if !frames.borrow().is_empty() {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        }
        output_waiters.borrow_mut().push(resolve);
    });
    let not_empty = js_sys::Promise::new(&mut |resolve, _reject| {
        not_empty_waiters.borrow_mut().push(resolve);
    });
    let tick = sleep_promise_ms(0);
    let _ = JsFuture::from(js_sys::Promise::race(&js_sys::Array::of3(
        &output, &not_empty, &tick,
    )))
    .await;
}

fn wake_waiters_deferred(waiting: Vec<js_sys::Function>) {
    if waiting.is_empty() {
        return;
    }
    // Leave the WebCodecs Closure stack before waking Rust waiters (reentrancy).
    // `spawn_local` is reliable; raw `queueMicrotask(promiseResolve)` can fail to
    // run and leave `open` / decode stuck with `busy=true` and no error.
    for resolve in waiting {
        spawn_local(async move {
            let _ = resolve.call0(&JsValue::NULL);
        });
    }
}

fn ticks_to_micros(ticks: i64, timescale: u64) -> i64 {
    let timescale = timescale.max(1) as i128;
    ((ticks as i128) * 1_000_000 / timescale) as i64
}

/// Monotonic ms, or `0.0` when `performance` is unavailable (stall check then
/// falls back to [`CONSUMER_MAX_SPINS`]).
fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

/// Macrotask turn via `setTimeout` + bare Promise `resolve` (mediabunny-compatible).
/// Required when waiting for WebCodecs to drain — microtasks alone never let it run.
fn sleep_promise_ms(ms: i32) -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, _reject| {
        let window = match web_sys::window() {
            Some(w) => w,
            None => {
                let _ = resolve.call1(&JsValue::NULL, &JsValue::from_str("timeout"));
                return;
            }
        };
        // Resolve with `"timeout"` so `flush_with_timeout_ms` can tell a real
        // flush from the race timer — a bare `resolve` made every wedged flush
        // look successful (`decodeQueueSize` still full afterwards).
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_1(
                &resolve,
                ms,
                &JsValue::from_str("timeout"),
            )
            .is_err()
        {
            let _ = resolve.call1(&JsValue::NULL, &JsValue::from_str("timeout"));
        }
    })
}

fn promise_race(a: js_sys::Promise, b: js_sys::Promise) -> js_sys::Promise {
    js_sys::Promise::race(&js_sys::Array::of2(&a, &b))
}

fn is_chromium() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let Ok(nav) = js_sys::Reflect::get(&window, &"navigator".into()) else {
        return false;
    };
    let Ok(ua) = js_sys::Reflect::get(&nav, &"userAgent".into()) else {
        return false;
    };
    let Some(nav) = ua.as_string() else {
        return false;
    };
    // mediabunny `isChromium`: Chrome / Edge / Chromium UA.
    nav.contains("Chrome") || nav.contains("Chromium") || nav.contains("Edg/")
}
