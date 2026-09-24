//! Exclusive main-decoder checkout (Idle / CheckedOut / reset-pending).
//!
//! Replaces the `decoder_reset: Cell<bool>` latch beside `Option<WebCodecsDecoder>`.
//! See `.ai/architecture.md` § State machines over flags.
//!
//! mediabunny: `src/media-sink.ts` `mediaSamplesInRange` — each range owns a
//! decoder, `terminated` on cancel, `flush` then `close` in `finally`; the next
//! iterator creates a fresh decoder. We share one WASM slot: Checkout ≈ start of
//! a decode op, PutBack ≈ end, Invalidate while out ≈ recreate-on-return
//! (`reset_pending`); Invalidate while idle ≈ immediate `reset()`.

use std::fmt;

/// Whether the main [`crate::VideoPlayer`] decoder is in the slot or checked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderSlotPhase {
    /// `Option` holds the decoder (or none before open).
    Idle,
    /// Decoder taken across an `.await` decode / prefetch.
    CheckedOut,
}

impl fmt::Display for DecoderSlotPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::CheckedOut => write!(f, "CheckedOut"),
        }
    }
}

/// Pure slot lifecycle + reset-pending bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecoderSlotModel {
    pub phase: DecoderSlotPhase,
    /// Set by [`DecoderSlotEvent::Invalidate`] while [`DecoderSlotPhase::CheckedOut`]
    /// (or left set if put-back before apply). Honored on the next successful
    /// [`DecoderSlotEvent::Checkout`] / take path via [`DecoderSlotCmd::ApplyReset`].
    pub reset_pending: bool,
}

impl Default for DecoderSlotModel {
    fn default() -> Self {
        Self {
            phase: DecoderSlotPhase::Idle,
            reset_pending: false,
        }
    }
}

impl DecoderSlotModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_idle(self) -> bool {
        matches!(self.phase, DecoderSlotPhase::Idle)
    }

    pub fn is_checked_out(self) -> bool {
        matches!(self.phase, DecoderSlotPhase::CheckedOut)
    }

    pub fn reset_pending(self) -> bool {
        self.reset_pending
    }
}

/// Input to [`reduce`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderSlotEvent {
    /// Caller took the decoder out of the `Option` slot.
    Checkout,
    /// Caller stored the decoder back into the `Option` slot.
    PutBack,
    /// Seek / settle / PREV invalidate — reset now if idle, else latch pending.
    Invalidate,
    /// Successful `decoder.reset()` after ApplyReset / ResetNow.
    ClearReset,
}

/// Side effects for [`crate::VideoPlayer`] to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderSlotCmd {
    /// Call `reset()` on the decoder still in the slot (Invalidate while Idle).
    ResetNow,
    /// Call `reset()` on the decoder just checked out (pending from prior Invalidate).
    ApplyReset,
}

/// Pure transition.
pub fn reduce(
    model: DecoderSlotModel,
    event: DecoderSlotEvent,
) -> (DecoderSlotModel, Vec<DecoderSlotCmd>) {
    use DecoderSlotCmd as C;
    use DecoderSlotEvent as E;
    use DecoderSlotPhase as P;

    match event {
        E::Checkout => {
            if model.phase != P::Idle {
                // Already out — no-op (caller should not double-take).
                return (model, vec![]);
            }
            let next = DecoderSlotModel {
                phase: P::CheckedOut,
                reset_pending: model.reset_pending,
            };
            let cmds = if model.reset_pending {
                vec![C::ApplyReset]
            } else {
                vec![]
            };
            (next, cmds)
        }

        E::PutBack => {
            if model.phase != P::CheckedOut {
                return (model, vec![]);
            }
            (
                DecoderSlotModel {
                    phase: P::Idle,
                    reset_pending: model.reset_pending,
                },
                vec![],
            )
        }

        E::Invalidate => match model.phase {
            P::Idle => (
                DecoderSlotModel {
                    phase: P::Idle,
                    reset_pending: true,
                },
                vec![C::ResetNow],
            ),
            P::CheckedOut => (
                DecoderSlotModel {
                    phase: P::CheckedOut,
                    reset_pending: true,
                },
                vec![],
            ),
        },

        E::ClearReset => (
            DecoderSlotModel {
                phase: model.phase,
                reset_pending: false,
            },
            vec![],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidate_idle_requests_reset_now() {
        let (next, cmds) = reduce(DecoderSlotModel::new(), DecoderSlotEvent::Invalidate);
        assert!(next.is_idle());
        assert!(next.reset_pending());
        assert_eq!(cmds, vec![DecoderSlotCmd::ResetNow]);
    }

    #[test]
    fn invalidate_checked_out_latches_pending() {
        let m = DecoderSlotModel {
            phase: DecoderSlotPhase::CheckedOut,
            reset_pending: false,
        };
        let (next, cmds) = reduce(m, DecoderSlotEvent::Invalidate);
        assert!(next.is_checked_out());
        assert!(next.reset_pending());
        assert!(cmds.is_empty());
    }

    #[test]
    fn checkout_applies_pending_reset() {
        let m = DecoderSlotModel {
            phase: DecoderSlotPhase::Idle,
            reset_pending: true,
        };
        let (next, cmds) = reduce(m, DecoderSlotEvent::Checkout);
        assert!(next.is_checked_out());
        assert!(next.reset_pending()); // cleared only via ClearReset after reset()
        assert_eq!(cmds, vec![DecoderSlotCmd::ApplyReset]);
    }

    #[test]
    fn checkout_clean_no_reset_cmd() {
        let (next, cmds) = reduce(DecoderSlotModel::new(), DecoderSlotEvent::Checkout);
        assert!(next.is_checked_out());
        assert!(!next.reset_pending());
        assert!(cmds.is_empty());
    }

    #[test]
    fn put_back_keeps_pending() {
        let m = DecoderSlotModel {
            phase: DecoderSlotPhase::CheckedOut,
            reset_pending: true,
        };
        let (next, cmds) = reduce(m, DecoderSlotEvent::PutBack);
        assert!(next.is_idle());
        assert!(next.reset_pending());
        assert!(cmds.is_empty());
    }

    #[test]
    fn clear_reset_after_apply() {
        let m = DecoderSlotModel {
            phase: DecoderSlotPhase::CheckedOut,
            reset_pending: true,
        };
        let (next, _) = reduce(m, DecoderSlotEvent::ClearReset);
        assert!(!next.reset_pending());
        assert!(next.is_checked_out());
    }

    #[test]
    fn double_checkout_noop() {
        let m = DecoderSlotModel {
            phase: DecoderSlotPhase::CheckedOut,
            reset_pending: false,
        };
        let (next, cmds) = reduce(m, DecoderSlotEvent::Checkout);
        assert_eq!(next, m);
        assert!(cmds.is_empty());
    }
}
