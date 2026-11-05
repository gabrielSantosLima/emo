//! The 65C816 (S-CPU) instruction set core.
//!
//! The core is a plain interpreter: [`Cpu::step`] fetches one opcode and
//! executes it, charging one bus cycle per access or internal cycle. Turning
//! those cycles into master cycles (6 for internal work, 8 or 12 for slow
//! regions) is the job of `emo-core`, not of the CPU.
//!
//! Timing follows the WDC datasheet and the 65816 reference tables; cycle
//! behavior is validated against the `SingleStepTests/65816` vectors. See
//! `docs/theory/` (section 05) for the architectural background.

use serde::{Deserialize, Serialize};

use crate::bus::Bus;

/// Carry flag.
pub const FLAG_C: u8 = 0x01;
/// Zero flag.
pub const FLAG_Z: u8 = 0x02;
/// Interrupt disable flag.
pub const FLAG_I: u8 = 0x04;
/// Decimal flag.
pub const FLAG_D: u8 = 0x08;
/// Index width flag: set means 8-bit X/Y.
pub const FLAG_X: u8 = 0x10;
/// Memory width flag: set means 8-bit accumulator/memory.
pub const FLAG_M: u8 = 0x20;
/// Overflow flag.
pub const FLAG_V: u8 = 0x40;
/// Negative flag.
pub const FLAG_N: u8 = 0x80;

/// Complete architectural state of the processor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Registers {
    /// Accumulator, including the hidden high byte (B).
    pub a: u16,
    /// Index register X.
    pub x: u16,
    /// Index register Y.
    pub y: u16,
    /// Stack pointer.
    pub s: u16,
    /// Direct page register.
    pub d: u16,
    /// Program counter.
    pub pc: u16,
    /// Program bank.
    pub pbr: u8,
    /// Data bank.
    pub dbr: u8,
    /// Processor status byte.
    pub p: u8,
    /// Emulation mode.
    pub e: bool,
}

/// A computed effective address.
#[derive(Clone, Copy, Debug)]
struct Effective {
    addr: u32,
    /// Address of the high byte of a 16-bit access. Absolute accesses carry
    /// into the next bank; direct-page accesses wrap inside bank 0.
    high: u32,
    /// The indexed read needs the extra cycle: either the index register is
    /// 16 bits wide or the index sum crossed a page boundary.
    read_penalty: bool,
    /// Indexed access: writes always pay one extra cycle.
    indexed: bool,
}

impl Effective {
    const fn new(bank: u8, addr: u16) -> Self {
        let full = (bank as u32) << 16 | addr as u32;
        Self {
            addr: full,
            high: (full + 1) & 0x00ff_ffff,
            read_penalty: false,
            indexed: false,
        }
    }

    /// Direct-page access: always bank 0, and a 16-bit access wraps at `$FFFF`.
    const fn direct(addr: u16) -> Self {
        Self {
            addr: addr as u32,
            high: addr.wrapping_add(1) as u32,
            read_penalty: false,
            indexed: false,
        }
    }

    const fn bank(self) -> u8 {
        (self.addr >> 16) as u8
    }

    const fn offset(self) -> u16 {
        self.addr as u16
    }
}

