//! When to save: about a second after the last edit, never on every keystroke.
//! The app also saves at once when the window loses focus or hides.

use std::time::{Duration, Instant};

/// Tracks edits and says when an autosave is due. Pure timing logic: the app
/// owns the timer and calls [`Buffer::save`](super::Buffer::save).
#[derive(Debug, Clone)]
pub struct Autosave {
    delay: Duration,
    last_edit: Option<Instant>,
}

impl Default for Autosave {
    fn default() -> Self {
        Self::new(Self::DEFAULT_DELAY)
    }
}

impl Autosave {
    pub const DEFAULT_DELAY: Duration = Duration::from_secs(1);

    pub fn new(delay: Duration) -> Self {
        Autosave {
            delay,
            last_edit: None,
        }
    }

    /// Call after every edit. Pushes the save back so typing never triggers it.
    pub fn edited(&mut self, now: Instant) {
        self.last_edit = Some(now);
    }

    /// Call after a save.
    pub fn saved(&mut self) {
        self.last_edit = None;
    }

    /// Whether edits are waiting to be saved.
    pub fn pending(&self) -> bool {
        self.last_edit.is_some()
    }

    /// How long until the save is due, or `None` if nothing is pending.
    pub fn due_in(&self, now: Instant) -> Option<Duration> {
        let last = self.last_edit?;
        Some((last + self.delay).saturating_duration_since(now))
    }

    /// Whether the save is due now.
    pub fn is_due(&self, now: Instant) -> bool {
        self.due_in(now).is_some_and(|d| d.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_a_second_after_the_last_edit() {
        let t0 = Instant::now();
        let mut a = Autosave::default();
        assert!(!a.pending() && !a.is_due(t0));

        a.edited(t0);
        assert!(!a.is_due(t0 + Duration::from_millis(500)));
        // Typing keeps pushing the save back.
        a.edited(t0 + Duration::from_millis(900));
        assert!(!a.is_due(t0 + Duration::from_millis(1500)));
        assert!(a.is_due(t0 + Duration::from_millis(1900)));
        assert_eq!(
            a.due_in(t0 + Duration::from_millis(1400)),
            Some(Duration::from_millis(500))
        );

        a.saved();
        assert!(!a.pending() && a.due_in(t0).is_none());
    }
}
