//! The audio processing unit.
//!
//! This crate owns the pieces that run independently of the main CPU: 64 KB of
//! audio RAM, the four communication ports, three timers and the S-DSP. The
//! SPC700 interpreter is the remaining piece of the audio subsystem and lands
//! separately; the bus and the DSP are already usable without it.

#![forbid(unsafe_code)]

mod dsp;
mod timers;

pub use dsp::Dsp;
pub use timers::Timer;

/// APU oscillator clock in hertz.
pub const APU_CLOCK: u64 = 1_024_000;

/// Main CPU master clock in hertz, used to translate master cycles.
pub const MASTER_CLOCK: u64 = 21_477_272;

/// ARAM size in bytes.
pub const ARAM_SIZE: usize = 0x1_0000;

/// The audio subsystem.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Apu {
    aram: Box<[u8]>,
    input: [u8; 4],
    output: [u8; 4],
    dsp: Dsp,
    timers: [Timer; 3],
    cycles: u64,
    sample_accumulator: u32,
    #[serde(skip)]
    audio: Vec<i16>,
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Apu {
    /// Create an APU in its post-reset state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            aram: vec![0; ARAM_SIZE].into_boxed_slice(),
            input: [0; 4],
            output: [0; 4],
            dsp: Dsp::new(),
            timers: [Timer::new(128), Timer::new(128), Timer::new(16)],
            cycles: 0,
            sample_accumulator: 0,
            audio: Vec::new(),
        }
    }

    /// Return the APU to its power-on state.
    pub fn reset(&mut self) {
        self.aram.fill(0);
        self.input = [0; 4];
        self.output = [0; 4];
        self.dsp = Dsp::new();
        self.timers = [Timer::new(128), Timer::new(128), Timer::new(16)];
        self.cycles = 0;
        self.sample_accumulator = 0;
        self.audio.clear();
    }

    /// Audio RAM.
    #[must_use]
    pub fn aram(&self) -> &[u8] {
        &self.aram
    }

    /// Mutable audio RAM.
    pub fn aram_mut(&mut self) -> &mut [u8] {
        &mut self.aram
    }

    /// DSP state.
    #[must_use]
    pub const fn dsp(&self) -> &Dsp {
        &self.dsp
    }

    /// APU cycles elapsed since reset.
    #[must_use]
    pub const fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Main CPU reads one of `$2140-$2143`.
    #[must_use]
    pub fn cpu_read(&self, port: usize) -> u8 {
        self.output[port & 3]
    }

    /// Main CPU writes one of `$2140-$2143`.
    pub fn cpu_write(&mut self, port: usize, value: u8) {
        self.input[port & 3] = value;
    }

    /// SPC700 read from its 64 KB address space.
    ///
    /// Only the I/O page is meaningful without the SPC700; ARAM is returned
    /// for every other address.
    #[must_use]
    pub fn spc_read(&self, addr: u16) -> u8 {
        match addr {
            0x00F4..=0x00F7 => self.input[usize::from(addr - 0x00F4)],
            0x00F2 => self.dsp.address(),
            0x00F3 => self.dsp.read_data(&self.aram),
            0x00FD..=0x00FF => self.timers[usize::from(addr - 0x00FD)].output(),
            _ => self.aram[usize::from(addr)],
        }
    }

    /// SPC700 write to its 64 KB address space.
    pub fn spc_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x00F4..=0x00F7 => self.output[usize::from(addr - 0x00F4)] = value,
            0x00F2 => self.dsp.set_address(value),
            0x00F3 => self.dsp.write_data(&mut self.aram, value),
            0x00FA..=0x00FC => {
                let index = usize::from(addr - 0x00FA);
                self.timers[index].set_target(value);
            }
            0x00F1 => {
                self.timers[0].set_enabled(value & 0x01 != 0);
                self.timers[1].set_enabled(value & 0x02 != 0);
                self.timers[2].set_enabled(value & 0x04 != 0);
                if value & 0x80 != 0 {
                    self.input = [0; 4];
                    self.output = [0; 4];
                }
            }
            _ => self.aram[usize::from(addr)] = value,
        }
    }

    /// Advance the APU by a number of main-CPU master cycles.
    pub fn advance(&mut self, master_cycles: u64) {
        let apu_cycles = master_cycles * APU_CLOCK / MASTER_CLOCK;
        self.cycles += apu_cycles;

        for cycle in 0..apu_cycles {
            for timer in &mut self.timers {
                timer.tick();
            }

            self.sample_accumulator += 1;
            if self.sample_accumulator >= 32 {
                self.sample_accumulator = 0;
                let (left, right) = self.dsp.sample(&self.aram);
                self.audio.push(left);
                self.audio.push(right);
            }

            // Keep the buffer bounded when nobody drains it.
            if self.audio.len() > 1_000_000 {
                self.audio.drain(..960_000);
            }

            let _ = cycle;
        }
    }

    /// Mixed stereo samples produced so far, interleaved L/R.
    #[must_use]
    pub fn audio(&self) -> &[i16] {
        &self.audio
    }

    /// Take the accumulated audio buffer.
    pub fn take_audio(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.audio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_bidirectional() {
        let mut apu = Apu::new();

        apu.cpu_write(0, 0x42);
        assert_eq!(apu.spc_read(0x00F4), 0x42);

        apu.spc_write(0x00F6, 0x99);
        assert_eq!(apu.cpu_read(2), 0x99);
    }

    #[test]
    fn dsp_registers_are_reachable_through_the_io_page() {
        let mut apu = Apu::new();

        apu.spc_write(0x00F2, 0x0C);
        apu.spc_write(0x00F3, 0x7F);

        assert_eq!(apu.dsp().volume_left(), 0x7F);
    }

    #[test]
    fn timers_count_apu_cycles() {
        let mut apu = Apu::new();
        apu.spc_write(0x00F1, 0x01);
        apu.spc_write(0x00FA, 0x01);

        apu.advance(100_000);

        assert!(apu.cycles() >= 1_000);
    }

    #[test]
    fn dsp_produces_silent_samples_with_no_voices() {
        let mut apu = Apu::new();
        apu.advance(100_000);

        assert!(!apu.audio().is_empty());
        assert!(apu.audio().iter().all(|sample| *sample == 0));
    }
}