/// The 65C816 processor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cpu {
    a: u16,
    x: u16,
    y: u16,
    s: u16,
    d: u16,
    pc: u16,
    pbr: u8,
    dbr: u8,
    p: u8,
    e: bool,
    cycles: u64,
    irq_line: bool,
    nmi_line: bool,
    waiting: bool,
    stopped: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Create a processor in its post-reset state (minus the vector fetch).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            s: 0x01ff,
            d: 0,
            pc: 0,
            pbr: 0,
            dbr: 0,
            p: FLAG_I | FLAG_M | FLAG_X,
            e: true,
            cycles: 0,
            irq_line: false,
            nmi_line: false,
            waiting: false,
            stopped: false,
        }
    }

    /// Current architectural state.
    #[must_use]
    pub const fn registers(&self) -> Registers {
        Registers {
            a: self.a,
            x: self.x,
            y: self.y,
            s: if self.e {
                0x0100 | (self.s & 0x00ff)
            } else {
                self.s
            },
            d: self.d,
            pc: self.pc,
            pbr: self.pbr,
            dbr: self.dbr,
            p: self.p(),
            e: self.e,
        }
    }

    /// Load architectural state directly (used by tests and save states).
    pub fn set_registers(&mut self, regs: Registers) {
        self.a = regs.a;
        self.x = regs.x;
        self.y = regs.y;
        self.s = regs.s;
        self.d = regs.d;
        self.pc = regs.pc;
        self.pbr = regs.pbr;
        self.dbr = regs.dbr;
        self.p = regs.p;
        self.e = regs.e;
        self.waiting = false;
        self.stopped = false;
        self.normalize_widths();
    }

    /// Bus cycles executed since creation.
    #[must_use]
    pub const fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Reset the cycle counter.
    pub fn set_cycles(&mut self, cycles: u64) {
        self.cycles = cycles;
    }

    /// Processor status byte as software sees it.
    #[must_use]
    pub const fn p(&self) -> u8 {
        if self.e {
            self.p | FLAG_M | FLAG_X
        } else {
            self.p
        }
    }

    /// Execute the reset sequence, pulling the vector at `$00:FFFC`.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        self.e = true;
        self.p = FLAG_I | FLAG_M | FLAG_X;
        self.d = 0;
        self.dbr = 0;
        self.pbr = 0;
        self.s = 0x01ff;
        self.waiting = false;
        self.stopped = false;
        let lo = u16::from(bus.read(0, 0xFFFC));
        let hi = u16::from(bus.read(0, 0xFFFD));
        self.pc = lo | hi << 8;
    }

    /* ---------- Flag helpers ---------- */

    const fn flag(&self, flag: u8) -> bool {
        self.p & flag != 0
    }

    fn set_flag(&mut self, flag: u8, value: bool) {
        if value {
            self.p |= flag;
        } else {
            self.p &= !flag;
        }
    }

    /// True when the accumulator and memory are 8 bits wide.
    #[must_use]
    pub const fn m8(&self) -> bool {
        self.e || self.p & FLAG_M != 0
    }

    /// True when X and Y are 8 bits wide.
    #[must_use]
    pub const fn x8(&self) -> bool {
        self.e || self.p & FLAG_X != 0
    }

    /* ---------- Bus access with cycle accounting ---------- */

    fn read(&mut self, bus: &mut impl Bus, bank: u8, addr: u16) -> u8 {
        self.cycles += 1;
        bus.read(bank, addr)
    }

    fn write(&mut self, bus: &mut impl Bus, bank: u8, addr: u16, value: u8) {
        self.cycles += 1;
        bus.write(bank, addr, value);
    }

    /// An internal cycle: no bus transaction, but it costs one cycle.
    fn idle(&mut self) {
        self.cycles += 1;
    }

    fn fetch_byte(&mut self, bus: &mut impl Bus) -> u8 {
        let value = self.read(bus, self.pbr, self.pc);
        self.pc = self.pc.wrapping_add(1);
        value
    }

    fn fetch_word(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = u16::from(self.fetch_byte(bus));
        let hi = u16::from(self.fetch_byte(bus));
        lo | hi << 8
    }

    fn push(&mut self, bus: &mut impl Bus, value: u8) {
        self.write(bus, 0, self.s, value);
        self.s = self.s.wrapping_sub(1);
    }

    fn pull_raw(&mut self, bus: &mut impl Bus) -> u8 {
        self.s = self.s.wrapping_add(1);
        self.read(bus, 0, self.s)
    }

    fn pull(&mut self, bus: &mut impl Bus) -> u8 {
        self.s = self.s.wrapping_add(1);
        let addr = if self.e {
            0x0100 | (self.s & 0x00ff)
        } else {
            self.s
        };
        self.read(bus, 0, addr)
    }

    fn push_interrupt(&mut self, bus: &mut impl Bus, value: u8) {
        if self.e {
            self.s = 0x0100 | (self.s & 0x00ff);
        }
        self.write(bus, 0, self.s, value);
        self.s = self.s.wrapping_sub(1);
    }

    fn push_word(&mut self, bus: &mut impl Bus, value: u16) {
        self.push(bus, (value >> 8) as u8);
        self.push(bus, value as u8);
    }

    fn pull_word(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = u16::from(self.pull(bus));
        let hi = u16::from(self.pull(bus));
        lo | hi << 8
    }

    /* ---------- Width helpers ---------- */

    const fn index(&self, value: u16) -> u16 {
        if self.x8() { value & 0x00ff } else { value }
    }

    fn set_a(&mut self, value: u16) {
        if self.m8() {
            self.a = (self.a & 0xff00) | (value & 0x00ff);
        } else {
            self.a = value;
        }
    }

    fn set_x(&mut self, value: u16) {
        self.x = self.index(value);
    }

    fn set_y(&mut self, value: u16) {
        self.y = self.index(value);
    }

    fn set_nz(&mut self, value: u8) {
        self.set_flag(FLAG_N, value & 0x80 != 0);
        self.set_flag(FLAG_Z, value == 0);
    }

    fn set_nz_m(&mut self, value: u16) {
        if self.m8() {
            self.set_flag(FLAG_N, value & 0x80 != 0);
            self.set_flag(FLAG_Z, value & 0x00ff == 0);
        } else {
            self.set_flag(FLAG_N, value & 0x8000 != 0);
            self.set_flag(FLAG_Z, value == 0);
        }
    }

    fn set_nz_x(&mut self, value: u16) {
        if self.x8() {
            self.set_flag(FLAG_N, value & 0x80 != 0);
            self.set_flag(FLAG_Z, value & 0x00ff == 0);
        } else {
            self.set_flag(FLAG_N, value & 0x8000 != 0);
            self.set_flag(FLAG_Z, value == 0);
        }
    }

    fn set_nz16(&mut self, value: u16) {
        self.set_flag(FLAG_N, value & 0x8000 != 0);
        self.set_flag(FLAG_Z, value == 0);
    }

    /// Keep the emulation-mode invariants: 8-bit index/memory and a page-1 stack.
    fn normalize_widths(&mut self) {
        if self.e {
            self.p |= FLAG_M | FLAG_X;
            self.s = 0x0100 | (self.s & 0x00ff);
        }
        // Switching the index registers to 8 bits clears their high bytes.
        if self.x8() {
            self.x &= 0x00ff;
            self.y &= 0x00ff;
        }
    }

    /* ---------- Operand reading ---------- */

    fn read_m(&mut self, bus: &mut impl Bus, ea: Effective) -> u16 {
        if ea.read_penalty {
            self.idle();
        }
        if self.m8() {
            let value = self.read(bus, ea.bank(), ea.offset());
            if ea.offset() == 0xFCD9 {
                eprintln!(
                    "dbg read_m bank={:02x} off={:04x} -> {:02x}",
                    ea.bank(),
                    ea.offset(),
                    value
                );
            }
            u16::from(value)
        } else {
            // A 16-bit access keeps the bank and wraps the offset, it does not
            // carry into the next bank.
            let low = ea.addr;
            let high = ea.high;
            let lo = u16::from(self.read(bus, (low >> 16) as u8, low as u16));
            let hi = u16::from(self.read(bus, (high >> 16) as u8, high as u16));
            lo | hi << 8
        }
    }

    fn read_x(&mut self, bus: &mut impl Bus, ea: Effective) -> u16 {
        if ea.read_penalty {
            self.idle();
        }
        if self.x8() {
            u16::from(self.read(bus, ea.bank(), ea.offset()))
        } else {
            let low = ea.addr;
            let high = ea.high;
            let lo = u16::from(self.read(bus, (low >> 16) as u8, low as u16));
            let hi = u16::from(self.read(bus, (high >> 16) as u8, high as u16));
            lo | hi << 8
        }
    }

    fn write_m(&mut self, bus: &mut impl Bus, ea: Effective, value: u16) {
        if ea.indexed {
            self.idle();
        }
        self.write(bus, ea.bank(), ea.offset(), value as u8);
        if !self.m8() {
            self.write(
                bus,
                (ea.high >> 16) as u8,
                ea.high as u16,
                (value >> 8) as u8,
            );
        }
    }

    fn write_x(&mut self, bus: &mut impl Bus, ea: Effective, value: u16) {
        if ea.indexed {
            self.idle();
        }
        self.write(bus, ea.bank(), ea.offset(), value as u8);
        if !self.x8() {
            let high = (ea.addr + 1) & 0x00ff_ffff;
            self.write(bus, (high >> 16) as u8, high as u16, (value >> 8) as u8);
        }
    }

    fn fetch_m(&mut self, bus: &mut impl Bus) -> u16 {
        if self.m8() {
            u16::from(self.fetch_byte(bus))
        } else {
            self.fetch_word(bus)
        }
    }

    fn fetch_x(&mut self, bus: &mut impl Bus) -> u16 {
        if self.x8() {
            u16::from(self.fetch_byte(bus))
        } else {
            self.fetch_word(bus)
        }
    }

    /* ---------- Addressing modes ---------- */

    fn dp_index(&self, offset: u16, index: u16) -> u16 {
        // In native mode the 65816 adds the full 16-bit index to the direct
        // page address (fullsnes: `[D+nn+X]`). In emulation mode it keeps the
        // 6502 behaviour and wraps inside the direct page.
        if self.e && self.d & 0x00ff == 0 {
            self.d.wrapping_add(offset.wrapping_add(index) & 0x00ff)
        } else {
            self.d.wrapping_add(offset).wrapping_add(index)
        }
    }

    fn dp_penalty(&mut self) {
        if self.d & 0x00ff != 0 {
            self.idle();
        }
    }

    fn ea_dir(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.dp_penalty();
        Effective::direct(self.d.wrapping_add(offset))
    }

    fn ea_dir_x(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.dp_penalty();
        self.idle();
        Effective::direct(self.dp_index(offset, self.index(self.x)))
    }

    fn ea_dir_y(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.dp_penalty();
        self.idle();
        Effective::direct(self.dp_index(offset, self.index(self.y)))
    }

    fn ea_abs(&mut self, bus: &mut impl Bus) -> Effective {
        let addr = self.fetch_word(bus);
        Effective::new(self.dbr, addr)
    }

    fn ea_abs_indexed(&mut self, bus: &mut impl Bus, index: u16, index16: bool) -> Effective {
        let addr = self.fetch_word(bus);
        let sum = u32::from(addr) + u32::from(index);
        let offset = sum as u16;
        let full = ((u32::from(self.dbr) << 16).wrapping_add(sum)) & 0x00ff_ffff;
        Effective {
            addr: full,
            high: (full + 1) & 0x00ff_ffff,
            read_penalty: index16 || (addr & 0xff00) != (offset & 0xff00),
            indexed: true,
        }
    }

    fn ea_abs_x(&mut self, bus: &mut impl Bus) -> Effective {
        let index = self.index(self.x);
        self.ea_abs_indexed(bus, index, !self.x8())
    }

    fn ea_abs_y(&mut self, bus: &mut impl Bus) -> Effective {
        let index = self.index(self.y);
        self.ea_abs_indexed(bus, index, !self.x8())
    }

    fn ea_long(&mut self, bus: &mut impl Bus) -> Effective {
        let addr = self.fetch_word(bus);
        let bank = self.fetch_byte(bus);
        Effective::new(bank, addr)
    }

    fn ea_long_x(&mut self, bus: &mut impl Bus) -> Effective {
        let addr = self.fetch_word(bus);
        let bank = self.fetch_byte(bus);
        let full =
            (u32::from(bank) << 16 | u32::from(addr)).wrapping_add(u32::from(self.index(self.x)));
        Effective {
            addr: full & 0x00ff_ffff,
            high: (full + 1) & 0x00ff_ffff,
            read_penalty: false,
            indexed: false,
        }
    }

    fn ea_dir_ind(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.dp_penalty();
        let pointer = self.d.wrapping_add(offset);
        let lo = u16::from(self.read(bus, 0, pointer));
        let hi = u16::from(self.read(bus, 0, pointer.wrapping_add(1)));
        Effective::new(self.dbr, lo | hi << 8)
    }

    fn ea_dir_ind_long(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.dp_penalty();
        let pointer = self.d.wrapping_add(offset);
        let lo = u16::from(self.read(bus, 0, pointer));
        let hi = u16::from(self.read(bus, 0, pointer.wrapping_add(1)));
        let bank = self.read(bus, 0, pointer.wrapping_add(2));
        Effective::new(bank, lo | hi << 8)
    }

    fn ea_dir_ind_x(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.dp_penalty();
        self.idle();
        let pointer = self.dp_index(offset, self.index(self.x));
        let lo = u16::from(self.read(bus, 0, pointer));
        let hi = u16::from(self.read(bus, 0, pointer.wrapping_add(1)));
        Effective::new(self.dbr, lo | hi << 8)
    }

    fn ea_dir_ind_y(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.dp_penalty();
        let pointer = self.d.wrapping_add(offset);
        let lo = u16::from(self.read(bus, 0, pointer));
        let hi = u16::from(self.read(bus, 0, pointer.wrapping_add(1)));
        let base = lo | hi << 8;
        let sum = u32::from(base) + u32::from(self.index(self.y));
        let full = ((u32::from(self.dbr) << 16).wrapping_add(sum)) & 0x00ff_ffff;
        Effective {
            addr: full,
            high: (full + 1) & 0x00ff_ffff,
            // A 16-bit index always pays the cycle, like `abs,Y`.
            read_penalty: !self.x8() || (base & 0xff00) != ((sum as u16) & 0xff00),
            indexed: true,
        }
    }

    fn ea_dir_ind_long_y(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.dp_penalty();
        let pointer = self.d.wrapping_add(offset);
        let lo = u16::from(self.read(bus, 0, pointer));
        let hi = u16::from(self.read(bus, 0, pointer.wrapping_add(1)));
        let bank = self.read(bus, 0, pointer.wrapping_add(2));
        let full = (u32::from(bank) << 16 | u32::from(lo | hi << 8))
            .wrapping_add(u32::from(self.index(self.y)));
        Effective {
            addr: full & 0x00ff_ffff,
            high: (full + 1) & 0x00ff_ffff,
            read_penalty: false,
            indexed: false,
        }
    }

    fn ea_sr(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.idle();
        Effective::direct(self.s.wrapping_add(offset))
    }

    fn ea_sr_ind_y(&mut self, bus: &mut impl Bus) -> Effective {
        let offset = u16::from(self.fetch_byte(bus));
        self.idle();
        self.idle();
        let pointer = self.s.wrapping_add(offset);
        let lo = u16::from(self.read(bus, 0, pointer));
        let hi = u16::from(self.read(bus, 0, pointer.wrapping_add(1)));
        let base = lo | hi << 8;
        let sum = u32::from(base) + u32::from(self.index(self.y));
        let full = ((u32::from(self.dbr) << 16).wrapping_add(sum)) & 0x00ff_ffff;
        Effective {
            addr: full,
            high: (full + 1) & 0x00ff_ffff,
            read_penalty: false,
            indexed: false,
        }
    }

    /* ---------- Stack and control-flow helpers ---------- */

    fn branch(&mut self, bus: &mut impl Bus, condition: bool) {
        let offset = self.fetch_byte(bus) as i8;
        if condition {
            let old = self.pc;
            let target = self.pc.wrapping_add_signed(i16::from(offset));
            self.idle();
            if self.e && (old & 0xff00) != (target & 0xff00) {
                self.idle();
            }
            self.pc = target;
        }
    }

    fn branch_long(&mut self, bus: &mut impl Bus) {
        let offset = self.fetch_word(bus) as i16;
        self.idle();
        self.pc = self.pc.wrapping_add_signed(offset);
    }

    /* ---------- Instruction execution ---------- */

    /// Fetch and execute one instruction.
    pub fn step(&mut self, bus: &mut impl Bus) {
        if self.stopped {
            return;
        }
        if self.waiting {
            self.idle();
            self.check_interrupts(bus);
            return;
        }

        self.check_interrupts(bus);
        if self.waiting || self.stopped {
            return;
        }

        let opcode = self.fetch_byte(bus);
        self.execute(opcode, bus);
    }

    fn check_interrupts(&mut self, bus: &mut impl Bus) {
        if self.nmi_line {
            self.nmi_line = false;
            self.interrupt(bus, Vector::Nmi, false);
        } else if self.irq_line && !self.flag(FLAG_I) && !self.waiting {
            self.interrupt(bus, Vector::Irq, false);
        }
    }

    /// Assert or release the IRQ input.
    pub fn set_irq(&mut self, asserted: bool) {
        self.irq_line = asserted;
    }

    /// Pulse the NMI input. The edge is latched until the interrupt is taken.
    pub fn set_nmi(&mut self, asserted: bool) {
        if asserted {
            self.nmi_line = true;
        }
    }

    #[allow(clippy::too_many_lines)]
    fn execute(&mut self, opcode: u8, bus: &mut impl Bus) {
        match opcode {
            0x00 => {
                // BRK: two-byte instruction (opcode + signature).
                self.fetch_byte(bus);
                self.interrupt(bus, Vector::Brk, true);
            }
            0x01 => {
                let ea = self.ea_dir_ind_x(bus);
                self.op_ora(bus, ea);
            }
            0x02 => {
                self.fetch_byte(bus);
                self.interrupt(bus, Vector::Cop, true);
            }
            0x03 => {
                let ea = self.ea_sr(bus);
                self.op_ora(bus, ea);
            }
            0x04 => {
                let ea = self.ea_dir(bus);
                self.op_tsb(bus, ea);
            }
            0x05 => {
                let ea = self.ea_dir(bus);
                self.op_ora(bus, ea);
            }
            0x06 => {
                let ea = self.ea_dir(bus);
                self.op_asl(bus, ea);
            }
            0x07 => {
                let ea = self.ea_dir_ind_long(bus);
                self.op_ora(bus, ea);
            }
            0x08 => {
                let p = self.p();
                self.idle();
                self.push(bus, p);
            }
            0x09 => {
                let value = self.fetch_m(bus);
                self.op_ora_imm(value);
            }
            0x0A => {
                self.op_asl_a();
                self.idle();
            }
            0x0B => {
                self.idle();
                self.push_word(bus, self.d);
            }
            0x0C => {
                let ea = self.ea_abs(bus);
                self.op_tsb(bus, ea);
            }
            0x0D => {
                let ea = self.ea_abs(bus);
                self.op_ora(bus, ea);
            }
            0x0E => {
                let ea = self.ea_abs(bus);
                self.op_asl(bus, ea);
            }
            0x0F => {
                let ea = self.ea_long(bus);
                self.op_ora(bus, ea);
            }
            0x10 => self.branch(bus, !self.flag(FLAG_N)),
            0x11 => {
                let ea = self.ea_dir_ind_y(bus);
                self.op_ora(bus, ea);
            }
            0x12 => {
                let ea = self.ea_dir_ind(bus);
                self.op_ora(bus, ea);
            }
            0x13 => {
                let ea = self.ea_sr_ind_y(bus);
                self.op_ora(bus, ea);
            }
            0x14 => {
                let ea = self.ea_dir(bus);
                self.op_trb(bus, ea);
            }
            0x15 => {
                let ea = self.ea_dir_x(bus);
                self.op_ora(bus, ea);
            }
            0x16 => {
                let ea = self.ea_dir_x(bus);
                self.op_asl(bus, ea);
            }
            0x17 => {
                let ea = self.ea_dir_ind_long_y(bus);
                self.op_ora(bus, ea);
            }
            0x18 => {
                self.set_flag(FLAG_C, false);
                self.idle();
            }
            0x19 => {
                let ea = self.ea_abs_y(bus);
                self.op_ora(bus, ea);
            }
            0x1A => {
                self.op_inc_a();
                self.idle();
            }
            0x1B => {
                self.s = self.a;
                if self.e {
                    self.s = 0x0100 | (self.s & 0x00ff);
                }
                self.idle();
            }
            0x1C => {
                let ea = self.ea_abs(bus);
                self.op_trb(bus, ea);
            }
            0x1D => {
                let ea = self.ea_abs_x(bus);
                self.op_ora(bus, ea);
            }
            0x1E => {
                let ea = self.ea_abs_x(bus);
                self.op_asl(bus, ea);
            }
            0x1F => {
                let ea = self.ea_long_x(bus);
                self.op_ora(bus, ea);
            }
            0x20 => {
                // JSR abs
                let target = self.fetch_word(bus);
                let return_addr = self.pc.wrapping_sub(1);
                self.idle();
                self.push_interrupt(bus, (return_addr >> 8) as u8);
                self.push_interrupt(bus, return_addr as u8);
                self.pc = target;
            }
            0x21 => {
                let ea = self.ea_dir_ind_x(bus);
                self.op_and(bus, ea);
            }
            0x22 => {
                let target = self.fetch_word(bus);
                let bank = self.fetch_byte(bus);
                let return_addr = self.pc.wrapping_sub(1);
                self.idle();
                self.push(bus, self.pbr);
                self.push_word(bus, return_addr);
                self.pbr = bank;
                self.pc = target;
            }
            0x23 => {
                let ea = self.ea_sr(bus);
                self.op_and(bus, ea);
            }
            0x24 => {
                let ea = self.ea_dir(bus);
                self.op_bit(bus, ea);
            }
            0x25 => {
                let ea = self.ea_dir(bus);
                self.op_and(bus, ea);
            }
            0x26 => {
                let ea = self.ea_dir(bus);
                self.op_rol(bus, ea);
            }
            0x27 => {
                let ea = self.ea_dir_ind_long(bus);
                self.op_and(bus, ea);
            }
            0x28 => {
                self.idle();
                let p = self.pull(bus);
                self.set_p_from_stack(p);
                self.idle();
            }
            0x29 => {
                let value = self.fetch_m(bus);
                self.op_and_imm(value);
            }
            0x2A => {
                self.op_rol_a();
                self.idle();
            }
            0x2B => {
                self.idle();
                let d = self.pull_word(bus);
                self.d = d;
                self.set_nz16(d);
                self.idle();
            }
            0x2C => {
                let ea = self.ea_abs(bus);
                self.op_bit(bus, ea);
            }
            0x2D => {
                let ea = self.ea_abs(bus);
                self.op_and(bus, ea);
            }
            0x2E => {
                let ea = self.ea_abs(bus);
                self.op_rol(bus, ea);
            }
            0x2F => {
                let ea = self.ea_long(bus);
                self.op_and(bus, ea);
            }
            0x30 => self.branch(bus, self.flag(FLAG_N)),
            0x31 => {
                let ea = self.ea_dir_ind_y(bus);
                self.op_and(bus, ea);
            }
            0x32 => {
                let ea = self.ea_dir_ind(bus);
                self.op_and(bus, ea);
            }
            0x33 => {
                let ea = self.ea_sr_ind_y(bus);
                self.op_and(bus, ea);
            }
            0x34 => {
                let ea = self.ea_dir_x(bus);
                self.op_bit(bus, ea);
            }
            0x35 => {
                let ea = self.ea_dir_x(bus);
                self.op_and(bus, ea);
            }
            0x36 => {
                let ea = self.ea_dir_x(bus);
                self.op_rol(bus, ea);
            }
            0x37 => {
                let ea = self.ea_dir_ind_long_y(bus);
                self.op_and(bus, ea);
            }
            0x38 => {
                self.set_flag(FLAG_C, true);
                self.idle();
            }
            0x39 => {
                let ea = self.ea_abs_y(bus);
                self.op_and(bus, ea);
            }
            0x3A => {
                self.op_dec_a();
                self.idle();
            }
            0x3B => {
                self.a = self.s;
                self.set_nz16(self.a);
                self.idle();
            }
            0x3C => {
                let ea = self.ea_abs_x(bus);
                self.op_bit(bus, ea);
            }
            0x3D => {
                let ea = self.ea_abs_x(bus);
                self.op_and(bus, ea);
            }
            0x3E => {
                let ea = self.ea_abs_x(bus);
                self.op_rol(bus, ea);
            }
            0x3F => {
                let ea = self.ea_long_x(bus);
                self.op_and(bus, ea);
            }
            0x40 => self.op_rti(bus),
            0x41 => {
                let ea = self.ea_dir_ind_x(bus);
                self.op_eor(bus, ea);
            }
            0x42 => {
                // WDM: reserved two-byte NOP.
                self.fetch_byte(bus);
            }
            0x43 => {
                let ea = self.ea_sr(bus);
                self.op_eor(bus, ea);
            }
            0x44 => self.op_block_move(bus, true),
            0x45 => {
                let ea = self.ea_dir(bus);
                self.op_eor(bus, ea);
            }
            0x46 => {
                let ea = self.ea_dir(bus);
                self.op_lsr(bus, ea);
            }
            0x47 => {
                let ea = self.ea_dir_ind_long(bus);
                self.op_eor(bus, ea);
            }
            0x48 => {
                self.idle();
                if self.m8() {
                    self.push(bus, self.a as u8);
                } else {
                    self.push_word(bus, self.a);
                }
            }
            0x49 => {
                let value = self.fetch_m(bus);
                self.op_eor_imm(value);
            }
            0x4A => {
                self.op_lsr_a();
                self.idle();
            }
            0x4B => {
                self.idle();
                self.push(bus, self.pbr);
            }
            0x4C => {
                let target = self.fetch_word(bus);
                self.pc = target;
            }
            0x4D => {
                let ea = self.ea_abs(bus);
                self.op_eor(bus, ea);
            }
            0x4E => {
                let ea = self.ea_abs(bus);
                self.op_lsr(bus, ea);
            }
            0x4F => {
                let ea = self.ea_long(bus);
                self.op_eor(bus, ea);
            }
            0x50 => self.branch(bus, !self.flag(FLAG_V)),
            0x51 => {
                let ea = self.ea_dir_ind_y(bus);
                self.op_eor(bus, ea);
            }
            0x52 => {
                let ea = self.ea_dir_ind(bus);
                self.op_eor(bus, ea);
            }
            0x53 => {
                let ea = self.ea_sr_ind_y(bus);
                self.op_eor(bus, ea);
            }
            0x54 => self.op_block_move(bus, false),
            0x55 => {
                let ea = self.ea_dir_x(bus);
                self.op_eor(bus, ea);
            }
            0x56 => {
                let ea = self.ea_dir_x(bus);
                self.op_lsr(bus, ea);
            }
            0x57 => {
                let ea = self.ea_dir_ind_long_y(bus);
                self.op_eor(bus, ea);
            }
            0x58 => {
                self.set_flag(FLAG_I, false);
                self.idle();
            }
            0x59 => {
                let ea = self.ea_abs_y(bus);
                self.op_eor(bus, ea);
            }
            0x5A => {
                self.idle();
                if self.x8() {
                    self.push(bus, self.y as u8);
                } else {
                    self.push_word(bus, self.y);
                }
            }
            0x5B => {
                self.d = self.a;
                self.set_nz16(self.d);
                self.idle();
            }
            0x5C => {
                let target = self.fetch_word(bus);
                let bank = self.fetch_byte(bus);
                self.pbr = bank;
                self.pc = target;
            }
            0x5D => {
                let ea = self.ea_abs_x(bus);
                self.op_eor(bus, ea);
            }
            0x5E => {
                let ea = self.ea_abs_x(bus);
                self.op_lsr(bus, ea);
            }
            0x5F => {
                let ea = self.ea_long_x(bus);
                self.op_eor(bus, ea);
            }
            0x60 => self.op_rts(bus),
            0x61 => {
                let ea = self.ea_dir_ind_x(bus);
                self.op_adc(bus, ea);
            }
            0x62 => {
                let mut offset = self.fetch_word(bus);
                self.idle();
                offset = self.pc.wrapping_add(offset);
                self.push_word(bus, offset);
            }
            0x63 => {
                let ea = self.ea_sr(bus);
                self.op_adc(bus, ea);
            }
            0x64 => {
                let ea = self.ea_dir(bus);
                self.op_stz(bus, ea);
            }
            0x65 => {
                let ea = self.ea_dir(bus);
                self.op_adc(bus, ea);
            }
            0x66 => {
                let ea = self.ea_dir(bus);
                self.op_ror(bus, ea);
            }
            0x67 => {
                let ea = self.ea_dir_ind_long(bus);
                self.op_adc(bus, ea);
            }
            0x68 => {
                self.idle();
                let value = if self.m8() {
                    u16::from(self.pull(bus))
                } else {
                    self.pull_word(bus)
                };
                self.set_a(value);
                self.set_nz_m(self.a);
                self.idle();
            }
            0x69 => {
                let value = self.fetch_m(bus);
                self.op_adc_imm(value);
            }
            0x6A => {
                self.op_ror_a();
                self.idle();
            }
            0x6B => self.op_rtl(bus),
            0x6C => {
                let pointer = self.fetch_word(bus);
                let lo = u16::from(self.read(bus, 0, pointer));
                let hi = u16::from(self.read(bus, 0, pointer.wrapping_add(1)));
                self.pc = lo | hi << 8;
            }
            0x6D => {
                let ea = self.ea_abs(bus);
                self.op_adc(bus, ea);
            }
            0x6E => {
                let ea = self.ea_abs(bus);
                self.op_ror(bus, ea);
            }
            0x6F => {
                let ea = self.ea_long(bus);
                self.op_adc(bus, ea);
            }
            0x70 => self.branch(bus, self.flag(FLAG_V)),
            0x71 => {
                let ea = self.ea_dir_ind_y(bus);
                self.op_adc(bus, ea);
            }
            0x72 => {
                let ea = self.ea_dir_ind(bus);
                self.op_adc(bus, ea);
            }
            0x73 => {
                let ea = self.ea_sr_ind_y(bus);
                self.op_adc(bus, ea);
            }
            0x74 => {
                let ea = self.ea_dir_x(bus);
                self.op_stz(bus, ea);
            }
            0x75 => {
                let ea = self.ea_dir_x(bus);
                self.op_adc(bus, ea);
            }
            0x76 => {
                let ea = self.ea_dir_x(bus);
                self.op_ror(bus, ea);
            }
            0x77 => {
                let ea = self.ea_dir_ind_long_y(bus);
                self.op_adc(bus, ea);
            }
            0x78 => {
                self.set_flag(FLAG_I, true);
                self.idle();
            }
            0x79 => {
                let ea = self.ea_abs_y(bus);
                self.op_adc(bus, ea);
            }
            0x7A => {
                self.idle();
                let value = if self.x8() {
                    u16::from(self.pull(bus))
                } else {
                    self.pull_word(bus)
                };
                self.set_y(value);
                self.set_nz_x(self.y);
                self.idle();
            }
            0x7B => {
                self.a = self.d;
                self.set_nz16(self.a);
                self.idle();
            }
            0x7C => {
                let pointer = self.fetch_word(bus);
                let full = pointer.wrapping_add(self.index(self.x));
                self.idle();
                let lo = u16::from(self.read(bus, self.pbr, full));
                let hi = u16::from(self.read(bus, self.pbr, full.wrapping_add(1)));
                self.pc = lo | hi << 8;
            }
            0x7D => {
                let ea = self.ea_abs_x(bus);
                self.op_adc(bus, ea);
            }
            0x7E => {
                let ea = self.ea_abs_x(bus);
                self.op_ror(bus, ea);
            }
            0x7F => {
                let ea = self.ea_long_x(bus);
                self.op_adc(bus, ea);
            }
            0x80 => {
                let offset = self.fetch_byte(bus) as i8;
                let old = self.pc;
                self.idle();
                let target = self.pc.wrapping_add_signed(i16::from(offset));
                if self.e && (old & 0xff00) != (target & 0xff00) {
                    self.idle();
                }
                self.pc = target;
            }
            0x81 => {
                let ea = self.ea_dir_ind_x(bus);
                self.op_sta(bus, ea);
            }
            0x82 => self.branch_long(bus),
            0x83 => {
                let ea = self.ea_sr(bus);
                self.op_sta(bus, ea);
            }
            0x84 => {
                let ea = self.ea_dir(bus);
                self.op_sty(bus, ea);
            }
            0x85 => {
                let ea = self.ea_dir(bus);
                self.op_sta(bus, ea);
            }
            0x86 => {
                let ea = self.ea_dir(bus);
                self.op_stx(bus, ea);
            }
            0x87 => {
                let ea = self.ea_dir_ind_long(bus);
                self.op_sta(bus, ea);
            }
            0x88 => {
                self.y = self.index(self.y.wrapping_sub(1));
                self.set_nz_x(self.y);
                self.idle();
            }
            0x89 => {
                let value = self.fetch_m(bus);
                self.set_flag(FLAG_Z, value & self.a & self.mask_m() == 0);
            }
            0x8A => {
                let value = self.index(self.x);
                self.set_a(value);
                self.set_nz_m(self.a);
                self.idle();
            }
            0x8B => {
                self.idle();
                self.push(bus, self.dbr);
            }
            0x8C => {
                let ea = self.ea_abs(bus);
                self.op_sty(bus, ea);
            }
            0x8D => {
                let ea = self.ea_abs(bus);
                self.op_sta(bus, ea);
            }
            0x8E => {
                let ea = self.ea_abs(bus);
                self.op_stx(bus, ea);
            }
            0x8F => {
                let ea = self.ea_long(bus);
                self.op_sta(bus, ea);
            }
            0x90 => self.branch(bus, !self.flag(FLAG_C)),
            0x91 => {
                let ea = self.ea_dir_ind_y(bus);
                self.op_sta(bus, ea);
            }
            0x92 => {
                let ea = self.ea_dir_ind(bus);
                self.op_sta(bus, ea);
            }
            0x93 => {
                let ea = self.ea_sr_ind_y(bus);
                self.op_sta(bus, ea);
            }
            0x94 => {
                let ea = self.ea_dir_x(bus);
                self.op_sty(bus, ea);
            }
            0x95 => {
                let ea = self.ea_dir_x(bus);
                self.op_sta(bus, ea);
            }
            0x96 => {
                let ea = self.ea_dir_y(bus);
                self.op_stx(bus, ea);
            }
            0x97 => {
                let ea = self.ea_dir_ind_long_y(bus);
                self.op_sta(bus, ea);
            }
            0x98 => {
                let value = self.index(self.y);
                self.set_a(value);
                self.set_nz_m(self.a);
                self.idle();
            }
            0x99 => {
                let ea = self.ea_abs_y(bus);
                self.op_sta(bus, ea);
            }
            0x9A => {
                self.s = self.index(self.x);
                if self.e {
                    self.s = 0x0100 | (self.s & 0x00ff);
                }
                self.idle();
            }
            0x9B => {
                self.set_y(self.index(self.x));
                self.set_nz_x(self.y);
                self.idle();
            }
            0x9C => {
                let ea = self.ea_abs(bus);
                self.op_stz(bus, ea);
            }
            0x9D => {
                let ea = self.ea_abs_x(bus);
                self.op_sta(bus, ea);
            }
            0x9E => {
                let ea = self.ea_abs_x(bus);
                self.op_stz(bus, ea);
            }
            0x9F => {
                let ea = self.ea_long_x(bus);
                self.op_sta(bus, ea);
            }
            0xA0 => {
                let value = self.fetch_x(bus);
                self.set_y(value);
                self.set_nz_x(self.y);
            }
            0xA1 => {
                let ea = self.ea_dir_ind_x(bus);
                self.op_lda(bus, ea);
            }
            0xA2 => {
                let value = self.fetch_x(bus);
                self.set_x(value);
                self.set_nz_x(self.x);
            }
            0xA3 => {
                let ea = self.ea_sr(bus);
                self.op_lda(bus, ea);
            }
            0xA4 => {
                let ea = self.ea_dir(bus);
                self.op_ldy(bus, ea);
            }
            0xA5 => {
                let ea = self.ea_dir(bus);
                self.op_lda(bus, ea);
            }
            0xA6 => {
                let ea = self.ea_dir(bus);
                self.op_ldx(bus, ea);
            }
            0xA7 => {
                let ea = self.ea_dir_ind_long(bus);
                self.op_lda(bus, ea);
            }
            0xA8 => {
                self.set_y(self.a);
                self.set_nz_x(self.y);
                self.idle();
            }
            0xA9 => {
                let value = self.fetch_m(bus);
                self.op_lda_imm(value);
            }
            0xAA => {
                self.set_x(self.a);
                self.set_nz_x(self.x);
                self.idle();
            }
            0xAB => {
                self.idle();
                let bank = self.pull(bus);
                self.dbr = bank;
                self.set_nz(bank);
                self.idle();
            }
            0xAC => {
                let ea = self.ea_abs(bus);
                self.op_ldy(bus, ea);
            }
            0xAD => {
                let ea = self.ea_abs(bus);
                self.op_lda(bus, ea);
            }
            0xAE => {
                let ea = self.ea_abs(bus);
                self.op_ldx(bus, ea);
            }
            0xAF => {
                let ea = self.ea_long(bus);
                self.op_lda(bus, ea);
            }
            0xB0 => self.branch(bus, self.flag(FLAG_C)),
            0xB1 => {
                let ea = self.ea_dir_ind_y(bus);
                self.op_lda(bus, ea);
            }
            0xB2 => {
                let ea = self.ea_dir_ind(bus);
                self.op_lda(bus, ea);
            }
            0xB3 => {
                let ea = self.ea_sr_ind_y(bus);
                self.op_lda(bus, ea);
            }
            0xB4 => {
                let ea = self.ea_dir_x(bus);
                self.op_ldy(bus, ea);
            }
            0xB5 => {
                let ea = self.ea_dir_x(bus);
                self.op_lda(bus, ea);
            }
            0xB6 => {
                let ea = self.ea_dir_y(bus);
                self.op_ldx(bus, ea);
            }
            0xB7 => {
                let ea = self.ea_dir_ind_long_y(bus);
                self.op_lda(bus, ea);
            }
            0xB8 => {
                self.set_flag(FLAG_V, false);
                self.idle();
            }
            0xB9 => {
                let ea = self.ea_abs_y(bus);
                self.op_lda(bus, ea);
            }
            0xBA => {
                self.set_x(self.s);
                self.set_nz_x(self.x);
                self.idle();
            }
            0xBB => {
                self.set_x(self.index(self.y));
                self.set_nz_x(self.x);
                self.idle();
            }
            0xBC => {
                let ea = self.ea_abs_x(bus);
                self.op_ldy(bus, ea);
            }
            0xBD => {
                let ea = self.ea_abs_x(bus);
                self.op_lda(bus, ea);
            }
            0xBE => {
                let ea = self.ea_abs_y(bus);
                self.op_ldx(bus, ea);
            }
            0xBF => {
                let ea = self.ea_long_x(bus);
                self.op_lda(bus, ea);
            }
            0xC0 => {
                let value = self.fetch_x(bus);
                self.op_cpy_imm(value);
            }
            0xC1 => {
                let ea = self.ea_dir_ind_x(bus);
                self.op_cmp(bus, ea);
            }
            0xC2 => {
                let mask = self.fetch_byte(bus);
                self.p &= !mask;
                self.normalize_widths();
                self.idle();
            }
            0xC3 => {
                let ea = self.ea_sr(bus);
                self.op_cmp(bus, ea);
            }
            0xC4 => {
                let ea = self.ea_dir(bus);
                self.op_cpy(bus, ea);
            }
            0xC5 => {
                let ea = self.ea_dir(bus);
                self.op_cmp(bus, ea);
            }
            0xC6 => {
                let ea = self.ea_dir(bus);
                self.op_dec_mem(bus, ea);
            }
            0xC7 => {
                let ea = self.ea_dir_ind_long(bus);
                self.op_cmp(bus, ea);
            }
            0xC8 => {
                self.y = self.index(self.y.wrapping_add(1));
                self.set_nz_x(self.y);
                self.idle();
            }
            0xC9 => {
                let value = self.fetch_m(bus);
                self.op_cmp_imm(value);
            }
            0xCA => {
                self.x = self.index(self.x.wrapping_sub(1));
                self.set_nz_x(self.x);
                self.idle();
            }
            0xCB => {
                self.waiting = true;
                self.idle();
                self.idle();
                self.idle();
            }
            0xCC => {
                let ea = self.ea_abs(bus);
                self.op_cpy(bus, ea);
            }
            0xCD => {
                let ea = self.ea_abs(bus);
                self.op_cmp(bus, ea);
            }
            0xCE => {
                let ea = self.ea_abs(bus);
                self.op_dec_mem(bus, ea);
            }
            0xCF => {
                let ea = self.ea_long(bus);
                self.op_cmp(bus, ea);
            }
            0xD0 => self.branch(bus, !self.flag(FLAG_Z)),
            0xD1 => {
                let ea = self.ea_dir_ind_y(bus);
                self.op_cmp(bus, ea);
            }
            0xD2 => {
                let ea = self.ea_dir_ind(bus);
                self.op_cmp(bus, ea);
            }
            0xD3 => {
                let ea = self.ea_sr_ind_y(bus);
                self.op_cmp(bus, ea);
            }
            0xD4 => {
                let offset = u16::from(self.fetch_byte(bus));
                self.dp_penalty();
                let pointer = self.d.wrapping_add(offset);
                let lo = u16::from(self.read(bus, 0, pointer));
                let hi = u16::from(self.read(bus, 0, pointer.wrapping_add(1)));
                self.push_word(bus, lo | hi << 8);
            }
            0xD5 => {
                let ea = self.ea_dir_x(bus);
                self.op_cmp(bus, ea);
            }
            0xD6 => {
                let ea = self.ea_dir_x(bus);
                self.op_dec_mem(bus, ea);
            }
            0xD7 => {
                let ea = self.ea_dir_ind_long_y(bus);
                self.op_cmp(bus, ea);
            }
            0xD8 => {
                self.set_flag(FLAG_D, false);
                self.idle();
            }
            0xD9 => {
                let ea = self.ea_abs_y(bus);
                self.op_cmp(bus, ea);
            }
            0xDA => {
                self.idle();
                if self.x8() {
                    self.push(bus, self.x as u8);
                } else {
                    self.push_word(bus, self.x);
                }
            }
            0xDB => {
                self.stopped = true;
                self.idle();
                self.idle();
                self.idle();
            }
            0xDC => {
                let pointer = self.fetch_word(bus);
                let lo = u16::from(self.read(bus, 0, pointer));
                let hi = u16::from(self.read(bus, 0, pointer.wrapping_add(1)));
                let bank = self.read(bus, 0, pointer.wrapping_add(2));
                self.pbr = bank;
                self.pc = lo | hi << 8;
            }
            0xDD => {
                let ea = self.ea_abs_x(bus);
                self.op_cmp(bus, ea);
            }
            0xDE => {
                let ea = self.ea_abs_x(bus);
                self.op_dec_mem(bus, ea);
            }
            0xDF => {
                let ea = self.ea_long_x(bus);
                self.op_cmp(bus, ea);
            }
            0xE0 => {
                let value = self.fetch_x(bus);
                self.op_cpx_imm(value);
            }
            0xE1 => {
                let ea = self.ea_dir_ind_x(bus);
                self.op_sbc(bus, ea);
            }
            0xE2 => {
                let mask = self.fetch_byte(bus);
                self.p |= mask;
                self.normalize_widths();
                self.idle();
            }
            0xE3 => {
                let ea = self.ea_sr(bus);
                self.op_sbc(bus, ea);
            }
            0xE4 => {
                let ea = self.ea_dir(bus);
                self.op_cpx(bus, ea);
            }
            0xE5 => {
                let ea = self.ea_dir(bus);
                self.op_sbc(bus, ea);
            }
            0xE6 => {
                let ea = self.ea_dir(bus);
                self.op_inc_mem(bus, ea);
            }
            0xE7 => {
                let ea = self.ea_dir_ind_long(bus);
                self.op_sbc(bus, ea);
            }
            0xE8 => {
                self.x = self.index(self.x.wrapping_add(1));
                self.set_nz_x(self.x);
                self.idle();
            }
            0xE9 => {
                let value = self.fetch_m(bus);
                self.op_sbc_imm(value);
            }
            0xEA => self.idle(),
            0xEB => {
                self.a = self.a.rotate_left(8);
                // XBA always sets N/Z from the low byte.
                self.set_nz(self.a as u8);
                self.idle();
                self.idle();
            }
            0xEC => {
                let ea = self.ea_abs(bus);
                self.op_cpx(bus, ea);
            }
            0xED => {
                let ea = self.ea_abs(bus);
                self.op_sbc(bus, ea);
            }
            0xEE => {
                let ea = self.ea_abs(bus);
                self.op_inc_mem(bus, ea);
            }
            0xEF => {
                let ea = self.ea_long(bus);
                self.op_sbc(bus, ea);
            }
            0xF0 => self.branch(bus, self.flag(FLAG_Z)),
            0xF1 => {
                let ea = self.ea_dir_ind_y(bus);
                self.op_sbc(bus, ea);
            }
            0xF2 => {
                let ea = self.ea_dir_ind(bus);
                self.op_sbc(bus, ea);
            }
            0xF3 => {
                let ea = self.ea_sr_ind_y(bus);
                self.op_sbc(bus, ea);
            }
            0xF4 => {
                let value = self.fetch_word(bus);
                self.push_word(bus, value);
            }
            0xF5 => {
                let ea = self.ea_dir_x(bus);
                self.op_sbc(bus, ea);
            }
            0xF6 => {
                let ea = self.ea_dir_x(bus);
                self.op_inc_mem(bus, ea);
            }
            0xF7 => {
                let ea = self.ea_dir_ind_long_y(bus);
                self.op_sbc(bus, ea);
            }
            0xF8 => {
                self.set_flag(FLAG_D, true);
                self.idle();
            }
            0xF9 => {
                let ea = self.ea_abs_y(bus);
                self.op_sbc(bus, ea);
            }
            0xFA => {
                self.idle();
                let value = if self.x8() {
                    u16::from(self.pull(bus))
                } else {
                    self.pull_word(bus)
                };
                self.set_x(value);
                self.set_nz_x(self.x);
                self.idle();
            }
            0xFB => {
                let carry = self.flag(FLAG_C);
                let was_emulation = self.e;
                self.e = carry;
                self.set_flag(FLAG_C, was_emulation);
                self.normalize_widths();
                self.idle();
            }
            0xFC => {
                let pointer = self.fetch_word(bus);
                let full = pointer.wrapping_add(self.index(self.x));
                self.idle();
                let lo = u16::from(self.read(bus, self.pbr, full));
                let hi = u16::from(self.read(bus, self.pbr, full.wrapping_add(1)));
                let return_addr = self.pc.wrapping_sub(1);
                self.push_word(bus, return_addr);
                self.pc = lo | hi << 8;
            }
            0xFD => {
                let ea = self.ea_abs_x(bus);
                self.op_sbc(bus, ea);
            }
            0xFE => {
                let ea = self.ea_abs_x(bus);
                self.op_inc_mem(bus, ea);
            }
            0xFF => {
                let ea = self.ea_long_x(bus);
                self.op_sbc(bus, ea);
            }
        }
    }

    /* ---------- Individual operations ---------- */

    const fn mask_m(&self) -> u16 {
        if self.m8() { 0x00ff } else { 0xffff }
    }

    const fn nmask_m(&self) -> u16 {
        if self.m8() { 0x0080 } else { 0x8000 }
    }

    fn op_lda(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        self.set_a(value);
        self.set_nz_m(value);
    }

    fn op_lda_imm(&mut self, value: u16) {
        self.set_a(value);
        self.set_nz_m(value);
    }

    fn op_ldx(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_x(bus, ea);
        self.set_x(value);
        self.set_nz_x(value);
    }

    fn op_ldy(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_x(bus, ea);
        self.set_y(value);
        self.set_nz_x(value);
    }

    fn op_sta(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.a;
        self.write_m(bus, ea, value);
    }

    fn op_stx(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.x;
        self.write_x(bus, ea, value);
    }

    fn op_sty(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.y;
        self.write_x(bus, ea, value);
    }

    fn op_stz(&mut self, bus: &mut impl Bus, ea: Effective) {
        let zero = 0;
        self.write_m(bus, ea, zero);
    }

    fn op_ora(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        self.a |= value & self.mask_m();
        self.set_nz_m(self.a);
    }

    fn op_ora_imm(&mut self, value: u16) {
        self.a |= value & self.mask_m();
        self.set_nz_m(self.a);
    }

    fn op_and(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        self.a &= value | !self.mask_m();
        self.set_nz_m(self.a);
    }

    fn op_and_imm(&mut self, value: u16) {
        self.a &= value | !self.mask_m();
        self.set_nz_m(self.a);
    }

    fn op_eor(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        self.a ^= value & self.mask_m();
        self.set_nz_m(self.a);
    }

    fn op_eor_imm(&mut self, value: u16) {
        self.a ^= value & self.mask_m();
        self.set_nz_m(self.a);
    }

    fn op_cmp(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        self.compare(self.a, value, self.mask_m());
    }

    fn op_cmp_imm(&mut self, value: u16) {
        self.compare(self.a, value, self.mask_m());
    }

    fn op_cpx(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_x(bus, ea);
        self.compare(self.x, value, self.mask_x());
    }

    fn op_cpx_imm(&mut self, value: u16) {
        self.compare(self.x, value, self.mask_x());
    }

    fn op_cpy(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_x(bus, ea);
        self.compare(self.y, value, self.mask_x());
    }

    fn op_cpy_imm(&mut self, value: u16) {
        self.compare(self.y, value, self.mask_x());
    }

    const fn mask_x(&self) -> u16 {
        if self.x8() { 0x00ff } else { 0xffff }
    }

    fn compare(&mut self, register: u16, value: u16, mask: u16) {
        let diff = register.wrapping_sub(value) & mask;
        // The negative flag comes from the sign bit of the active width, not
        // from bit 7 of the low byte in 16-bit mode.
        let sign = if mask == 0x00ff { 0x0080 } else { 0x8000 };
        self.set_flag(FLAG_C, register & mask >= value & mask);
        self.set_flag(FLAG_Z, diff == 0);
        self.set_flag(FLAG_N, diff & sign != 0);
    }

    fn op_adc(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        self.adc(value);
    }

    fn op_adc_imm(&mut self, value: u16) {
        self.adc(value);
    }

    fn op_sbc(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        self.sbc(value);
    }

    fn op_sbc_imm(&mut self, value: u16) {
        self.sbc(value);
    }

    fn adc(&mut self, value: u16) {
        let mask = self.mask_m();
        let sign = self.nmask_m();
        let carry = u16::from(self.flag(FLAG_C));
        let a = self.a & mask;
        let b = value & mask;
        let sum = u32::from(a) + u32::from(b) + u32::from(carry);
        let binary = sum as u16 & mask;

        if self.flag(FLAG_D) {
            let (result, carry_out, _) = decimal_add(a, b, carry, mask);
            self.set_flag(FLAG_V, decimal_overflow_add(a, b, carry, mask));
            self.a = (self.a & !mask) | result;
            self.set_flag(FLAG_C, carry_out);
            self.set_nz_m(result);
        } else {
            self.set_flag(FLAG_V, ((a ^ binary) & (b ^ binary) & sign) != 0);
            self.a = (self.a & !mask) | binary;
            self.set_flag(FLAG_C, sum > u32::from(mask));
            self.set_nz_m(binary);
        }
    }

    fn sbc(&mut self, value: u16) {
        let mask = self.mask_m();
        let sign = self.nmask_m();
        let borrow = 1 - u16::from(self.flag(FLAG_C));
        let a = self.a & mask;
        let b = value & mask;
        let diff = u32::from(a)
            .wrapping_sub(u32::from(b))
            .wrapping_sub(u32::from(borrow));
        let binary = diff as u16 & mask;

        if self.flag(FLAG_D) {
            let (result, borrow_out, _) = decimal_sub(a, b, borrow, mask);
            self.set_flag(FLAG_V, decimal_overflow_sub(a, b, borrow, mask));
            self.a = (self.a & !mask) | result;
            self.set_flag(FLAG_C, !borrow_out);
            self.set_nz_m(result);
        } else {
            self.set_flag(FLAG_V, ((a ^ b) & (a ^ binary) & sign) != 0);
            self.a = (self.a & !mask) | binary;
            self.set_flag(FLAG_C, u32::from(a) >= u32::from(b) + u32::from(borrow));
            self.set_nz_m(binary);
        }
    }

    fn op_bit(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        let mask = self.mask_m();
        self.set_flag(FLAG_Z, (self.a & value & mask) == 0);
        self.set_flag(FLAG_N, value & self.nmask_m() != 0);
        self.set_flag(FLAG_V, value & (self.nmask_m() >> 1) != 0);
    }

    fn op_tsb(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        let mask = self.mask_m();
        self.set_flag(FLAG_Z, (value & self.a & mask) == 0);
        self.idle();
        let result = value | (self.a & mask);
        self.write_m(bus, ea, result);
    }

    fn op_trb(&mut self, bus: &mut impl Bus, ea: Effective) {
        let value = self.read_m(bus, ea);
        let mask = self.mask_m();
        self.set_flag(FLAG_Z, (value & self.a & mask) == 0);
        self.idle();
        let result = value & !(self.a & mask);
        self.write_m(bus, ea, result);
    }

    fn op_asl_a(&mut self) {
        let mask = self.mask_m();
        let value = self.a & mask;
        self.set_flag(FLAG_C, value & self.nmask_m() != 0);
        self.a = (self.a & !mask) | ((value << 1) & mask);
        self.set_nz_m(self.a);
    }

    fn op_lsr_a(&mut self) {
        let mask = self.mask_m();
        let value = self.a & mask;
        self.set_flag(FLAG_C, value & 1 != 0);
        self.a = (self.a & !mask) | ((value >> 1) & mask);
        self.set_nz_m(self.a);
    }

    fn op_rol_a(&mut self) {
        let mask = self.mask_m();
        let value = self.a & mask;
        let carry = u16::from(self.flag(FLAG_C));
        self.set_flag(FLAG_C, value & self.nmask_m() != 0);
        self.a = (self.a & !mask) | (((value << 1) | carry) & mask);
        self.set_nz_m(self.a);
    }

    fn op_ror_a(&mut self) {
        let mask = self.mask_m();
        let value = self.a & mask;
        let carry = if self.flag(FLAG_C) { self.nmask_m() } else { 0 };
        self.set_flag(FLAG_C, value & 1 != 0);
        self.a = (self.a & !mask) | ((value >> 1) | carry);
        self.set_nz_m(self.a);
    }

    fn op_inc_a(&mut self) {
        let mask = self.mask_m();
        let value = (self.a & mask).wrapping_add(1) & mask;
        self.a = (self.a & !mask) | value;
        self.set_nz_m(self.a);
    }

    fn op_dec_a(&mut self) {
        let mask = self.mask_m();
        let value = (self.a & mask).wrapping_sub(1) & mask;
        self.a = (self.a & !mask) | value;
        self.set_nz_m(self.a);
    }

    fn op_asl(&mut self, bus: &mut impl Bus, mut ea: Effective) {
        ea.read_penalty = false;
        let value = self.read_m(bus, ea);
        let mask = self.mask_m();
        let cut = self.nmask_m();
        self.set_flag(FLAG_C, value & cut != 0);
        self.idle();
        self.write_m(bus, ea, (value << 1) & mask);
        self.set_nz_m((value << 1) & mask);
    }

    fn op_lsr(&mut self, bus: &mut impl Bus, mut ea: Effective) {
        ea.read_penalty = false;
        let value = self.read_m(bus, ea);
        self.set_flag(FLAG_C, value & 1 != 0);
        self.idle();
        self.write_m(bus, ea, value >> 1);
        self.set_nz_m(value >> 1);
    }

    fn op_rol(&mut self, bus: &mut impl Bus, mut ea: Effective) {
        ea.read_penalty = false;
        let value = self.read_m(bus, ea);
        let mask = self.mask_m();
        let carry = u16::from(self.flag(FLAG_C));
        self.set_flag(FLAG_C, value & self.nmask_m() != 0);
        self.idle();
        let result = ((value << 1) | carry) & mask;
        self.write_m(bus, ea, result);
        self.set_nz_m(result);
    }

    fn op_ror(&mut self, bus: &mut impl Bus, mut ea: Effective) {
        ea.read_penalty = false;
        let value = self.read_m(bus, ea);
        let carry = if self.flag(FLAG_C) { self.nmask_m() } else { 0 };
        self.set_flag(FLAG_C, value & 1 != 0);
        self.idle();
        let result = (value >> 1) | carry;
        self.write_m(bus, ea, result);
        self.set_nz_m(result);
    }

    fn op_inc_mem(&mut self, bus: &mut impl Bus, mut ea: Effective) {
        ea.read_penalty = false;
        let value = self.read_m(bus, ea);
        self.idle();
        let mask = self.mask_m();
        let result = value.wrapping_add(1) & mask;
        self.write_m(bus, ea, result);
        self.set_nz_m(result);
    }

    fn op_dec_mem(&mut self, bus: &mut impl Bus, mut ea: Effective) {
        ea.read_penalty = false;
        let value = self.read_m(bus, ea);
        self.idle();
        let mask = self.mask_m();
        let result = value.wrapping_sub(1) & mask;
        self.write_m(bus, ea, result);
        self.set_nz_m(result);
    }

    fn op_rts(&mut self, bus: &mut impl Bus) {
        self.idle();
        let target = self.pull_word(bus);
        self.idle();
        self.idle();
        self.pc = target.wrapping_add(1);
    }

    fn op_rtl(&mut self, bus: &mut impl Bus) {
        self.idle();
        let target = {
            let lo = u16::from(self.pull_raw(bus));
            let hi = u16::from(self.pull_raw(bus));
            lo | hi << 8
        };
        let bank = self.pull_raw(bus);
        self.idle();
        self.pbr = bank;
        self.pc = target.wrapping_add(1);
    }

    fn op_rti(&mut self, bus: &mut impl Bus) {
        self.idle();
        let p = self.pull(bus);
        self.set_p_from_stack(p);
        let target = self.pull_word(bus);
        self.idle();
        if !self.e {
            let bank = self.pull(bus);
            self.pbr = bank;
        }
        self.pc = target;
    }

    fn set_p_from_stack(&mut self, value: u8) {
        self.p = value;
        self.normalize_widths();
    }

    fn op_block_move(&mut self, bus: &mut impl Bus, negative: bool) {
        let dst_bank = self.fetch_byte(bus);
        let src_bank = self.fetch_byte(bus);
        self.dbr = dst_bank;
        let mut first = true;

        loop {
            if !first {
                // The instruction stream is re-read on every iteration.
                self.read(bus, self.pbr, self.pc.wrapping_sub(3));
                self.read(bus, self.pbr, self.pc.wrapping_sub(2));
                self.read(bus, self.pbr, self.pc.wrapping_sub(1));
            }
            first = false;

            let value = self.read(bus, src_bank, self.x);
            self.write(bus, dst_bank, self.y, value);
            self.idle();
            self.idle();

            if negative {
                self.x = self.index(self.x.wrapping_sub(1));
                self.y = self.index(self.y.wrapping_sub(1));
            } else {
                self.x = self.index(self.x.wrapping_add(1));
                self.y = self.index(self.y.wrapping_add(1));
            }
            self.a = self.a.wrapping_sub(1);

            if self.a == 0xffff {
                break;
            }
        }
    }

    /* ---------- Interrupts ---------- */

    fn interrupt(&mut self, bus: &mut impl Bus, vector: Vector, software: bool) {
        if !software {
            self.idle();
            self.idle();
        }
        let p = self.p();
        let pc = self.pc;
        if !self.e {
            let bank = self.pbr;
            self.push_interrupt(bus, bank);
        }
        self.push_interrupt(bus, (pc >> 8) as u8);
        self.push_interrupt(bus, pc as u8);
        // In emulation mode bit 4 of the stacked status is the ghost B flag:
        // hardware interrupts push 0, BRK/PHP push 1. In native mode the
        // status byte is pushed verbatim (bit 4 is the X index-width flag).
        let pushed = if !software && self.e { p & !0x10 } else { p };
        self.push_interrupt(bus, pushed);
        self.set_flag(FLAG_I, true);
        self.set_flag(FLAG_D, false);
        self.pbr = 0;
        if self.e {
            self.pc = self.read_vector(bus, vector.emulation_address());
            self.set_flag(FLAG_M, true);
            self.set_flag(FLAG_X, true);
        } else {
            self.pc = self.read_vector(bus, vector.native_address());
        }
        self.waiting = false;
        self.stopped = false;
    }

    fn read_vector(&mut self, bus: &mut impl Bus, address: u16) -> u16 {
        let lo = u16::from(self.read(bus, 0, address));
        let hi = u16::from(self.read(bus, 0, address.wrapping_add(1)));
        lo | hi << 8
    }
}

