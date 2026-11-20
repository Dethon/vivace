//! Relative seeks (the ±15 s buttons, media keys) added up while the user
//! keeps pressing, so a burst of presses moves by their sum.
//!
//! Positions reported by playback lag behind the presses: taken as the base
//! for the next press, they made a burst lose or repeat some of its steps.
//! While a skip is on its way, such reports are held back.

use std::time::{Duration, Instant};

/// How long a reported position may stay away from the target after the
/// seek was sent before it is believed anyway.
const SETTLE: Duration = Duration::from_millis(2500);
/// How near the target a reported position counts as the seek having landed.
const LANDED: u32 = 2000;

#[derive(Debug, Default)]
pub struct Skip {
    pending: Option<Pending>,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    target_ms: u32,
    /// When the target was sent; `None` while presses may still add to it.
    sent: Option<Instant>,
}

impl Skip {
    /// Adds a press to the target, starting from `shown_ms` when none is
    /// pending, and returns the new target.
    pub fn press(&mut self, shown_ms: u32, offset_ms: i64) -> u32 {
        let base = self.pending.map_or(shown_ms, |pending| pending.target_ms);
        let target_ms = i64::from(base)
            .saturating_add(offset_ms)
            .clamp(0, i64::from(u32::MAX)) as u32;
        self.pending = Some(Pending {
            target_ms,
            sent: None,
        });
        target_ms
    }

    /// The presses have rested: the target to seek to, now on its way.
    pub fn send(&mut self, now: Instant) -> Option<u32> {
        let pending = self.pending.as_mut()?;
        pending.sent = Some(now);
        Some(pending.target_ms)
    }

    /// Whether a position reported by playback should be shown.
    pub fn accepts(&mut self, reported_ms: u32, now: Instant) -> bool {
        let Some(pending) = self.pending else {
            return true;
        };
        let Some(sent) = pending.sent else {
            return false;
        };
        let landed = reported_ms.abs_diff(pending.target_ms) <= LANDED;
        if landed || now.duration_since(sent) >= SETTLE {
            self.pending = None;
            return true;
        }
        false
    }

    /// Whether presses are adding up, or their seek has yet to land and is
    /// not yet given up on.
    pub fn holds(&self, now: Instant) -> bool {
        self.pending.is_some_and(|pending| {
            pending
                .sent
                .is_none_or(|sent| now.duration_since(sent) < SETTLE)
        })
    }

    /// Forgets the skip: something else moved playback.
    pub fn cancel(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presses_add_up_from_the_position_shown() {
        let mut skip = Skip::default();
        assert_eq!(skip.press(600_000, -15_000), 585_000);
        assert_eq!(skip.press(585_000, -15_000), 570_000);
        assert_eq!(skip.press(570_000, -15_000), 555_000);
        assert_eq!(skip.send(Instant::now()), Some(555_000));
    }

    #[test]
    fn stale_positions_do_not_reset_the_base() {
        let mut skip = Skip::default();
        let now = Instant::now();
        skip.press(600_000, -15_000);
        // Playback still reports where it was before the presses.
        assert!(!skip.accepts(601_000, now));
        skip.press(601_000, -15_000);
        let sent = now + Duration::from_millis(300);
        assert_eq!(skip.send(sent), Some(570_000));
        // A report from before the seek took hold.
        assert!(!skip.accepts(601_500, sent + Duration::from_millis(100)));
        // Pressing on, after the first seek went out but before it landed.
        assert_eq!(skip.press(601_500, -15_000), 555_000);
        assert!(!skip.accepts(570_200, sent + Duration::from_millis(400)));
        let sent = sent + Duration::from_millis(600);
        assert_eq!(skip.send(sent), Some(555_000));
        assert!(skip.accepts(555_100, sent + Duration::from_millis(500)));
        // Landed: the next press starts from what is shown again.
        assert_eq!(skip.press(560_000, 15_000), 575_000);
    }

    #[test]
    fn a_seek_that_never_lands_is_given_up() {
        let mut skip = Skip::default();
        let now = Instant::now();
        skip.press(10_000, -15_000);
        assert_eq!(skip.send(now), Some(0));
        assert!(!skip.accepts(9_000, now + Duration::from_secs(1)));
        assert!(skip.holds(now + Duration::from_secs(1)));
        assert!(!skip.holds(now + SETTLE));
        assert!(skip.accepts(9_000, now + SETTLE));
        assert!(skip.accepts(10_000, now + SETTLE));
    }

    #[test]
    fn cancelling_shows_positions_again() {
        let mut skip = Skip::default();
        skip.press(10_000, 15_000);
        skip.cancel();
        assert!(skip.accepts(42_000, Instant::now()));
        assert_eq!(skip.send(Instant::now()), None);
    }
}
