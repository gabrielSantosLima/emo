//! The three APU timers.

/// A down-counter that drives one of the `$FD-$FF` output registers.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Timer {
    divider: u32,
    counter: u32,
    target: u8,
    output: u8,
    enabled: bool,
}

impl Timer {
    /// Create a timer with the given base divider.
    #[must_use]
    pub const fn new(divider: u32) -> Self {
        Self {
            divider,
            counter: 0,
            target: 0,
            output: 0,
            enabled: false,
        }
    }

    /// Set the target value.
    pub fn set_target(&mut self, value: u8) {
        self.target = value;
    }

    /// Enable or disable the timer.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Current output value.
    #[must_use]
    pub const fn output(&self) -> u8 {
        self.output
    }

    /// Advance one base clock.
    pub fn tick(&mut self) {
        if !self.enabled {
            return;
        }

        self.counter += 1;
        if self.counter >= self.divider {
            self.counter = 0;
            self.output = if self.output == self.target {
                0
            } else {
                self.output.wrapping_add(1)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_counts_up_to_the_target() {
        let mut timer = Timer::new(1);
        timer.set_target(2);
        timer.set_enabled(true);

        timer.tick();
        assert_eq!(timer.output(), 1);
        timer.tick();
        assert_eq!(timer.output(), 2);
        timer.tick();
        assert_eq!(timer.output(), 0);
    }

    #[test]
    fn disabled_timers_do_not_move() {
        let mut timer = Timer::new(1);
        timer.tick();
        assert_eq!(timer.output(), 0);
    }
}
