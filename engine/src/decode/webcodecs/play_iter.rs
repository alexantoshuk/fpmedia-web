//! Continuous Play iterator — one packet pump + one consumer per play session.
//!
//! `mediabunny: src/media-sink.ts mediaSamplesInRange` — the pump feeds
//! decode-order packets from the start key; while
//! `sampleQueue.length + decoder.getDecodeQueueSize() >= MAX_QUEUE_SIZE` it
//! awaits `queueDequeue` (woken by decoder output or a consumer take, no
//! timer). Samples before the start timestamp are closed on output. The
//! consumer pulls the earliest sample (`queueNotEmpty`). `iterator.return()`
//! closes the decoder → [`PlayIter::finish`] resets it before handing it back.
//!
//! Distinct from [`super::WebCodecsDecoder::decode_frame`]
//! (`samplesAtTimestamps`-shaped single-target seek / step path).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::JsValue;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use super::WebCodecsDecoder;
use super::core::{DecoderCore, PtsToFrameIds, pts_map_push, pts_map_take};
use super::pump::{sleep_promise_ms, ticks_to_micros};
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::demux::Mp4VideoTrack;
use crate::error::MediaError;
use crate::types::FrameId;

/// mediabunny `mediaSamplesInRange` `MAX_QUEUE_SIZE` (decoded + in-flight).
const PLAY_MAX_QUEUE_SIZE: usize = 4;
/// Re-check only if a browser wake is lost — not a pacing timer.
const LOST_WAKE_MS: i32 = 250;
/// Bound the EOF `flush()` (a wedged Chromium decoder never settles).
const EOF_FLUSH_TIMEOUT_MS: i32 = 5_000;
/// Re-kick sample-byte prefetch every N fed packets.
const PREFETCH_EVERY: u64 = 24;
/// [`PlayWaits`] reporting window.
const WAIT_WINDOW_MS: f64 = 1_000.0;
/// rAF interval counted as a main-thread stall (≈3 frames at 60 Hz).
const RAF_STALL_MS: f64 = 50.0;

/// Diagnostic wait totals over the last ~1 s window (stats line only).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayWaits {
    /// Pump awaiting sample bytes (`encoded_sample`).
    pub read_ms: f64,
    pub read_max_ms: f64,
    /// Consumer `next()` parked with no decoded sample.
    pub out_wait_ms: f64,
    /// Pump parked on `MAX_QUEUE_SIZE`.
    pub cap_wait_ms: f64,
    /// rAF intervals over [`RAF_STALL_MS`] and the longest one.
    pub raf_stalls: u32,
    pub raf_max_ms: f64,
}

#[derive(Default)]
struct WaitWindow {
    start: f64,
    cur: PlayWaits,
    last: PlayWaits,
    last_raf: Option<f64>,
}

impl WaitWindow {
    fn roll(&mut self, now: f64) {
        if self.start == 0.0 {
            self.start = now;
        }
        if now - self.start >= WAIT_WINDOW_MS {
            self.last = self.cur;
            self.cur = PlayWaits::default();
            self.start = now;
        }
    }
}

struct Inner {
    core: Rc<RefCell<DecoderCore>>,
    decoder: RefCell<Option<WebCodecsDecoder>>,
    track: Rc<Mp4VideoTrack>,
    start_us: i64,
    index_lead: u64,
    pts_to_id: RefCell<PtsToFrameIds>,
    terminated: Cell<bool>,
    /// Pump fed every packet and flushed (or failed) — no further outputs.
    ended: Cell<bool>,
    error: RefCell<Option<MediaError>>,
    waits: RefCell<WaitWindow>,
}

impl Inner {
    fn note_wait(&self, f: impl FnOnce(&mut PlayWaits)) {
        let now = js_sys::Date::now();
        let mut w = self.waits.borrow_mut();
        w.roll(now);
        f(&mut w.cur);
    }
}

/// Shared handle — the pump task and the player both hold one.
#[derive(Clone)]
pub struct PlayIter(Rc<Inner>);

impl PlayIter {
    /// Take ownership of `decoder` and start the pump at presentation `start`.
    /// Infallible so the decoder always lands in [`Self::finish`]; a bad
    /// `start` surfaces from [`Self::next`] (key verify fails in the pump).
    pub fn start(
        decoder: WebCodecsDecoder,
        track: Rc<Mp4VideoTrack>,
        start: FrameId,
        index_lead: u64,
    ) -> Self {
        let start_us = track
            .index()
            .get(start)
            .map(|e| ticks_to_micros(e.composition_timestamp_ticks, e.timescale))
            .unwrap_or(0);
        let inner = Rc::new(Inner {
            core: Rc::clone(&decoder.core),
            decoder: RefCell::new(Some(decoder)),
            track,
            start_us,
            index_lead: index_lead.max(1),
            pts_to_id: RefCell::new(PtsToFrameIds::new()),
            terminated: Cell::new(false),
            ended: Cell::new(false),
            error: RefCell::new(None),
            waits: RefCell::new(WaitWindow::default()),
        });
        spawn_local(run_pump(Rc::clone(&inner), start));
        Self(inner)
    }

