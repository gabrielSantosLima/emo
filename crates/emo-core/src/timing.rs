//! Master-clock timing for the SNES video standards.
//!
//! Every duration in the emulator is a count of master cycles. The video clock
//! is the reference: the CPU machine cycle and the PPU dot are integer
//! divisions of it. The APU oscillator is the exception, and the scheduler
//! tracks it with an exact ratio instead of a division.
//!
//! Values follow the SNES development manual and fullsnes; the derivation is
//! documented in `docs/theory/` (section 02, "The machine is a clocked system").

/// NTSC master clock: 21.477272 MHz.
pub const MASTER_CLOCK_NTSC: u64 = 21_477_272;

/// PAL master clock: 21.281370 MHz.
pub const MASTER_CLOCK_PAL: u64 = 21_281_370;

/// Master cycles per PPU dot. The PPU emits one dot every four master cycles.
pub const MASTER_CYCLES_PER_DOT: u64 = 4;

/// PPU dots per scanline, including horizontal blanking.
pub const DOTS_PER_LINE: u64 = 341;

/// Master cycles per scanline: 341 dots of 4 master cycles.
pub const MASTER_CYCLES_PER_LINE: u64 = DOTS_PER_LINE * MASTER_CYCLES_PER_DOT;

/// Video standard a cartridge was built for.
///
/// The standard fixes the master clock and the number of scanlines per frame,
/// so it also fixes the frame rate: NTSC runs 262 lines at 60.0985 Hz, PAL runs
/// 312 lines at 50.0070 Hz.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum VideoStandard {
    /// 262 scanlines, 21.477272 MHz master clock.
    Ntsc,
    /// 312 scanlines, 21.281370 MHz master clock.
    Pal,
}

impl VideoStandard {
    /// Master clock frequency of this standard, in hertz.
    #[must_use]
    pub const fn master_clock(self) -> u64 {
        match self {
            Self::Ntsc => MASTER_CLOCK_NTSC,
            Self::Pal => MASTER_CLOCK_PAL,
        }
    }

    /// Scanlines in one frame, including vertical blanking.
    #[must_use]
    pub const fn lines_per_frame(self) -> u64 {
        match self {
            Self::Ntsc => 262,
            Self::Pal => 312,
        }
    }

    /// Master cycles in one frame: lines times cycles per line.
    #[must_use]
    pub const fn master_cycles_per_frame(self) -> u64 {
        self.lines_per_frame() * MASTER_CYCLES_PER_LINE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_budget_matches_hardware() {
        assert_eq!(VideoStandard::Ntsc.master_cycles_per_frame(), 357_368);
        assert_eq!(VideoStandard::Pal.master_cycles_per_frame(), 425_568);
    }

    #[test]
    fn frame_rate_matches_the_video_clock() {
        let ntsc = MASTER_CLOCK_NTSC as f64 / VideoStandard::Ntsc.master_cycles_per_frame() as f64;
        let pal = MASTER_CLOCK_PAL as f64 / VideoStandard::Pal.master_cycles_per_frame() as f64;

        assert!(
            (ntsc - 60.0985).abs() < 0.0001,
            "unexpected NTSC rate: {ntsc}"
        );
        assert!((pal - 50.0070).abs() < 0.0001, "unexpected PAL rate: {pal}");
    }
}
