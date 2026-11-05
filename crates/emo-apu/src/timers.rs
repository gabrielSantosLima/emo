//! The three APU timers.

/// A divider that drives one of the `$FD-$FF` output registers.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Timer {
    base: u32,
    counter: u32,
    target: u8,
    output: u8,
    enabled: bool,
}

impl Timer {
    /// Create a timer with the given base divider (128 for the 8 kHz timers,
    /// 16 for the 64 kHz timer).
    #[must_use]
    pub const fn new(base: u32) -> Self {
        Self {
            base,
            counter: 0,
            target: 0,
            output: 0,
            enabled: false,
        }
    }

    /// Set the divider value. A target of zero divides by 256.
    pub fn set_target(&mut self, value: u8) {
        self.target = value;
    }

    /// Enable or disable the timer. Disabling clears the output and reloads
    /// the divider, as the hardware does when CONTROL enables are cleared.
    pub fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.output = 0;
            self.counter = 0;
        }
        self.enabled = enabled;
    }

    /// Current output value.
    #[must_use]
    pub const fn output(&self) -> u8 {
        self.output
    }

    /// Read the output and reset it to zero, as the hardware does at `$FD-$FF`.
    pub fn take_output(&mut self) -> u8 {
        let value = self.output;
        self.output = 0;
        value
    }

    /// Advance one base clock (one APU cycle).
    pub fn tick(&mut self) {
        self.advance(1);
    }

    /// Advance by a number of APU cycles in one step.
    pub fn advance(&mut self, cycles: u64) {
        if !self.enabled {
            return;
        }

        let divider = if self.target == 0 {
            256
        } else {
            u64::from(self.target)
        };
        let period = u64::from(self.base) * divider;
        let total = u64::from(self.counter) + cycles;
        let steps = total / period;
        self.counter = (total % period) as u32;
        self.output = (self.output + (steps % 16) as u8) & 0x0f;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_counts_up_to_the_divider_and_wraps() {
        let mut timer = Timer::new(1);
        timer.set_target(2);
        timer.set_enabled(true);

        timer.tick();
        assert_eq!(timer.output(), 0);
        timer.tick();
        assert_eq!(timer.output(), 1);
        timer.tick();
        timer.tick();
        assert_eq!(timer.output(), 2);
    }

    #[test]
    fn zero_divider_means_256() {
        let mut timer = Timer::new(1);
        timer.set_target(0);
        timer.set_enabled(true);

        for _ in 0..255 {
            timer.tick();
        }
        assert_eq!(timer.output(), 0);
        timer.tick();
        assert_eq!(timer.output(), 1);
    }

    #[test]
    fn output_is_four_bits_and_reads_reset_it() {
        let mut timer = Timer::new(1);
        timer.set_target(1);
        timer.set_enabled(true);

        for _ in 0..16 {
            timer.tick();
        }
        assert_eq!(timer.output(), 0);

        for _ in 0..3 {
            timer.tick();
        }
        assert_eq!(timer.take_output(), 3);
        assert_eq!(timer.output(), 0);
    }
}