    pub fn same(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// No decoded sample left and none coming (or terminated).
    pub fn exhausted(&self) -> bool {
        let inner = &self.0;
        if inner.terminated.get() {
            return true;
        }
        if !inner.ended.get() {
            return false;
        }
        let Ok(c) = inner.core.try_borrow() else {
            return false;
        };
        c.held.is_empty() && !c.raw_outputs_pending()
    }

    pub fn held_len(&self) -> usize {
        self.0.core.try_borrow().map(|c| c.held.len()).unwrap_or(0)
    }

    pub fn decode_queue_size(&self) -> u32 {
        self.0
            .core
            .try_borrow()
            .map(|c| c.decode_queue_size())
            .unwrap_or(0)
    }

    /// Last completed [`PlayWaits`] window.
    pub fn waits(&self) -> PlayWaits {
        let mut w = self.0.waits.borrow_mut();
        w.roll(js_sys::Date::now());
        w.last
    }

    /// Record one play rAF (main-thread stall meter).
    pub fn note_raf(&self) {
        let now = js_sys::Date::now();
        let prev = self.0.waits.borrow_mut().last_raf.replace(now);
        if let Some(gap) = prev.map(|p| now - p)
            && gap > RAF_STALL_MS
        {
            self.0.note_wait(|w| {
                w.raf_stalls += 1;
                w.raf_max_ms = w.raf_max_ms.max(gap);
            });
        }
    }

    /// Next presentation-order sample, `Ok(None)` at end / after [`Self::finish`].
    pub async fn next(&self) -> Result<Option<(FrameId, OwnedVideoFrame)>, MediaError> {
        let inner = &self.0;
        loop {
            if inner.terminated.get() {
                return Ok(None);
            }
            if let Some(e) = inner.error.borrow_mut().take() {
                return Err(e);
            }
            let taken = {
                let mut c = inner.core.borrow_mut();
                c.check_error()?;
                collect(&mut c, inner.start_us)?;
                let earliest = c
                    .held
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, h)| h.pts_us)
                    .map(|(i, _)| i);
                earliest.map(|i| {
                    let h = c.held.remove(i);
                    c.wake_dequeue_waiters();
                    h
                })
            };
            if let Some(h) = taken {
                let id = pts_map_take(&mut inner.pts_to_id.borrow_mut(), h.pts_us)
                    .or_else(|| frame_at_pts_us(&inner.track, h.pts_us));
                match id {
                    Some(id) => return Ok(Some((id, h.frame))),
                    None => continue,
                }
            }
            if inner.ended.get() {
                return match inner.error.borrow_mut().take() {
                    Some(e) => Err(e),
                    None => Ok(None),
                };
            }
            let t0 = js_sys::Date::now();
            wait_progress(&inner.core).await;
            inner.note_wait(|w| w.out_wait_ms += js_sys::Date::now() - t0);
        }
    }

    /// mediabunny `iterator.return()` → `decoder.close()`. Returns the reset
    /// decoder for the slot (first call only).
    pub fn finish(&self) -> Option<WebCodecsDecoder> {
        let inner = &self.0;
        inner.terminated.set(true);
        let mut decoder = inner.decoder.borrow_mut().take()?;
        // Bumps `pump_epoch` and wakes every waiter so pump + consumer exit.
        let _ = decoder.reset();
        inner.pts_to_id.borrow_mut().clear();
        Some(decoder)
    }
}

/// Remap outputs into `held`; close samples before the iterator start.
fn collect(c: &mut DecoderCore, start_us: i64) -> Result<(), MediaError> {
    c.collect_outputs_into_held()?;
    let before = c.held.len();
    c.held.retain(|h| h.pts_us >= start_us);
    if c.held.len() != before {
        c.wake_dequeue_waiters();
    }
    Ok(())
}

fn frame_at_pts_us(track: &Mp4VideoTrack, pts_us: i64) -> Option<FrameId> {
    let index = track.index();
    let frames = index.frames();
    let i = frames.partition_point(|e| {
        ticks_to_micros(e.composition_timestamp_ticks, e.timescale) < pts_us.saturating_sub(1)
    });
    frames
        .get(i)
        .filter(|e| {
            (ticks_to_micros(e.composition_timestamp_ticks, e.timescale) - pts_us).abs() <= 1
        })
        .map(|e| e.frame)
}

