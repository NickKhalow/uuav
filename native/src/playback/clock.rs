use std::time::Instant;

/// Master clock corrections below this are ignored to avoid jitter.
pub(super) const CLOCK_SNAP_THRESHOLD: f64 = 0.05;

/// Media timeline clock. Free-runs on the monotonic clock while playing and
/// snaps to the externally assigned master clock when it drifts.
///
/// An immutable snapshot published through `ArcSwap`: updates replace the
/// whole value, readers (render/audio threads) never block.
#[derive(Clone, Copy)]
pub(super) struct Clock {
    base: f64,
    anchor: Option<Instant>,
}

impl Clock {
    pub(super) const fn new() -> Self {
        Self {
            base: 0.0,
            anchor: None,
        }
    }

    pub(super) fn now(&self) -> f64 {
        self.base + self.anchor.map_or(0.0, |a| a.elapsed().as_secs_f64())
    }

    /// Re-anchored at `time`, preserving the running/held state.
    pub(super) fn at(self, time: f64) -> Self {
        Self {
            base: time,
            anchor: self.anchor.map(|_| Instant::now()),
        }
    }

    pub(super) fn running(self) -> Self {
        Self {
            base: self.base,
            anchor: self.anchor.or_else(|| Some(Instant::now())),
        }
    }

    pub(super) fn held(self) -> Self {
        Self {
            base: self.now(),
            anchor: None,
        }
    }
}
