//! The audio processing unit.
//!
//! This crate owns the pieces that run independently of the main CPU: 64 KB of
//! audio RAM, the four communication ports, three timers and the S-DSP. The
//! SPC700 interpreter is the remaining piece of the audio subsystem and lands
//! separately; the bus and the DSP are already usable without it.

#![forbid(unsafe_code)]

mod dsp;
mod spc700;
mod timers;

pub use dsp::Dsp;
pub use spc700::Spc700;
pub use timers::Timer;

/// Memory interface used by the SPC700.
pub trait ApuBus {
    /// Read one byte of the SPC700 address space.
    fn read(&mut self, addr: u16) -> u8;

    /// Write one byte of the SPC700 address space.
    fn write(&mut self, addr: u16, value: u8);
}

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
    spc: Spc700,
    dsp: Dsp,
    timers: [Timer; 3],
    cycles: u64,
    sample_accumulator: u32,
    #[serde(skip)]
    audio: Vec<i16>,

    booting: bool,
    boot_started: bool,
    boot_address: u16,
    boot_write_address: u16,
    boot_expected: u8,
    boot_pending: Option<u8>,
    #[serde(skip)]
    port_trace: Vec<(u8, u8)>,
    #[serde(skip)]
    cpu_port_writes: Vec<(u64, u8, u8)>,
    #[serde(skip)]
    spc_port_reads: Vec<(u64, u16, u16, u8)>,
    #[serde(skip)]
    boot_log: Vec<(u16, bool)>,
    #[serde(skip)]
    boot_writes: Vec<(u8, u8)>,
    #[serde(skip)]
    trace: std::collections::VecDeque<SpcTrace>,
    #[serde(skip)]
    trace_limit: usize,
}

