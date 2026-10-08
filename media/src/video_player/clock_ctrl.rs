//! Media clock / rate / audio-timeline control for [`super::VideoPlayer`].
//!
//! Split from `mod.rs` for navigation only — no behavior change (P2).
//! `mediabunny: examples/media-player/media-player.ts` getPlaybackTime / pause / play.

use std::rc::Rc;

use crate::clock_state::{ClockCmd, ClockEvent, reduce as clock_reduce};
use crate::error::MediaError;

use super::VideoPlayer;
use super::now_ms;

impl VideoPlayer {
    pub fn set_volume(&self, volume: f64) {
        if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.set_volume(volume);
        }
    }

    pub fn volume(&self) -> f64 {
        self.shared
            .audio
            .borrow()
            .as_ref()
            .map(|a| a.volume())
            .unwrap_or(0.0)
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.set_muted(muted);
        }
    }

    pub fn muted(&self) -> bool {
        self.shared
            .audio
            .borrow()
            .as_ref()
            .map(|a| a.muted())
            .unwrap_or(true)
    }

    pub(super) fn playback_rate(&self) -> f64 {
        self.shared.clock.borrow().playback_rate()
    }

    /// True when current rate uses shuttle policy (`|rate| >= 2`).
    pub(super) fn is_shuttle(&self) -> bool {
        self.shared.clock.borrow().is_shuttle()
    }

    /// Playback rate: forward `0.25…8`.
    /// Above [`crate::PLAYBACK_RATE_AUDIO_MAX`] → wall clock, audio off
    /// ([`crate::is_wall_clock_rate`]). Through 4× keep `AudioContext` +
    /// fixed-pitch WSOLA stretch (shuttle **present** policy still `≥2`).
    /// Live change snaps clock origins.
    ///
    /// On rate **decrease**, snap to the displayed/committed playhead — shuttle
    /// disables underrun hold so `media_time` can run ahead of decode; keeping
    /// that raced time makes J (8→4→2→1) keep skip-catching for seconds.
    /// `mediabunny: no equivalent` (no rate API).
    pub fn set_playback_rate(&self, rate: f64) {
        let rate = crate::normalize_playback_rate(rate);
        let wall = crate::is_wall_clock_rate(rate);
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        let old_rate = self.shared.clock.borrow().playback_rate();
        let clock_t = self.media_time().max(0.0);
        let t = if rate < old_rate - 1e-9 {
            self.rate_decrease_snap_time(clock_t)
        } else {
            clock_t
        };
        let was_playing = self.shared.clock.borrow().is_playing()
            || audio.as_ref().is_some_and(|a| a.is_playing());

        if let Some(a) = audio {
            if wall {
                if a.is_playing() {
                    let _ = a.stop();
                }
                a.store_playback_rate(rate);
                self.shared.clock.borrow_mut().set_playback_rate(rate);
                if was_playing {
                    self.shared.clock.borrow_mut().start(t);
                    if let Some(now) = now_ms() {
                        self.shared.wall_origin.set(Some(now / 1000.0));
                    }
                }
            } else {
                // Leave shuttle (wall) → review/1× audio while still playing.
                self.shared.clock.borrow_mut().set_playback_rate(rate);
                if was_playing && !a.is_playing() {
                    self.shared.clock.borrow_mut().start(t);
                    self.shared.wall_origin.set(None);
                    a.store_playback_rate(rate);
                    let audio = Rc::clone(&a);
                    let shared = Rc::clone(&self.shared);
                    let start_t = t;
                    wasm_bindgen_futures::spawn_local(async move {
                        // Host may have paused after rate snap (K: pause then 1×,
                        // or rate→pause race). Do not restart audio into a stop.
                        if shared.is_disposed()
                            || !shared.playing.get()
                            || crate::is_wall_clock_rate(shared.clock.borrow().playback_rate())
                        {
                            return;
                        }
                        if let Err(e) = audio.start_spawned(start_t).await {
                            web_sys::console::warn_1(
                                &format!("webmedia: audio after shuttle: {e}").into(),
                            );
                        }
                    });
                } else if a.is_playing() {
                    a.set_playback_rate(rate);
                    self.shared.clock.borrow_mut().start(t);
                } else {
                    a.store_playback_rate(rate);
                }
            }
        } else {
            let playing = self.shared.clock.borrow().is_playing()
                || self
                    .shared
                    .playback_clock
                    .as_ref()
                    .is_some_and(|c| c.is_running());
            self.shared.clock.borrow_mut().set_playback_rate(rate);
            if let Some(c) = self.shared.playback_clock.as_ref() {
                c.set_playback_rate(rate);
            }
            if playing {
                self.shared.clock.borrow_mut().start(t);
                if wall {
                    if let Some(c) = self.shared.playback_clock.as_ref() {
                        let _ = c.stop();
                    }
                    if let Some(now) = now_ms() {
                        self.shared.wall_origin.set(Some(now / 1000.0));
                    }
                } else if let Some(c) = self.shared.playback_clock.as_ref() {
                    self.shared.wall_origin.set(None);
                    c.start(t);
                } else if let Some(now) = now_ms() {
                    self.shared.wall_origin.set(Some(now / 1000.0));
                }
            }
        }
    }

    /// Visual playhead time for rate-down snaps (canvas, else committed).
    fn rate_decrease_snap_time(&self, clock_t: f64) -> f64 {
        let head = self
            .shared
            .displayed
            .borrow()
            .as_ref()
            .and_then(|q| self.frame_timestamp_seconds(q.frame).ok())
            .unwrap_or_else(|| self.state().timestamp.seconds)
            .max(0.0);
        head.min(clock_t)
    }

    /// Media timeline seconds.
    ///
    /// AAC playing → [`AudioOutput`] context clock. No track → silent
    /// [`crate::audio::PlaybackClock`] when running. Else wall `performance`
    /// (shuttle `>4×` / paused).
    /// `mediabunny: examples/media-player/media-player.ts` `getPlaybackTime`.
    pub fn media_time(&self) -> f64 {
        if let Some(a) = self.shared.audio.borrow().as_ref()
            && a.is_playing()
        {
            return a.media_time();
        }
        if let Some(c) = self.shared.playback_clock.as_ref()
            && c.is_running()
        {
            return c.media_time();
        }
        let elapsed = self
            .shared
            .wall_origin
            .get()
            .and_then(|origin| now_ms().map(|now| now / 1000.0 - origin));
        self.shared.clock.borrow().media_time(elapsed)
    }

    /// Resume `AudioContext` while the Play click gesture is still warm.
    /// Call **before** long `prime_playback` awaits.
    pub async fn resume_audio_context(&self) -> Result<(), MediaError> {
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        if let Some(a) = audio {
            a.resume_context().await?;
        } else if let Some(c) = self.shared.playback_clock.as_ref() {
            c.resume_context().await?;
        }
        Ok(())
    }

    /// Arm the media clock after `prime_playback` (host play path).
    ///
    /// Review / 1×–4× + audio track → decode/schedule pump (mediabunny `play` +
    /// `runAudioIterator`; non-1× adds WSOLA stretch). No AAC → silent
    /// [`PlaybackClock`]. Above 4× → wall `performance`.
    ///
    /// Silent / no-AAC PlayPrime should prefer [`Self::arm_silent_play_clock`] on
    /// the **first present rAF** instead — arming here leaves a main-thread gap
    /// (ChG-class open) where `AudioContext` runs ahead → skip storm.
    /// `mediabunny: examples/media-player/media-player.ts` `play` sets
    /// `audioContextStartTime` while `render` already loops.
    pub async fn start_media_clock(&self, media_time_secs: f64) -> Result<(), MediaError> {
        self.apply_clock_event(ClockEvent::Start);
        self.shared.clock.borrow_mut().start(media_time_secs);
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        if crate::is_wall_clock_rate(self.playback_rate()) {
            if let Some(a) = audio {
                let _ = a.stop();
                a.store_playback_rate(self.playback_rate());
            }
            if let Some(c) = self.shared.playback_clock.as_ref() {
                let _ = c.stop();
                c.set_playback_rate(self.playback_rate());
            }
            if let Some(now) = now_ms() {
                self.shared.wall_origin.set(Some(now / 1000.0));
            }
            return Ok(());
        }
        if let Some(a) = audio {
            self.shared.wall_origin.set(None);
            if let Some(c) = self.shared.playback_clock.as_ref() {
                let _ = c.stop();
            }
            a.start_spawned(media_time_secs).await?;
        } else {
            self.arm_silent_play_driver(media_time_secs);
        }
        Ok(())
    }

    /// Sync-arm silent [`PlaybackClock`] / wall on the first play rAF (no AAC).
    ///
    /// Host PlayPrime sets `pending_clock_start` and calls
    /// [`Self::finish_silent_play_prime`] from rAF so the first canvas present
    /// lands **before** the driver runs (mediabunny `startVideoIterator` draw
    /// then `play` / `audioContextStartTime`).
    pub fn arm_silent_play_clock(&self, media_time_secs: f64) {
        self.apply_clock_event(ClockEvent::Start);
        self.shared.clock.borrow_mut().start(media_time_secs);
        if crate::is_wall_clock_rate(self.playback_rate()) {
            if let Some(c) = self.shared.playback_clock.as_ref() {
                let _ = c.stop();
                c.set_playback_rate(self.playback_rate());
            }
            if let Some(now) = now_ms() {
                self.shared.wall_origin.set(Some(now / 1000.0));
            }
            return;
        }
        self.arm_silent_play_driver(media_time_secs);
    }

    /// First present turn for no-AAC PlayPrime: paint presentation front, then
    /// arm the silent clock at that frame's time.
    ///
    /// Presentation holds frames **after** the committed playhead (canvas already
    /// shows it). Drawing the front here is mediabunny `startVideoIterator`'s
    /// first canvas; clock arm matches `play` / `audioContextStartTime`.
    /// Returns the media origin used for the driver (caller stores on session).
    pub fn finish_silent_play_prime(&self) -> Result<f64, MediaError> {
        let origin = if let Some(queued) = self.shared.presentation.borrow_mut().pop_front() {
            let frame = queued.frame;
            self.present_play_frame(frame, queued.video)?;
            self.touch_playhead(frame);
            self.shared.commit_frame(frame)?;
            self.frame_timestamp_seconds(frame)
                .unwrap_or_else(|_| self.state().timestamp.seconds)
        } else {
            self.state().timestamp.seconds
        };
        self.arm_silent_play_clock(origin);
        Ok(origin)
    }

    fn arm_silent_play_driver(&self, media_time_secs: f64) {
        if let Some(c) = self.shared.playback_clock.as_ref() {
            self.shared.wall_origin.set(None);
            c.set_playback_rate(self.playback_rate());
            c.start(media_time_secs);
        } else if let Some(now) = now_ms() {
            self.shared.wall_origin.set(Some(now / 1000.0));
        }
    }

    /// Snap the media clock (pause / seek). Stops audio nodes if attached.
    pub(super) fn stop_media_clock(&self) -> f64 {
        self.apply_clock_event(ClockEvent::Stop);
        let t = if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.stop()
        } else if let Some(c) = self.shared.playback_clock.as_ref() {
            c.stop()
        } else {
            self.media_time()
        };
        self.shared.wall_origin.set(None);
        self.shared.clock.borrow_mut().snap(t);
        let mut engine = self.shared.engine.get();
        engine.audio = crate::AudioResume::Cold;
        self.shared.engine.set(engine);
        t
    }

    /// Soft-silence for Pause button / scrub-while-play — keep `AudioDecoder`
    /// configured.
    ///
    /// Next PlayPrime should [`Self::resume_media_clock`] (not cold
    /// [`Self::start_media_clock`]). Open / EOF / abort-prime still use
    /// [`Self::stop_media_clock`].
    /// `mediabunny: examples/media-player/media-player.ts` `pause`.
    pub(super) fn hold_media_clock_for_scrub(&self) -> f64 {
        self.apply_clock_event(ClockEvent::HoldScrub);
        let t = if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.hold_underrun()
        } else if let Some(c) = self.shared.playback_clock.as_ref() {
            c.hold()
        } else {
            self.media_time()
        };
        self.shared.wall_origin.set(None);
        self.shared.clock.borrow_mut().snap(t);
        let mut engine = self.shared.engine.get();
        engine.audio = crate::AudioResume::Warm;
        self.shared.engine.set(engine);
        t
    }

    /// Consume the scrub soft-hold latch (true → warm audio resume).
    pub fn take_audio_warm_resume(&self) -> bool {
        let mut engine = self.shared.engine.get();
        let warm = engine.take_warm_audio();
        self.shared.engine.set(engine);
        warm
    }

    /// True while soft-held after scrub-while-play (settle may seed play frames).
    pub(crate) fn audio_warm_resume(&self) -> bool {
        self.shared.engine.get().warm_audio()
    }

    /// Freeze the media clock while the presentation queue is empty.
    ///
    /// Prefer this over `skip_behind` when Chromium decode briefly lags the
    /// audio clock (long-GOP: skip spiral → `skipped≈242` then End before EOF).
    /// Keeps `playing` true; host resumes via [`Self::resume_media_clock`] once
    /// prefetch has frames again.
    ///
    /// Soft-holds audio ([`AudioOutput::hold_underrun`]) — silence without
    /// `AudioDecoder` reset — so resume is not a cold reconfigure gap.
    pub fn hold_media_clock_for_underrun(&self) -> f64 {
        self.apply_clock_event(ClockEvent::HoldUnderrun);
        let t = if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.hold_underrun()
        } else if let Some(c) = self.shared.playback_clock.as_ref() {
            c.hold()
        } else {
            self.media_time()
        };
        self.shared.wall_origin.set(None);
        self.shared.clock.borrow_mut().snap(t);
        t
    }

    /// Freeze while the tab / window is hidden (browser suspends `AudioContext`
    /// and throttles rAF). Keeps UI `playing`; on `visibilitychange` → visible the
    /// host calls [`Self::resume_audio_context`] then [`Self::resume_media_clock`].
    ///
    /// No-op when not playing, already hidden-held, or scrub soft-hold owns the
    /// clock. `mediabunny: examples/media-player` — **no** visibility handler;
    /// local adaptation (HTML media / AudioContext background policy).
    pub fn hold_media_clock_for_hidden(&self) -> Option<f64> {
        if !self.is_playing() {
            return None;
        }
        let cs = self.shared.clock_state.get();
        if cs.hidden_holding() {
            return Some(self.shared.clock.borrow().media_time(None));
        }
        if matches!(
            cs,
            crate::ClockState::HoldScrub | crate::ClockState::Stopped
        ) {
            return None;
        }
        self.apply_clock_event(ClockEvent::HoldHidden);
        if !self.shared.clock_state.get().hidden_holding() {
            return None;
        }
        let t = if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.hold_underrun()
        } else if let Some(c) = self.shared.playback_clock.as_ref() {
            c.hold()
        } else {
            self.media_time()
        };
        self.shared.wall_origin.set(None);
        self.shared.clock.borrow_mut().snap(t);
        Some(t)
    }

    /// True while [`Self::hold_media_clock_for_hidden`] latched the clock.
    pub fn is_hidden_hold(&self) -> bool {
        self.shared.clock_state.get().hidden_holding()
    }

    /// Warm resume after [`Self::hold_media_clock_for_underrun`], scrub soft-hold,
    /// or loop seam.
    ///
    /// Reuses the configured `AudioDecoder` + primes the first buffers
    /// ([`AudioOutput::restart_warm`]). Fresh Play after hard pause still uses
    /// [`Self::start_media_clock`] (cold `begin`).
    pub async fn resume_media_clock(&self, media_time_secs: f64) -> Result<(), MediaError> {
        self.apply_clock_event(ClockEvent::Resume);
        self.shared.clock.borrow_mut().start(media_time_secs);
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        if crate::is_wall_clock_rate(self.playback_rate()) {
            if let Some(a) = audio {
                let _ = a.stop();
                a.store_playback_rate(self.playback_rate());
            }
            if let Some(c) = self.shared.playback_clock.as_ref() {
                let _ = c.stop();
                c.set_playback_rate(self.playback_rate());
            }
            if let Some(now) = now_ms() {
                self.shared.wall_origin.set(Some(now / 1000.0));
            }
            return Ok(());
        }
        if let Some(a) = audio {
            self.shared.wall_origin.set(None);
            if let Some(c) = self.shared.playback_clock.as_ref() {
                let _ = c.stop();
            }
            a.restart_warm(media_time_secs).await?;
        } else if let Some(c) = self.shared.playback_clock.as_ref() {
            self.shared.wall_origin.set(None);
            c.set_playback_rate(self.playback_rate());
            c.resume_context().await?;
            c.start(media_time_secs);
        } else if let Some(now) = now_ms() {
            self.shared.wall_origin.set(Some(now / 1000.0));
        }
        Ok(())
    }

    pub(super) fn apply_clock_event(&self, event: ClockEvent) {
        let (next, cmds) = clock_reduce(self.shared.clock_state.get(), event);
        self.shared.clock_state.set(next);
        for cmd in cmds {
            match cmd {
                ClockCmd::NoteUnderrunHoldStart => self.note_underrun_hold_start(),
                ClockCmd::FinishUnderrunTimer => self.finish_underrun_hold_timer(),
                ClockCmd::AbandonUnderrunTimer => {
                    let _ = self.shared.underrun_hold_since_ms.take();
                }
            }
        }
    }
    fn note_underrun_hold_start(&self) {
        if self.shared.underrun_hold_since_ms.get().is_some() {
            return;
        }
        self.shared
            .underrun_holds
            .set(self.shared.underrun_holds.get().saturating_add(1));
        self.shared.underrun_hold_since_ms.set(now_ms());
    }

    fn finish_underrun_hold_timer(&self) {
        let Some(since) = self.shared.underrun_hold_since_ms.take() else {
            return;
        };
        if let Some(now) = now_ms() {
            let dt = (now - since).max(0.0);
            self.shared
                .underrun_hold_ms
                .set(self.shared.underrun_hold_ms.get() + dt);
        }
    }

    /// Hold stats including an in-progress freeze (for live pipeline UI).
    pub(super) fn underrun_hold_ms_now(&self) -> f64 {
        let mut total = self.shared.underrun_hold_ms.get();
        if let (Some(since), Some(now)) = (self.shared.underrun_hold_since_ms.get(), now_ms()) {
            total += (now - since).max(0.0);
        }
        total
    }
    pub(super) fn snap_media_clock_to_playhead(&self) {
        let t = self.state().timestamp.seconds;
        self.shared.wall_origin.set(None);
        if let Some(c) = self.shared.playback_clock.as_ref() {
            c.snap(t);
        }
        self.shared.clock.borrow_mut().snap(t);
    }
}