/// BCD addition on one 8- or 16-bit value.
///
/// Each nibble is added with the incoming carry, adjusted by 6 when it exceeds
/// 9, and the outgoing carry feeds the next nibble. This mirrors the 65816's
/// decimal adder and matches the single-step vectors for invalid BCD inputs too.
fn decimal_add(a: u16, b: u16, carry: u16, mask: u16) -> (u16, bool, bool) {
    let nibbles = if mask == 0x00ff { 2 } else { 4 };
    let mut result = 0u16;
    let mut carry_in = carry;
    let mut top_carry_in = false;

    for index in 0..nibbles {
        if index == nibbles - 1 {
            top_carry_in = carry_in != 0;
        }
        let shift = index * 4;
        let digit_a = (a >> shift) & 0x0f;
        let digit_b = (b >> shift) & 0x0f;
        let mut digit = digit_a + digit_b + carry_in;
        if digit > 9 {
            digit += 6;
        }
        carry_in = u16::from(digit > 0x0f);
        result |= (digit & 0x0f) << shift;
    }

    (result & mask, carry_in != 0, top_carry_in)
}

/// BCD subtraction on one 8- or 16-bit value, borrowing per nibble.
fn decimal_sub(a: u16, b: u16, borrow: u16, mask: u16) -> (u16, bool, bool) {
    let nibbles = if mask == 0x00ff { 2 } else { 4 };
    let mut result = 0u16;
    let mut borrow_in = borrow;
    let mut top_borrow_in = false;

    for index in 0..nibbles {
        if index == nibbles - 1 {
            top_borrow_in = borrow_in != 0;
        }
        let shift = index * 4;
        let digit_a = (a >> shift) & 0x0f;
        let digit_b = (b >> shift) & 0x0f;
        let mut digit = digit_a.wrapping_sub(digit_b).wrapping_sub(borrow_in);
        if digit & 0x10 != 0 {
            digit = digit.wrapping_sub(6) & 0x0f;
            borrow_in = 1;
        } else {
            borrow_in = 0;
        }
        result |= (digit & 0x0f) << shift;
    }

    (result & mask, borrow_in != 0, top_borrow_in)
}