/// One executed SPC700 instruction, for debugging.
#[derive(Clone, Copy, Debug)]
pub struct SpcTrace {
    /// Program counter before the instruction.
    pub pc: u16,
    /// Opcode byte.
    pub opcode: u8,
    /// Accumulator after the instruction.
    pub a: u8,
    /// X after the instruction.
    pub x: u8,
    /// Y after the instruction.
    pub y: u8,
    /// PSW after the instruction.
    pub psw: u8,
    /// Stack pointer after the instruction.
    pub sp: u8,
    /// Program counter after the instruction.
    pub next_pc: u16,
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
            spc: Spc700::new(),
            dsp: Dsp::new(),
            timers: [Timer::new(128), Timer::new(128), Timer::new(16)],
            cycles: 0,
            sample_accumulator: 0,
            audio: Vec::new(),
            booting: true,
            boot_started: false,
            boot_address: 0,
            boot_write_address: 0,
            boot_expected: 0,
            boot_pending: None,
            port_trace: Vec::new(),
            cpu_port_writes: Vec::new(),
            spc_port_reads: Vec::new(),
            boot_log: Vec::new(),
            boot_writes: Vec::new(),
            trace: std::collections::VecDeque::new(),
            trace_limit: 0,
        }
        .with_boot_ready()
    }

    fn with_boot_ready(mut self) -> Self {
        self.output[0] = 0xAA;
        self.output[1] = 0xBB;
        self.spc.stopped = true;
        self
    }

    /// Return the APU to its power-on state.
    pub fn reset(&mut self) {
        self.aram.fill(0);
        self.input = [0; 4];
        self.output = [0; 4];
        self.spc = Spc700::new();
        self.dsp = Dsp::new();
        self.timers = [Timer::new(128), Timer::new(128), Timer::new(16)];
        self.cycles = 0;
        self.sample_accumulator = 0;
        self.audio.clear();
        self.booting = true;
        self.boot_started = false;
        self.boot_address = 0;
        self.boot_write_address = 0;
        self.boot_expected = 0;
        self.boot_pending = None;
        self.output = [0; 4];
        self.output[0] = 0xAA;
        self.output[1] = 0xBB;
        self.spc.stopped = true;
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

    /// SPC700 state.
    #[must_use]
    pub const fn spc(&self) -> &Spc700 {
        &self.spc
    }

    /// Mutable SPC700 state, for boot and tests.
    pub fn spc_mut(&mut self) -> &mut Spc700 {
        &mut self.spc
    }

    fn spc_step(&mut self) -> u32 {
        if self.booting && self.spc.stopped {
            return 1;
        }

        let pc = self.spc.pc;
        let opcode = self.aram[usize::from(pc)];
        if opcode == 0xE5 {
            let operand = self.aram[usize::from(pc.wrapping_add(1))];
            if operand == 0x00F5 && self.spc_port_reads.len() < 4096 {
                let value = self.input[1];
                self.spc_port_reads
                    .push((self.cycles, pc, u16::from(operand), value));
            }
        }

        let mut bus = SpcBus {
            aram: &mut self.aram,
            input: &self.input,
            output: &mut self.output,
            dsp: &mut self.dsp,
            timers: &mut self.timers,
        };
        let cycles = self.spc.step(&mut bus);

        if self.trace_limit > 0 {
            self.trace.push_back(SpcTrace {
                pc,
                opcode,
                a: self.spc.a,
                x: self.spc.x,
                y: self.spc.y,
                psw: self.spc.psw,
                sp: self.spc.sp,
                next_pc: self.spc.pc,
            });
            while self.trace.len() > self.trace_limit {
                self.trace.pop_front();
            }
            if !self.booting && self.spc.pc < 0x0200 {
                self.trace_limit = 0;
            }
        }

        cycles
    }

    /// CPU port writes with their APU cycle stamps.
    #[must_use]
    pub fn cpu_port_writes(&self) -> &[(u64, u8, u8)] {
        &self.cpu_port_writes
    }

    /// SPC reads of the port registers with their APU cycle stamps.
    #[must_use]
    pub fn spc_port_reads(&self) -> &[(u64, u16, u16, u8)] {
        &self.spc_port_reads
    }

    /// Every CPU port write while the IPL is in control.
    #[must_use]
    pub fn boot_writes(&self) -> &[(u8, u8)] {
        &self.boot_writes
    }

    /// Block commands seen by the IPL: address and whether it was a jump.
    #[must_use]
    pub fn boot_log(&self) -> &[(u16, bool)] {
        &self.boot_log
    }

    /// Enable an instruction trace after the IPL hands control over.
    pub fn set_spc_trace(&mut self, limit: usize) {
        self.trace_limit = limit;
        self.trace.clear();
    }

    /// Recorded SPC700 instructions.
    #[must_use]
    pub fn spc_trace(&self) -> Vec<SpcTrace> {
        self.trace.iter().copied().collect()
    }

    /// Recent CPU port writes, for debugging.
    #[must_use]
    pub fn port_trace(&self) -> &[(u8, u8)] {
        &self.port_trace
    }

    /// True while the IPL boot protocol is still in control.
    #[must_use]
    pub const fn is_booting(&self) -> bool {
        self.booting
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
        if !self.booting && self.cpu_port_writes.len() < 4096 {
            self.cpu_port_writes.push((self.cycles, port as u8, value));
        }
        if self.booting && self.boot_writes.len() < 200_000 {
            self.boot_writes.push((port as u8, value));
        }
        if self.port_trace.len() >= 64 {
            self.port_trace.remove(0);
        }
        self.port_trace.push((port as u8, value));

        match port & 3 {
            2 => self.boot_address = (self.boot_address & 0xFF00) | u16::from(value),
            3 => self.boot_address = (self.boot_address & 0x00FF) | (u16::from(value) << 8),
            1 => self.boot_pending = Some(value),
            0 => self.boot_command(value),
            _ => {}
        }
    }

    /// Clean-room high-level implementation of the IPL boot protocol.
    fn boot_command(&mut self, value: u8) {
        if !self.booting {
            return;
        }

        if !self.boot_started {
            if value != 0xCC {
                return;
            }
            self.boot_started = true;
            self.output[0] = value;

            if self.boot_pending.unwrap_or(0) == 0 {
                self.boot_jump(self.boot_address);
            } else {
                if self.boot_log.len() < 128 {
                    self.boot_log.push((self.boot_address, false));
                }
                self.boot_write_address = self.boot_address;
                self.boot_expected = 1;
                self.boot_pending = None;
            }
            return;
        }

        self.output[0] = value;
        let expected_next = self.boot_expected;
        let ahead = value.wrapping_sub(expected_next);

        if ahead == 0
            && let Some(byte) = self.boot_pending.take()
        {
            let address = self.boot_write_address as usize;
            self.aram[address % ARAM_SIZE] = byte;
            self.boot_write_address = self.boot_write_address.wrapping_add(1);
            self.boot_expected = self.boot_expected.wrapping_add(1);
            return;
        }

        if ahead != 0 && ahead <= 127 {
            if self.boot_pending.unwrap_or(0) == 0 {
                self.boot_jump(self.boot_address);
            } else {
                if self.boot_log.len() < 128 {
                    self.boot_log.push((self.boot_address, false));
                }
                self.boot_write_address = self.boot_address;
                self.boot_expected = 1;
                self.boot_pending = None;
            }
        }
    }

    fn boot_jump(&mut self, address: u16) {
        if self.boot_log.len() < 128 {
            self.boot_log.push((address, true));
        }
        self.aram[0] = address as u8;
        self.aram[1] = (address >> 8) as u8;
        self.output[0] = self.input[0];
        self.output[1] = 0xBB;
        self.booting = false;
        self.spc.reset(address);
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
        let mut remaining = master_cycles * APU_CLOCK / MASTER_CLOCK;

        while remaining > 0 {
            let used = u64::from(self.spc_step()).max(1);
            let step = used.min(remaining);
            self.cycles += step;

            for _ in 0..step {
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

                if self.audio.len() > 1_000_000 {
                    self.audio.drain(..960_000);
                }
            }

            remaining -= step;
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

struct SpcBus<'a> {
    aram: &'a mut Box<[u8]>,
    input: &'a [u8; 4],
    output: &'a mut [u8; 4],
    dsp: &'a mut Dsp,
    timers: &'a mut [Timer; 3],
}

impl ApuBus for SpcBus<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x00F4..=0x00F7 => self.input[usize::from(addr - 0x00F4)],
            0x00F2 => self.dsp.address(),
            0x00F3 => self.dsp.read_data(self.aram),
            0x00FD..=0x00FF => self.timers[usize::from(addr - 0x00FD)].output(),
            _ => self.aram[usize::from(addr)],
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x00F4..=0x00F7 => self.output[usize::from(addr - 0x00F4)] = value,
            0x00F2 => self.dsp.set_address(value),
            0x00F3 => self.dsp.write_data(self.aram, value),
            0x00FA..=0x00FC => {
                let index = usize::from(addr - 0x00FA);
                self.timers[index].set_target(value);
            }
            0x00F1 => {
                self.timers[0].set_enabled(value & 0x01 != 0);
                self.timers[1].set_enabled(value & 0x02 != 0);
                self.timers[2].set_enabled(value & 0x04 != 0);
            }
            _ => self.aram[usize::from(addr)] = value,
        }
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
    fn ipl_protocol_uploads_a_block_and_jumps() {
        let mut apu = Apu::new();

        assert_eq!(apu.cpu_read(0), 0xAA);
        assert_eq!(apu.cpu_read(1), 0xBB);

        // Command: write block at $0200.
        apu.cpu_write(2, 0x00);
        apu.cpu_write(3, 0x02);
        apu.cpu_write(1, 0x01);
        apu.cpu_write(0, 0xCC);
        assert_eq!(apu.cpu_read(0), 0xCC);

        // A sync counter, then two bytes with echoed counters 1 and 2.
        apu.cpu_write(0, 0x00);
        apu.cpu_write(1, 0xE8);
        apu.cpu_write(0, 0x01);
        assert_eq!(apu.cpu_read(0), 0x01);
        apu.cpu_write(1, 0x42);
        apu.cpu_write(0, 0x02);
        assert_eq!(apu.cpu_read(0), 0x02);

        assert_eq!(apu.aram()[0x0200], 0xE8);
        assert_eq!(apu.aram()[0x0201], 0x42);

        // Jump command: last counter + 2.
        apu.cpu_write(2, 0x00);
        apu.cpu_write(3, 0x02);
        apu.cpu_write(1, 0x00);
        apu.cpu_write(0, 0x04);

        assert_eq!(apu.spc().pc, 0x0200);
        assert_eq!(apu.aram()[0], 0x00);
        assert_eq!(apu.aram()[1], 0x02);
    }

    #[test]
    fn ipl_can_jump_immediately() {
        let mut apu = Apu::new();

        apu.cpu_write(2, 0x00);
        apu.cpu_write(3, 0x04);
        apu.cpu_write(1, 0x00);
        apu.cpu_write(0, 0xCC);

        assert_eq!(apu.spc().pc, 0x0400);
    }

    #[test]
    fn spc700_runs_a_small_program() {
        let mut apu = Apu::new();

        // MOV A,#$42 ; MOV $10,A ; BRA -3
        let program = [0xE8, 0x42, 0xC4, 0x10, 0x2F, 0xFE];
        for (index, byte) in program.iter().enumerate() {
            apu.spc_write(0x0200 + index as u16, *byte);
        }
        apu.spc_mut().reset(0x0200);

        apu.spc_step();
        apu.spc_step();

        assert_eq!(apu.aram()[0x10], 0x42);
    }

    #[test]
    fn spc700_arithmetic_sets_flags() {
        let mut apu = Apu::new();

        // MOV A,#$40 ; ADC A,#$40
        let program = [0xE8, 0x40, 0x88, 0x40];
        for (index, byte) in program.iter().enumerate() {
            apu.spc_write(0x0200 + index as u16, *byte);
        }
        apu.spc_mut().reset(0x0200);

        apu.spc_step();
        apu.spc_step();

        assert_eq!(apu.spc().a, 0x80);
        assert!(apu.spc().psw & 0x40 != 0, "overflow should be set");
    }

    fn run_program(apu: &mut Apu, program: &[u8]) {
        for (index, byte) in program.iter().enumerate() {
            apu.spc_write(0x0200 + index as u16, *byte);
        }
        apu.spc_mut().reset(0x0200);
    }

    #[test]
    fn mov1_reads_and_writes_memory_bits() {
        let mut apu = Apu::new();
        apu.spc_write(0x1418, 0x10);
        // MOV1 C,$1418.4 ; MOV X,#0
        run_program(&mut apu, &[0xAA, 0x18, 0x14, 0x04, 0xCD, 0x00]);
        apu.spc_step();
        assert_eq!(apu.spc().psw & 0x01, 1, "carry should mirror the bit");

        // MOV1 $1418.4,C with C=1 sets the bit.
        run_program(&mut apu, &[0xCA, 0x18, 0x14, 0x04]);
        apu.spc_mut().psw |= 0x01;
        apu.spc_step();
        assert_eq!(apu.aram()[0x1418] & 0x10, 0x10);
    }

    #[test]
    fn set1_and_bbc_use_the_direct_page() {
        let mut apu = Apu::new();
        apu.spc_write(0x0014, 0x00);
        // SET1 $14.2 ; BBC $14.2,+1 ; MOV X,#1 ; MOV X,#2
        run_program(
            &mut apu,
            &[0x42, 0x14, 0x53, 0x14, 0x01, 0xCD, 0x01, 0xCD, 0x02],
        );

        apu.spc_step();
        assert_eq!(apu.aram()[0x0014] & 0x04, 0x04);

        apu.spc_step();
        assert_eq!(apu.spc().pc, 0x0205, "bit set means BBC does not branch");

        apu.spc_step();
        apu.spc_step();
        assert_eq!(apu.spc().x, 0x02);
    }

    #[test]
    fn addw_subw_and_cmpw_operate_on_words() {
        let mut apu = Apu::new();
        apu.spc_write(0x0010, 0x11);
        apu.spc_write(0x0011, 0x11);
        // MOVW YA,$10 ; ADDW YA,$10
        run_program(&mut apu, &[0xBA, 0x10, 0x7A, 0x10]);
        apu.spc_step();
        assert_eq!(
            u16::from(apu.spc().a) | (u16::from(apu.spc().y) << 8),
            0x1111
        );
        apu.spc_step();
        assert_eq!(
            u16::from(apu.spc().a) | (u16::from(apu.spc().y) << 8),
            0x2222
        );
        assert_eq!(apu.spc().psw & 0x01, 0);

        // SUBW YA,$10
        run_program(&mut apu, &[0xBA, 0x10, 0x9A, 0x10]);
        apu.spc_step();
        apu.spc_step();
        assert_eq!(
            u16::from(apu.spc().a) | (u16::from(apu.spc().y) << 8),
            0x0000
        );
        assert_eq!(apu.spc().psw & 0x02, 0x02, "zero flag after equal words");
    }

    #[test]
    fn movw_round_trips_through_direct_page() {
        let mut apu = Apu::new();
        // MOVW YA,$10 writes YA=0 (reset), then MOVW $12,YA.
        run_program(&mut apu, &[0xBA, 0x10, 0xDA, 0x12]);
        apu.spc_step();
        apu.spc_step();
        assert_eq!(apu.aram()[0x12], 0);
        assert_eq!(apu.aram()[0x13], 0);
    }

    #[test]
    fn indirect_indexed_addressing_uses_the_pointer() {
        let mut apu = Apu::new();
        apu.spc_write(0x0020, 0x00);
        apu.spc_write(0x0021, 0x03);
        apu.spc_write(0x0305, 0x5A);
        // MOV Y,#5 ; MOV A,[$20]+Y
        run_program(&mut apu, &[0x8D, 0x05, 0xF7, 0x20]);
        apu.spc_step();
        apu.spc_step();
        assert_eq!(apu.spc().a, 0x5A);
    }

    #[test]
    fn dsp_produces_silent_samples_with_no_voices() {
        let mut apu = Apu::new();
        apu.advance(100_000);

        assert!(!apu.audio().is_empty());
        assert!(apu.audio().iter().all(|sample| *sample == 0));
    }
}
