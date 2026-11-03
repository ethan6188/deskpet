//! Frame scheduling. The pet only renders when a frame is due, and how often a
//! frame is due depends on what the user is doing and the chosen power profile.

use std::time::{Duration, Instant};

/// After this long without interaction the pet counts as idle.
const ACTIVE_FOR: Duration = Duration::from_secs(2);
/// After this long without interaction the pet dozes.
const DOZE_AFTER: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activity {
    /// Dragged, spun or hovered just now.
    Active,
    Idle,
    /// Nobody has touched it for a while.
    Dozing,
}

#[derive(Clone, Copy, Debug)]
pub enum PowerProfile {
    Eco,
    Balanced,
    Smooth,
}

impl PowerProfile {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "eco" => Some(Self::Eco),
            "balanced" => Some(Self::Balanced),
            "smooth" => Some(Self::Smooth),
            _ => None,
        }
    }

    /// Target frames per second. Vsync still caps it at the display's rate.
    pub fn fps(self, activity: Activity) -> u32 {
        match (self, activity) {
            (Self::Eco, Activity::Active) => 30,
            (Self::Eco, Activity::Idle) => 12,
            (Self::Eco, Activity::Dozing) => 4,
            (Self::Balanced, Activity::Active) => 60,
            (Self::Balanced, Activity::Idle) => 24,
            (Self::Balanced, Activity::Dozing) => 8,
            (Self::Smooth, Activity::Active) => 120,
            (Self::Smooth, Activity::Idle) => 30,
            (Self::Smooth, Activity::Dozing) => 10,
        }
    }
}

pub struct Scheduler {
    pub profile: PowerProfile,
    last_input: Instant,
    next_frame: Instant,
}

impl Scheduler {
    pub fn new(profile: PowerProfile) -> Self {
        let now = Instant::now();
        Self {
            profile,
            last_input: now,
            next_frame: now,
        }
    }

    pub fn touch(&mut self) {
        let now = Instant::now();
        // Waking from idle should show the reaction right away, not at the next slow tick.
        if self.activity() != Activity::Active {
            self.next_frame = now;
        }
        self.last_input = now;
    }

    pub fn activity(&self) -> Activity {
        let since = self.last_input.elapsed();
        if since < ACTIVE_FOR {
            Activity::Active
        } else if since < DOZE_AFTER {
            Activity::Idle
        } else {
            Activity::Dozing
        }
    }

    /// Returns true if a frame is due now, and advances the deadline.
    pub fn frame_due(&mut self, now: Instant) -> bool {
        if now < self.next_frame {
            return false;
        }
        let period = Duration::from_secs_f64(1.0 / self.profile.fps(self.activity()) as f64);
        // Advance on a fixed grid so timing doesn't drift; resync after a stall.
        self.next_frame += period;
        if self.next_frame < now {
            self.next_frame = now + period;
        }
        true
    }

    pub fn next_frame(&self) -> Instant {
        self.next_frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_not_due_before_the_deadline() {
        let mut s = Scheduler::new(PowerProfile::Eco);
        let t0 = Instant::now();
        assert!(s.frame_due(t0));
        assert!(!s.frame_due(t0 + Duration::from_millis(5)));
        assert!(s.frame_due(t0 + Duration::from_millis(40)));
    }

    #[test]
    fn a_stall_resyncs_instead_of_bursting() {
        let mut s = Scheduler::new(PowerProfile::Eco);
        let t0 = Instant::now();
        assert!(s.frame_due(t0));
        let late = t0 + Duration::from_secs(1);
        assert!(s.frame_due(late));
        assert!(!s.frame_due(late + Duration::from_millis(1)));
    }
}