/// Decimal-mode overflow for ADC.
///
/// Hardware evaluates the sign of the high nibble *before* the +6 correction,
/// using the decimal carry out of the previous nibble. The single-step vectors
/// distinguish this from a plain binary overflow.
fn decimal_overflow_add(a: u16, b: u16, carry: u16, mask: u16) -> bool {
    let (top_a, top_b, top_carry) = if mask == 0x00ff {
        (a & 0x00ff, b & 0x00ff, carry)
    } else {
        let low = (a & 0x0f) + (b & 0x0f) + carry;
        let carry0 = u16::from(low > 9);
        let mid = ((a >> 4) & 0x0f) + ((b >> 4) & 0x0f) + carry0;
        let carry1 = u16::from(mid > 9);
        ((a >> 8) & 0x00ff, (b >> 8) & 0x00ff, carry1)
    };

    let low = (top_a & 0x0f) + (top_b & 0x0f) + top_carry;
    let carry0 = u16::from(low > 9);
    let high = (top_a >> 4) + (top_b >> 4) + carry0;

    let result_sign = (high >> 3) & 1;
    let a_sign = (top_a >> 7) & 1;
    let b_sign = (top_b >> 7) & 1;
    a_sign == b_sign && result_sign != a_sign
}

/// Decimal-mode overflow for SBC, using the decimal borrow chain.
fn decimal_overflow_sub(a: u16, b: u16, borrow: u16, mask: u16) -> bool {
    let (top_a, top_b, top_borrow) = if mask == 0x00ff {
        (a & 0x00ff, b & 0x00ff, borrow)
    } else {
        let low = (a & 0x0f).wrapping_sub(b & 0x0f).wrapping_sub(borrow);
        let borrow0 = u16::from(low & 0x10 != 0);
        let mid = ((a >> 4) & 0x0f)
            .wrapping_sub((b >> 4) & 0x0f)
            .wrapping_sub(borrow0);
        let borrow1 = u16::from(mid & 0x10 != 0);
        ((a >> 8) & 0x00ff, (b >> 8) & 0x00ff, borrow1)
    };

    let low = (top_a & 0x0f)
        .wrapping_sub(top_b & 0x0f)
        .wrapping_sub(top_borrow);
    let borrow0 = u16::from(low & 0x10 != 0);
    let high = (top_a >> 4).wrapping_sub(top_b >> 4).wrapping_sub(borrow0);

    let result_sign = (high >> 3) & 1;
    let a_sign = (top_a >> 7) & 1;
    let b_sign = (top_b >> 7) & 1;
    a_sign != b_sign && result_sign != a_sign
}