/// Park until decoder output, dequeue, or a consumer take / new sample.
async fn wait_progress(core: &Rc<RefCell<DecoderCore>>) {
    let (frames, output_w, dequeue_w, not_empty_w) = {
        let c = core.borrow();
        (
            Rc::clone(&c.frames),
            Rc::clone(&c.output_waiters),
            Rc::clone(&c.dequeue_waiters),
            Rc::clone(&c.not_empty_waiters),
        )
    };
    let output = js_sys::Promise::new(&mut |resolve, _reject| {
        if !frames.borrow().is_empty() {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        }
        output_w.borrow_mut().push(resolve);
    });
    let dequeue = js_sys::Promise::new(&mut |resolve, _reject| {
        dequeue_w.borrow_mut().push(resolve);
    });
    let not_empty = js_sys::Promise::new(&mut |resolve, _reject| {
        not_empty_w.borrow_mut().push(resolve);
    });
    let lost_wake = sleep_promise_ms(LOST_WAKE_MS);
    let all = js_sys::Array::of4(&output, &dequeue, &not_empty, &lost_wake);
    let _ = JsFuture::from(js_sys::Promise::race(&all)).await;
}

async fn run_pump(inner: Rc<Inner>, start: FrameId) {
    let result = pump(&inner, start).await;
    if let Err(e) = result
        && !inner.terminated.get()
        && !e.is_cancelled()
    {
        *inner.error.borrow_mut() = Some(e);
    }
    inner.ended.set(true);
    if let Ok(c) = inner.core.try_borrow() {
        c.wake_not_empty_waiters();
    }
}

async fn pump(inner: &Inner, start: FrameId) -> Result<(), MediaError> {
    let track = &inner.track;
    // mediabunny: verifyKeyPackets on retrieve.
    let key = track.ensure_key_verified(start).await?;
    if inner.terminated.get() {
        return Err(MediaError::Cancelled);
    }
    let key_do = track
        .index()
        .get(key)
        .map(|e| e.decode_order)
        .ok_or(MediaError::FrameOutOfRange(key, track.frame_count()))?;
    // Fresh decoder per range (mediabunny creates one per iterator).
    inner
        .core
        .borrow_mut()
        .begin_key_cascade(key_do, true, false)?;
    let epoch = inner.core.borrow().pump_epoch.get();
    let stopped = || inner.terminated.get() || inner.core.borrow().pump_epoch.get() != epoch;

    let lead = inner.index_lead;
    let mut after_do: Option<usize> = None;
    let mut fed = 0u64;
    loop {
        if stopped() {
            return Err(MediaError::Cancelled);
        }
        let frame = match after_do {
            None => key,
            Some(after) => {
                let next = track.index().frames_after_decode(after, 1).first().copied();
                match next {
                    Some(f) => f,
                    None => {
                        let indexed = track.indexed_frame_count();
                        if track.fragments_complete() || indexed >= track.frame_count() {
                            break;
                        }
                        track.ensure_indexed_through(indexed).await?;
                        continue;
                    }
                }
            }
        };
        if !track.fragments_complete() {
            let indexed = track.indexed_frame_count();
            if frame.saturating_add(lead) >= indexed {
                track.request_index_through(frame.saturating_add(lead.saturating_mul(2)));
            }
        }
        if fed.is_multiple_of(PREFETCH_EVERY) {
            track.prefetch_samples_ahead(frame, lead);
        }
        let is_sync = track.index().get(frame).is_some_and(|e| e.is_sync);
        if is_sync && after_do.is_some() {
            let _ = track.ensure_key_verified(frame).await?;
        }
        let t0 = js_sys::Date::now();
        let sample = track.encoded_sample(frame).await?;
        let read = js_sys::Date::now() - t0;
        inner.note_wait(|w| {
            w.read_ms += read;
            w.read_max_ms = w.read_max_ms.max(read);
        });
        if stopped() {
            return Err(MediaError::Cancelled);
        }
        let t0 = js_sys::Date::now();
        await_capacity(inner, &stopped).await?;
        inner.note_wait(|w| w.cap_wait_ms += js_sys::Date::now() - t0);
        let pts_us = ticks_to_micros(sample.composition_timestamp_ticks, sample.timescale);
        pts_map_push(&mut inner.pts_to_id.borrow_mut(), pts_us, frame);
        inner
            .core
            .borrow_mut()
            .decode_encoded_sample(&sample, after_do.is_none())?;
        after_do = Some(sample.decode_order);
        fed += 1;
    }

    // mediabunny: `await decoder.flush()` once packets are exhausted.
    let race = inner.core.borrow().flush_race_promise(EOF_FLUSH_TIMEOUT_MS);
    JsFuture::from(race).await.map_err(MediaError::from)?;
    if stopped() {
        return Err(MediaError::Cancelled);
    }
    let mut c = inner.core.borrow_mut();
    c.check_error()?;
    collect(&mut c, inner.start_us)
}

/// `sampleQueue.length + decodeQueueSize >= MAX_QUEUE_SIZE` → await dequeue.
async fn await_capacity(inner: &Inner, stopped: &dyn Fn() -> bool) -> Result<(), MediaError> {
    loop {
        if stopped() {
            return Err(MediaError::Cancelled);
        }
        let full = {
            let mut c = inner.core.borrow_mut();
            c.check_error()?;
            collect(&mut c, inner.start_us)?;
            c.held.len() + c.decode_queue_size() as usize >= PLAY_MAX_QUEUE_SIZE
        };
        if !full {
            return Ok(());
        }
        wait_progress(&inner.core).await;
    }
}