/// Interrupt vectors differ between emulation and native mode.
#[derive(Clone, Copy, Debug)]
enum Vector {
    Nmi,
    Irq,
    Brk,
    Cop,
}

impl Vector {
    const fn emulation_address(self) -> u16 {
        match self {
            Self::Nmi => 0xFFFA,
            Self::Irq | Self::Brk => 0xFFFE,
            Self::Cop => 0xFFF4,
        }
    }

    const fn native_address(self) -> u16 {
        match self {
            Self::Nmi => 0xFFEA,
            Self::Irq => 0xFFEE,
            Self::Brk => 0xFFE6,
            Self::Cop => 0xFFE4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::FlatMemory;

    fn native_cpu(p: u8) -> Cpu {
        let mut cpu = Cpu::new();
        cpu.set_registers(Registers {
            a: 0,
            x: 0,
            y: 0,
            s: 0x01ff,
            d: 0,
            pc: 0,
            pbr: 0,
            dbr: 0,
            p,
            e: false,
        });
        cpu
    }

    fn run(cpu: &mut Cpu, bus: &mut FlatMemory, steps: usize) {
        for _ in 0..steps {
            cpu.step(bus);
        }
    }

    #[test]
    fn sixteen_bit_compare_takes_n_from_bit_15() {
        let mut cpu = native_cpu(FLAG_I);
        let mut bus = FlatMemory::new();
        // LDA #$00E8 ; CMP #$000F -> $00D9, bit 7 set but bit 15 clear.
        bus.load(&[0xa9, 0xe8, 0x00, 0xc9, 0x0f, 0x00]);
        run(&mut cpu, &mut bus, 2);

        assert!(!cpu.registers().p & FLAG_N != 0);
        assert!(!cpu.registers().p & FLAG_Z != 0);
        assert!(cpu.registers().p & FLAG_C != 0);

        // LDA #$8000 ; CMP #$0001 -> $7FFF, bit 15 clear.
        let mut cpu = native_cpu(FLAG_I);
        let mut bus = FlatMemory::new();
        bus.load(&[0xa9, 0x00, 0x80, 0xc9, 0x01, 0x00]);
        run(&mut cpu, &mut bus, 2);
        assert!(!cpu.registers().p & FLAG_N != 0);

        // LDA #$0001 ; CMP #$8000 -> $8001, bit 15 set.
        let mut cpu = native_cpu(FLAG_I);
        let mut bus = FlatMemory::new();
        bus.load(&[0xa9, 0x01, 0x00, 0xc9, 0x00, 0x80]);
        run(&mut cpu, &mut bus, 2);
        assert!(cpu.registers().p & FLAG_N != 0);
    }

    #[test]
    fn absolute_indexed_read_pays_a_cycle_for_a_16_bit_index() {
        let mut bus = FlatMemory::new();
        // LDA $00F0,X with X = $0020: the low bytes ($F0 + $20) cross a page.
        bus.load(&[0xbd, 0xf0, 0x00]);
        let mut cpu = native_cpu(FLAG_I | FLAG_M);
        cpu.set_registers(Registers {
            x: 0x0020,
            pc: 0,
            pbr: 0,
            dbr: 0,
            p: FLAG_I | FLAG_M,
            s: 0x01ff,
            ..cpu.registers()
        });
        cpu.set_cycles(0);
        cpu.step(&mut bus);
        assert_eq!(cpu.cycles(), 5);

        // X = $0010 keeps both bytes inside page $00, but the 16-bit index
        // register still costs the extra cycle.
        let mut bus = FlatMemory::new();
        bus.load(&[0xbd, 0xf0, 0x00]);
        let mut cpu = native_cpu(FLAG_I | FLAG_M);
        cpu.set_registers(Registers {
            x: 0x0010,
            pc: 0,
            pbr: 0,
            dbr: 0,
            p: FLAG_I | FLAG_M,
            s: 0x01ff,
            ..cpu.registers()
        });
        cpu.set_cycles(0);
        cpu.step(&mut bus);
        assert_eq!(cpu.cycles(), 5);
    }

    #[test]
    fn eight_bit_index_only_pays_on_a_page_cross() {
        let mut bus = FlatMemory::new();
        // LDA $00F0,X with 8-bit X = $0F: no page cross, 4 cycles.
        bus.load(&[0xbd, 0xf0, 0x00]);
        let mut cpu = native_cpu(FLAG_I | FLAG_X | FLAG_M);
        cpu.set_registers(Registers {
            x: 0x000f,
            pc: 0,
            pbr: 0,
            dbr: 0,
            p: FLAG_I | FLAG_X | FLAG_M,
            s: 0x01ff,
            ..cpu.registers()
        });
        cpu.set_cycles(0);
        cpu.step(&mut bus);
        assert_eq!(cpu.cycles(), 4);

        let mut bus = FlatMemory::new();
        bus.load(&[0xbd, 0xf0, 0x00]);
        let mut cpu = native_cpu(FLAG_I | FLAG_X | FLAG_M);
        cpu.set_registers(Registers {
            x: 0x0020,
            pc: 0,
            pbr: 0,
            dbr: 0,
            p: FLAG_I | FLAG_X | FLAG_M,
            s: 0x01ff,
            ..cpu.registers()
        });
        cpu.set_cycles(0);
        cpu.step(&mut bus);
        assert_eq!(cpu.cycles(), 5);
    }

    #[test]
    fn sixteen_bit_accumulator_push_and_pull() {
        let mut cpu = native_cpu(FLAG_I);
        let mut bus = FlatMemory::new();
        bus.load(&[
            0xa9, 0x34, 0x12, // LDA #$1234
            0x48, // PHA
            0xa9, 0x00, 0x00, // LDA #$0000
            0x68, // PLA
            0xdb, // STP
        ]);

        run(&mut cpu, &mut bus, 5);

        assert_eq!(cpu.registers().a, 0x1234);
        assert_eq!(cpu.registers().s, 0x01ff);
    }

    #[test]
    fn sixteen_bit_index_push_and_pull() {
        let mut cpu = native_cpu(FLAG_I | FLAG_M);
        let mut bus = FlatMemory::new();
        bus.load(&[
            0xa2, 0x78, 0x56, // LDX #$5678
            0xda, // PHX
            0xa2, 0x00, 0x00, // LDX #$0000
            0xfa, // PLX
            0xa0, 0xcd, 0xab, // LDY #$ABCD
            0x5a, // PHY
            0xa0, 0x00, 0x00, // LDY #$0000
            0x7a, // PLY
            0xdb, // STP
        ]);

        run(&mut cpu, &mut bus, 9);

        assert_eq!(cpu.registers().x, 0x5678);
        assert_eq!(cpu.registers().y, 0xabcd);
        assert_eq!(cpu.registers().s, 0x01ff);
    }

    #[test]
    fn push_and_pull_direct_page_use_two_bytes() {
        let mut cpu = native_cpu(FLAG_I);
        let mut bus = FlatMemory::new();
        bus.load(&[
            0xa9, 0x21, 0x43, // LDA #$4321
            0x5b, // TCD
            0x0b, // PHD
            0xa9, 0x00, 0x00, // LDA #$0000
            0x5b, // TCD
            0x2b, // PLD
            0xdb, // STP
        ]);

        run(&mut cpu, &mut bus, 6);

        assert_eq!(cpu.registers().d, 0x4321);
        assert_eq!(cpu.registers().s, 0x01ff);
    }

    #[test]
    fn native_interrupt_preserves_index_width_in_stacked_status() {
        let mut cpu = native_cpu(FLAG_I | FLAG_M | FLAG_X);
        let mut bus = FlatMemory::new();
        bus.write(0, 0xffea, 0x00);
        bus.write(0, 0xffeb, 0x80);

        cpu.set_nmi(true);
        cpu.step(&mut bus);

        // P is stacked below PCL/PCH/PBR; after the push S points below it.
        assert_eq!(bus.read(0, 0x01fc), FLAG_I | FLAG_M | FLAG_X);
    }

    #[test]
    fn emulation_interrupt_clears_the_ghost_break_flag() {
        let mut cpu = Cpu::new();
        let mut bus = FlatMemory::new();
        bus.write(0, 0xfffa, 0x00);
        bus.write(0, 0xfffb, 0x80);

        cpu.set_nmi(true);
        cpu.step(&mut bus);

        assert_eq!(bus.read(0, 0x01fd), FLAG_I | FLAG_M);
    }
}
