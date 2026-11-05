//! The SPC700 audio CPU.
//!
//! Instruction encodings and base cycles come from the published SPC700
//! reference tables; the interpreter itself is a plain match over the 256
//! opcodes. Direct-page addressing is relative to `$0000` or `$0100` depending
//! on the P flag, as on hardware.

use crate::ApuBus;

const PSW_N: u8 = 0x80;
const PSW_V: u8 = 0x40;
const PSW_P: u8 = 0x20;
const PSW_B: u8 = 0x10;
const PSW_H: u8 = 0x08;
const PSW_I: u8 = 0x04;
const PSW_Z: u8 = 0x02;
const PSW_C: u8 = 0x01;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Alu {
    Adc,
    Sbc,
    Cmp,
    And,
    Or,
    Eor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shift {
    Asl,
    Lsr,
    Rol,
    Ror,
}

/// SPC700 register file.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Spc700 {
    /// Accumulator.
    pub a: u8,
    /// Index X.
    pub x: u8,
    /// Index Y.
    pub y: u8,
    /// Stack pointer.
    pub sp: u8,
    /// Program counter.
    pub pc: u16,
    /// Processor status word.
    pub psw: u8,
    /// Idle after `SLEEP` or `STOP`.
    pub stopped: bool,
}

impl Default for Spc700 {
    fn default() -> Self {
        Self::new()
    }
}

impl Spc700 {
    /// Create a CPU in its post-reset state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0,
            pc: 0,
            psw: 0,
            stopped: false,
        }
    }

    /// Reset registers and fetch the entry point (the IPL hands off at `$0200`).
    pub fn reset(&mut self, entry: u16) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0xEF;
        self.pc = entry;
        self.psw = 0;
        self.stopped = false;
    }

    /// Execute one instruction and return its cycle cost.
    pub fn step(&mut self, bus: &mut impl ApuBus) -> u32 {
        if self.stopped {
            return 1;
        }

        let opcode = self.fetch(bus);
        self.execute(opcode, bus)
    }

    /* ---------- Memory ---------- */

    fn read(&self, bus: &mut impl ApuBus, addr: u16) -> u8 {
        bus.read(addr)
    }

    fn write(&mut self, bus: &mut impl ApuBus, addr: u16, value: u8) {
        bus.write(addr, value);
    }

    fn fetch(&mut self, bus: &mut impl ApuBus) -> u8 {
        let value = self.read(bus, self.pc);
        self.pc = self.pc.wrapping_add(1);
        value
    }

    fn imm(&mut self, bus: &mut impl ApuBus) -> u8 {
        self.fetch(bus)
    }

    fn fetch16(&mut self, bus: &mut impl ApuBus) -> u16 {
        let lo = u16::from(self.fetch(bus));
        let hi = u16::from(self.fetch(bus));
        lo | hi << 8
    }

    fn dp_base(&self) -> u16 {
        if self.psw & PSW_P != 0 {
            0x0100
        } else {
            0x0000
        }
    }

    fn addr_dp(&mut self, bus: &mut impl ApuBus) -> u16 {
        self.dp_base() + u16::from(self.fetch(bus))
    }

    fn addr_dp_x(&mut self, bus: &mut impl ApuBus) -> u16 {
        self.dp_base() + u16::from(self.fetch(bus).wrapping_add(self.x))
    }

    fn addr_dp_y(&mut self, bus: &mut impl ApuBus) -> u16 {
        self.dp_base() + u16::from(self.fetch(bus).wrapping_add(self.y))
    }

    fn addr_abs(&mut self, bus: &mut impl ApuBus) -> u16 {
        self.fetch16(bus)
    }

    fn addr_abs_x(&mut self, bus: &mut impl ApuBus) -> u16 {
        let base = self.fetch16(bus);
        base.wrapping_add(u16::from(self.x))
    }

    fn addr_abs_y(&mut self, bus: &mut impl ApuBus) -> u16 {
        let base = self.fetch16(bus);
        base.wrapping_add(u16::from(self.y))
    }

    fn ind_x(&self) -> u16 {
        self.dp_base() + u16::from(self.x)
    }

    fn ind_y(&self) -> u16 {
        self.dp_base() + u16::from(self.y)
    }

    /// Read a 16-bit pointer from the direct page. The high byte wraps inside
    /// the current direct page (Anomie: "direct page accesses will wrap within
    /// the direct page").
    fn read_pointer_dp(&mut self, bus: &mut impl ApuBus, pointer: u16) -> u16 {
        let high = (pointer & 0xFF00) | ((pointer + 1) & 0x00FF);
        u16::from(self.read(bus, pointer)) | (u16::from(self.read(bus, high)) << 8)
    }

    fn dp_x_ptr(&mut self, bus: &mut impl ApuBus) -> u16 {
        let pointer = self.addr_dp_x(bus);
        self.read_pointer_dp(bus, pointer)
    }

    fn dp_y_ptr(&mut self, bus: &mut impl ApuBus) -> u16 {
        let pointer = self.addr_dp(bus);
        let base = self.read_pointer_dp(bus, pointer);
        base.wrapping_add(u16::from(self.y))
    }

    fn read_word(&mut self, bus: &mut impl ApuBus, addr: u16) -> u16 {
        u16::from(self.read(bus, addr)) | (u16::from(self.read(bus, addr.wrapping_add(1))) << 8)
    }

    fn push(&mut self, bus: &mut impl ApuBus, value: u8) {
        self.write(bus, 0x0100 | u16::from(self.sp), value);
        self.sp = self.sp.wrapping_sub(1);
    }

    fn pull(&mut self, bus: &mut impl ApuBus) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.read(bus, 0x0100 | u16::from(self.sp))
    }

    /* ---------- Flags ---------- */

    fn flag(&self, flag: u8) -> bool {
        self.psw & flag != 0
    }

    fn set_flag(&mut self, flag: u8, value: bool) {
        if value {
            self.psw |= flag;
        } else {
            self.psw &= !flag;
        }
    }

    fn set_nz(&mut self, value: u8) {
        self.set_flag(PSW_N, value & 0x80 != 0);
        self.set_flag(PSW_Z, value == 0);
    }

    fn set_nz16(&mut self, value: u16) {
        self.set_flag(PSW_N, value & 0x8000 != 0);
        self.set_flag(PSW_Z, value == 0);
    }

    /// Read a word from the direct page, wrapping inside the page.
    fn read_word_dp(&mut self, bus: &mut impl ApuBus, addr: u16) -> u16 {
        let low = u16::from(self.read(bus, addr));
        let high = u16::from(self.read(bus, (addr & 0xFF00) | ((addr + 1) & 0x00FF)));
        low | high << 8
    }

    /// Write a word to the direct page, wrapping inside the page.
    fn write_word_dp(&mut self, bus: &mut impl ApuBus, addr: u16, value: u16) {
        self.write(bus, addr, value as u8);
        self.write(
            bus,
            (addr & 0xFF00) | ((addr + 1) & 0x00FF),
            (value >> 8) as u8,
        );
    }

    fn ya(&self) -> u16 {
        u16::from(self.a) | (u16::from(self.y) << 8)
    }

    fn set_ya(&mut self, value: u16) {
        self.a = value as u8;
        self.y = (value >> 8) as u8;
        self.set_nz16(value);
    }

    /* ---------- Operations ---------- */

    fn alu(&mut self, op: Alu, left: u8, right: u8) -> Option<u8> {
        match op {
            Alu::Adc => {
                let carry = u16::from(self.flag(PSW_C));
                let sum = u16::from(left) + u16::from(right) + carry;
                let result = sum as u8;
                self.set_flag(PSW_H, (left & 0x0F) + (right & 0x0F) + carry as u8 > 0x0F);
                self.set_flag(PSW_V, (!(left ^ right) & (left ^ result) & 0x80) != 0);
                self.set_flag(PSW_C, sum > 0xFF);
                self.set_nz(result);
                Some(result)
            }
            Alu::Sbc => {
                let borrow = 1 - u16::from(self.flag(PSW_C));
                let diff = u16::from(left)
                    .wrapping_sub(u16::from(right))
                    .wrapping_sub(borrow);
                let result = diff as u8;
                self.set_flag(PSW_H, (left & 0x0F) >= (right & 0x0F) + borrow as u8);
                self.set_flag(PSW_V, ((left ^ right) & (left ^ result) & 0x80) != 0);
                self.set_flag(PSW_C, u16::from(left) >= u16::from(right) + borrow);
                self.set_nz(result);
                Some(result)
            }
            Alu::Cmp => {
                self.set_flag(PSW_C, left >= right);
                self.set_nz(left.wrapping_sub(right));
                None
            }
            Alu::And => {
                let result = left & right;
                self.set_nz(result);
                Some(result)
            }
            Alu::Or => {
                let result = left | right;
                self.set_nz(result);
                Some(result)
            }
            Alu::Eor => {
                let result = left ^ right;
                self.set_nz(result);
                Some(result)
            }
        }
    }

    fn shift(&mut self, op: Shift, value: u8) -> u8 {
        match op {
            Shift::Asl => {
                self.set_flag(PSW_C, value & 0x80 != 0);
                let result = value << 1;
                self.set_nz(result);
                result
            }
            Shift::Lsr => {
                self.set_flag(PSW_C, value & 0x01 != 0);
                let result = value >> 1;
                self.set_nz(result);
                result
            }
            Shift::Rol => {
                let carry = u8::from(self.flag(PSW_C));
                self.set_flag(PSW_C, value & 0x80 != 0);
                let result = (value << 1) | carry;
                self.set_nz(result);
                result
            }
            Shift::Ror => {
                let carry = if self.flag(PSW_C) { 0x80 } else { 0 };
                self.set_flag(PSW_C, value & 0x01 != 0);
                let result = (value >> 1) | carry;
                self.set_nz(result);
                result
            }
        }
    }

    fn branch(&mut self, bus: &mut impl ApuBus, condition: bool) -> u32 {
        self.branch_cycles(bus, condition, 4, 2)
    }

    fn branch_cycles(
        &mut self,
        bus: &mut impl ApuBus,
        condition: bool,
        taken: u32,
        not_taken: u32,
    ) -> u32 {
        let offset = self.fetch(bus) as i8;
        if condition {
            self.pc = self.pc.wrapping_add_signed(i16::from(offset));
            taken
        } else {
            not_taken
        }
    }

    fn inc(&mut self, value: u8) -> u8 {
        let result = value.wrapping_add(1);
        self.set_nz(result);
        result
    }

    fn dec(&mut self, value: u8) -> u8 {
        let result = value.wrapping_sub(1);
        self.set_nz(result);
        result
    }

    fn bit_mask(bit: u8) -> u8 {
        1 << (bit & 7)
    }

    /* ---------- Dispatch ---------- */

    #[allow(clippy::too_many_lines, clippy::possible_missing_else)]
    fn execute(&mut self, opcode: u8, bus: &mut impl ApuBus) -> u32 {
        match opcode {
            0x00 => 2, // NOP
            0x01 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(0));
                8
            } // TCALL n
            0x02 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(0);
                let value = self.read(bus, addr) | mask;
                self.write(bus, addr, value);
                4
            } // SET1 d.bit
            0x03 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(0);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, set, 7, 5)
            } // BBS d.bit, r
            0x04 => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.a = result;
                };
                3
            } // OR A, d
            0x05 => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.a = result;
                };
                4
            } // OR A, !a
            0x06 => {
                let value = self.read(bus, self.ind_x());
                let left = self.a;
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.a = result;
                };
                3
            } // OR A, (X)
            0x07 => {
                let addr = self.dp_x_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.a = result;
                };
                6
            } // OR A, [d+X]
            0x08 => {
                let value = self.imm(bus);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.a = result;
                };
                2
            } // OR A, #i
            0x09 => {
                let source = self.addr_dp(bus);
                let destination = self.addr_dp(bus);
                let left = self.read(bus, destination);
                let right = self.read(bus, source);
                if let Some(result) = self.alu(Alu::Or, left, right) {
                    self.write(bus, destination, result);
                };
                6
            } // OR dd, ds
            0x0A => {
                let low = self.fetch(bus);
                let high = self.fetch(bus);
                let addr = u16::from(low) | (u16::from(high & 0x1F) << 8);
                let bit = high >> 5;
                let bit_value = self.read(bus, addr) & Self::bit_mask(bit) != 0;
                self.set_flag(PSW_C, self.flag(PSW_C) || bit_value);
                5
            } // OR1 C, m.b
            0x0B => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Asl, value);
                self.write(bus, addr, result);
                4
            } // ASL d
            0x0C => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Asl, value);
                self.write(bus, addr, result);
                5
            } // ASL !a
            0x0D => {
                let value = self.psw;
                self.push(bus, value);
                4
            } // PUSH PSW
            0x0E => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                self.set_nz(self.a.wrapping_sub(value));
                let result = value | self.a;
                self.write(bus, addr, result);
                6
            } // TSET1 !a
            0x0F => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.push(bus, self.psw);
                self.set_flag(PSW_B, true);
                self.set_flag(PSW_I, false);
                self.pc = self.read_word(bus, 0xFFDE);
                8
            } // BRK
            0x10 => self.branch(bus, !self.flag(PSW_N)), // BPL r
            0x11 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(2));
                8
            } // TCALL n
            0x12 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(0);
                let value = self.read(bus, addr) & !mask;
                self.write(bus, addr, value);
                4
            } // CLR1 d.bit
            0x13 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(0);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, !set, 7, 5)
            } // BBC d.bit, r
            0x14 => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.a = result;
                };
                4
            } // OR A, d+X
            0x15 => {
                let addr = self.addr_abs_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.a = result;
                };
                5
            } // OR A, !a+X
            0x16 => {
                let addr = self.addr_abs_y(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.a = result;
                };
                5
            } // OR A, !a+Y
            0x17 => {
                let addr = self.dp_y_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.a = result;
                };
                6
            } // OR A, [d]+Y
            0x18 => {
                let value = self.imm(bus);
                let addr = self.addr_dp(bus);
                let left = self.read(bus, addr);
                if let Some(result) = self.alu(Alu::Or, left, value) {
                    self.write(bus, addr, result);
                };
                5
            } // OR d, #i
            0x19 => {
                let left_addr = self.ind_x();
                let right_addr = self.ind_y();
                let left = self.read(bus, left_addr);
                let right = self.read(bus, right_addr);
                if let Some(result) = self.alu(Alu::Or, left, right) {
                    self.write(bus, left_addr, result);
                };
                5
            } // OR (X), (Y)
            0x1A => {
                let addr = self.addr_dp(bus);
                let value = self.read_word_dp(bus, addr).wrapping_sub(1);
                self.write_word_dp(bus, addr, value);
                self.set_nz16(value);
                6
            } // DECW d
            0x1B => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Asl, value);
                self.write(bus, addr, result);
                5
            } // ASL d+X
            0x1C => {
                let result = self.shift(Shift::Asl, self.a);
                self.a = result;
                2
            } // ASL A
            0x1D => {
                self.x = self.dec(self.x);
                2
            } // DEC X
            0x1E => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                self.set_flag(PSW_C, self.x >= value);
                self.set_nz(self.x.wrapping_sub(value));
                4
            } // CMP X, !a
            0x1F => {
                let base = self.addr_abs(bus);
                self.pc = self.read_word(bus, base.wrapping_add(u16::from(self.x)));
                6
            } // JMP [!a+X]
            0x20 => {
                self.set_flag(PSW_P, false);
                2
            } // CLRP
            0x21 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(4));
                8
            } // TCALL n
            0x22 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(1);
                let value = self.read(bus, addr) | mask;
                self.write(bus, addr, value);
                4
            } // SET1 d.bit
            0x23 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(1);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, set, 7, 5)
            } // BBS d.bit, r
            0x24 => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.a = result;
                };
                3
            } // AND A, d
            0x25 => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.a = result;
                };
                4
            } // AND A, !a
            0x26 => {
                let value = self.read(bus, self.ind_x());
                let left = self.a;
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.a = result;
                };
                3
            } // AND A, (X)
            0x27 => {
                let addr = self.dp_x_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.a = result;
                };
                6
            } // AND A, [d+X]
            0x28 => {
                let value = self.imm(bus);
                let left = self.a;
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.a = result;
                };
                2
            } // AND A, #i
            0x29 => {
                let source = self.addr_dp(bus);
                let destination = self.addr_dp(bus);
                let left = self.read(bus, destination);
                let right = self.read(bus, source);
                if let Some(result) = self.alu(Alu::And, left, right) {
                    self.write(bus, destination, result);
                };
                6
            } // AND dd, ds
            0x2A => {
                let low = self.fetch(bus);
                let high = self.fetch(bus);
                let addr = u16::from(low) | (u16::from(high & 0x1F) << 8);
                let bit = high >> 5;
                let bit_value = self.read(bus, addr) & Self::bit_mask(bit) != 0;
                self.set_flag(PSW_C, self.flag(PSW_C) || !bit_value);
                5
            } // OR1 C, /m.b
            0x2B => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Rol, value);
                self.write(bus, addr, result);
                4
            } // ROL d
            0x2C => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Rol, value);
                self.write(bus, addr, result);
                5
            } // ROL !a
            0x2D => {
                let value = self.a;
                self.push(bus, value);
                4
            } // PUSH A
            0x2E => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                self.branch_cycles(bus, self.a != value, 7, 5)
            } // CBNE d, r
            0x2F => self.branch(bus, true), // BRA r
            0x30 => self.branch(bus, self.flag(PSW_N)), // BMI r
            0x31 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(6));
                8
            } // TCALL n
            0x32 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(1);
                let value = self.read(bus, addr) & !mask;
                self.write(bus, addr, value);
                4
            } // CLR1 d.bit
            0x33 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(1);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, !set, 7, 5)
            } // BBC d.bit, r
            0x34 => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.a = result;
                };
                4
            } // AND A, d+X
            0x35 => {
                let addr = self.addr_abs_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.a = result;
                };
                5
            } // AND A, !a+X
            0x36 => {
                let addr = self.addr_abs_y(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.a = result;
                };
                5
            } // AND A, !a+Y
            0x37 => {
                let addr = self.dp_y_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.a = result;
                };
                6
            } // AND A, [d]+Y
            0x38 => {
                let value = self.imm(bus);
                let addr = self.addr_dp(bus);
                let left = self.read(bus, addr);
                if let Some(result) = self.alu(Alu::And, left, value) {
                    self.write(bus, addr, result);
                };
                5
            } // AND d, #i
            0x39 => {
                let left_addr = self.ind_x();
                let right_addr = self.ind_y();
                let left = self.read(bus, left_addr);
                let right = self.read(bus, right_addr);
                if let Some(result) = self.alu(Alu::And, left, right) {
                    self.write(bus, left_addr, result);
                };
                5
            } // AND (X), (Y)
            0x3A => {
                let addr = self.addr_dp(bus);
                let value = self.read_word_dp(bus, addr).wrapping_add(1);
                self.write_word_dp(bus, addr, value);
                self.set_nz16(value);
                6
            } // INCW d
            0x3B => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Rol, value);
                self.write(bus, addr, result);
                5
            } // ROL d+X
            0x3C => {
                let result = self.shift(Shift::Rol, self.a);
                self.a = result;
                2
            } // ROL A
            0x3D => {
                self.x = self.inc(self.x);
                2
            } // INC X
            0x3E => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                self.set_flag(PSW_C, self.x >= value);
                self.set_nz(self.x.wrapping_sub(value));
                3
            } // CMP X, d
            0x3F => {
                let target = self.addr_abs(bus);
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = target;
                8
            } // CALL !a
            0x40 => {
                self.set_flag(PSW_P, true);
                2
            } // SETP
            0x41 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(8));
                8
            } // TCALL n
            0x42 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(2);
                let value = self.read(bus, addr) | mask;
                self.write(bus, addr, value);
                4
            } // SET1 d.bit
            0x43 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(2);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, set, 7, 5)
            } // BBS d.bit, r
            0x44 => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.a = result;
                };
                3
            } // EOR A, d
            0x45 => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.a = result;
                };
                4
            } // EOR A, !a
            0x46 => {
                let value = self.read(bus, self.ind_x());
                let left = self.a;
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.a = result;
                };
                3
            } // EOR A, (X)
            0x47 => {
                let addr = self.dp_x_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.a = result;
                };
                6
            } // EOR A, [d+X]
            0x48 => {
                let value = self.imm(bus);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.a = result;
                };
                2
            } // EOR A, #i
            0x49 => {
                let source = self.addr_dp(bus);
                let destination = self.addr_dp(bus);
                let left = self.read(bus, destination);
                let right = self.read(bus, source);
                if let Some(result) = self.alu(Alu::Eor, left, right) {
                    self.write(bus, destination, result);
                };
                6
            } // EOR dd, ds
            0x4A => {
                let low = self.fetch(bus);
                let high = self.fetch(bus);
                let addr = u16::from(low) | (u16::from(high & 0x1F) << 8);
                let bit = high >> 5;
                let bit_value = self.read(bus, addr) & Self::bit_mask(bit) != 0;
                self.set_flag(PSW_C, self.flag(PSW_C) && bit_value);
                4
            } // AND1 C, m.b
            0x4B => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Lsr, value);
                self.write(bus, addr, result);
                4
            } // LSR d
            0x4C => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Lsr, value);
                self.write(bus, addr, result);
                5
            } // LSR !a
            0x4D => {
                let value = self.x;
                self.push(bus, value);
                4
            } // PUSH X
            0x4E => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                self.set_nz(self.a.wrapping_sub(value));
                let result = value & !self.a;
                self.write(bus, addr, result);
                6
            } // TCLR1 !a
            0x4F => {
                let offset = self.imm(bus);
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = 0xFF00 | u16::from(offset);
                6
            } // PCALL u
            0x50 => self.branch(bus, !self.flag(PSW_V)), // BVC r
            0x51 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(10));
                8
            } // TCALL n
            0x52 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(2);
                let value = self.read(bus, addr) & !mask;
                self.write(bus, addr, value);
                4
            } // CLR1 d.bit
            0x53 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(2);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, !set, 7, 5)
            } // BBC d.bit, r
            0x54 => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.a = result;
                };
                4
            } // EOR A, d+X
            0x55 => {
                let addr = self.addr_abs_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.a = result;
                };
                5
            } // EOR A, !a+X
            0x56 => {
                let addr = self.addr_abs_y(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.a = result;
                };
                5
            } // EOR A, !a+Y
            0x57 => {
                let addr = self.dp_y_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.a = result;
                };
                6
            } // EOR A, [d]+Y
            0x58 => {
                let value = self.imm(bus);
                let addr = self.addr_dp(bus);
                let left = self.read(bus, addr);
                if let Some(result) = self.alu(Alu::Eor, left, value) {
                    self.write(bus, addr, result);
                };
                5
            } // EOR d, #i
            0x59 => {
                let left_addr = self.ind_x();
                let right_addr = self.ind_y();
                let left = self.read(bus, left_addr);
                let right = self.read(bus, right_addr);
                if let Some(result) = self.alu(Alu::Eor, left, right) {
                    self.write(bus, left_addr, result);
                };
                5
            } // EOR (X), (Y)
            0x5A => {
                let addr = self.addr_dp(bus);
                let value = self.read_word_dp(bus, addr);
                self.set_flag(PSW_C, self.ya() >= value);
                self.set_nz16(self.ya().wrapping_sub(value));
                4
            } // CMPW YA, d
            0x5B => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Lsr, value);
                self.write(bus, addr, result);
                5
            } // LSR d+X
            0x5C => {
                let result = self.shift(Shift::Lsr, self.a);
                self.a = result;
                2
            } // LSR A
            0x5D => {
                self.x = self.a;
                self.set_nz(self.x);
                2
            } // MOV X, A
            0x5E => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                self.set_flag(PSW_C, self.y >= value);
                self.set_nz(self.y.wrapping_sub(value));
                4
            } // CMP Y, !a
            0x5F => {
                self.pc = self.addr_abs(bus);
                3
            } // JMP !a
            0x60 => {
                self.set_flag(PSW_C, false);
                2
            } // CLRC
            0x61 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(12));
                8
            } // TCALL n
            0x62 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(3);
                let value = self.read(bus, addr) | mask;
                self.write(bus, addr, value);
                4
            } // SET1 d.bit
            0x63 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(3);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, set, 7, 5)
            } // BBS d.bit, r
            0x64 => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.a = result;
                };
                3
            } // CMP A, d
            0x65 => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.a = result;
                };
                4
            } // CMP A, !a
            0x66 => {
                let value = self.read(bus, self.ind_x());
                let left = self.a;
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.a = result;
                };
                3
            } // CMP A, (X)
            0x67 => {
                let addr = self.dp_x_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.a = result;
                };
                6
            } // CMP A, [d+X]
            0x68 => {
                let value = self.imm(bus);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.a = result;
                };
                2
            } // CMP A, #i
            0x69 => {
                let source = self.addr_dp(bus);
                let destination = self.addr_dp(bus);
                let left = self.read(bus, destination);
                let right = self.read(bus, source);
                if let Some(result) = self.alu(Alu::Cmp, left, right) {
                    self.write(bus, destination, result);
                };
                6
            } // CMP dd, ds
            0x6A => {
                let low = self.fetch(bus);
                let high = self.fetch(bus);
                let addr = u16::from(low) | (u16::from(high & 0x1F) << 8);
                let bit = high >> 5;
                let bit_value = self.read(bus, addr) & Self::bit_mask(bit) != 0;
                self.set_flag(PSW_C, self.flag(PSW_C) && !bit_value);
                4
            } // AND1 C, /m.b
            0x6B => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Ror, value);
                self.write(bus, addr, result);
                4
            } // ROR d
            0x6C => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Ror, value);
                self.write(bus, addr, result);
                5
            } // ROR !a
            0x6D => {
                let value = self.y;
                self.push(bus, value);
                4
            } // PUSH Y
            0x6E => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr).wrapping_sub(1);
                self.write(bus, addr, value);
                self.branch_cycles(bus, value != 0, 7, 5)
            } // DBNZ d, r
            0x6F => {
                let low = self.pull(bus);
                let high = self.pull(bus);
                self.pc = u16::from(low) | (u16::from(high) << 8);
                5
            } // RET
            0x70 => self.branch(bus, self.flag(PSW_V)), // BVS r
            0x71 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(14));
                8
            } // TCALL n
            0x72 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(3);
                let value = self.read(bus, addr) & !mask;
                self.write(bus, addr, value);
                4
            } // CLR1 d.bit
            0x73 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(3);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, !set, 7, 5)
            } // BBC d.bit, r
            0x74 => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.a = result;
                };
                4
            } // CMP A, d+X
            0x75 => {
                let addr = self.addr_abs_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.a = result;
                };
                5
            } // CMP A, !a+X
            0x76 => {
                let addr = self.addr_abs_y(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.a = result;
                };
                5
            } // CMP A, !a+Y
            0x77 => {
                let addr = self.dp_y_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.a = result;
                };
                6
            } // CMP A, [d]+Y
            0x78 => {
                let value = self.imm(bus);
                let addr = self.addr_dp(bus);
                let left = self.read(bus, addr);
                if let Some(result) = self.alu(Alu::Cmp, left, value) {
                    self.write(bus, addr, result);
                };
                5
            } // CMP d, #i
            0x79 => {
                let left_addr = self.ind_x();
                let right_addr = self.ind_y();
                let left = self.read(bus, left_addr);
                let right = self.read(bus, right_addr);
                if let Some(result) = self.alu(Alu::Cmp, left, right) {
                    self.write(bus, left_addr, result);
                };
                5
            } // CMP (X), (Y)
            0x7A => {
                let addr = self.addr_dp(bus);
                let value = self.read_word_dp(bus, addr);
                let sum = self.ya().wrapping_add(value);
                self.set_flag(PSW_H, (self.ya() & 0x0FFF) + (value & 0x0FFF) > 0x0FFF);
                self.set_flag(PSW_C, u32::from(self.ya()) + u32::from(value) > 0xFFFF);
                self.set_flag(
                    PSW_V,
                    (!(self.ya() ^ value) & (self.ya() ^ sum) & 0x8000) != 0,
                );
                self.set_ya(sum);
                5
            } // ADDW YA, d
            0x7B => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let result = self.shift(Shift::Ror, value);
                self.write(bus, addr, result);
                5
            } // ROR d+X
            0x7C => {
                let result = self.shift(Shift::Ror, self.a);
                self.a = result;
                2
            } // ROR A
            0x7D => {
                self.a = self.x;
                self.set_nz(self.a);
                2
            } // MOV A, X
            0x7E => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                self.set_flag(PSW_C, self.y >= value);
                self.set_nz(self.y.wrapping_sub(value));
                3
            } // CMP Y, d
            0x7F => {
                self.psw = self.pull(bus);
                let low = self.pull(bus);
                let high = self.pull(bus);
                self.pc = u16::from(low) | (u16::from(high) << 8);
                6
            } // RETI
            0x80 => {
                self.set_flag(PSW_C, true);
                2
            } // SETC
            0x81 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(16));
                8
            } // TCALL n
            0x82 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(4);
                let value = self.read(bus, addr) | mask;
                self.write(bus, addr, value);
                4
            } // SET1 d.bit
            0x83 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(4);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, set, 7, 5)
            } // BBS d.bit, r
            0x84 => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.a = result;
                };
                3
            } // ADC A, d
            0x85 => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.a = result;
                };
                4
            } // ADC A, !a
            0x86 => {
                let value = self.read(bus, self.ind_x());
                let left = self.a;
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.a = result;
                };
                3
            } // ADC A, (X)
            0x87 => {
                let addr = self.dp_x_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.a = result;
                };
                6
            } // ADC A, [d+X]
            0x88 => {
                let value = self.imm(bus);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.a = result;
                };
                2
            } // ADC A, #i
            0x89 => {
                let source = self.addr_dp(bus);
                let destination = self.addr_dp(bus);
                let left = self.read(bus, destination);
                let right = self.read(bus, source);
                if let Some(result) = self.alu(Alu::Adc, left, right) {
                    self.write(bus, destination, result);
                };
                6
            } // ADC dd, ds
            0x8A => {
                let low = self.fetch(bus);
                let high = self.fetch(bus);
                let addr = u16::from(low) | (u16::from(high & 0x1F) << 8);
                let bit = high >> 5;
                let bit_value = self.read(bus, addr) & Self::bit_mask(bit) != 0;
                self.set_flag(PSW_C, self.flag(PSW_C) ^ bit_value);
                5
            } // EOR1 C, m.b
            0x8B => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let result = self.dec(value);
                self.write(bus, addr, result);
                4
            } // DEC d
            0x8C => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let result = self.dec(value);
                self.write(bus, addr, result);
                5
            } // DEC !a
            0x8D => {
                let value = self.imm(bus);
                self.y = value;
                self.set_nz(self.y);
                2
            } // MOV Y, #i
            0x8E => {
                self.psw = self.pull(bus);
                4
            } // POP PSW
            0x8F => {
                let value = self.imm(bus);
                let addr = self.addr_dp(bus);
                self.write(bus, addr, value);
                5
            } // MOV d, #i
            0x90 => self.branch(bus, !self.flag(PSW_C)), // BCC r
            0x91 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(18));
                8
            } // TCALL n
            0x92 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(4);
                let value = self.read(bus, addr) & !mask;
                self.write(bus, addr, value);
                4
            } // CLR1 d.bit
            0x93 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(4);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, !set, 7, 5)
            } // BBC d.bit, r
            0x94 => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.a = result;
                };
                4
            } // ADC A, d+X
            0x95 => {
                let addr = self.addr_abs_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.a = result;
                };
                5
            } // ADC A, !a+X
            0x96 => {
                let addr = self.addr_abs_y(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.a = result;
                };
                5
            } // ADC A, !a+Y
            0x97 => {
                let addr = self.dp_y_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.a = result;
                };
                6
            } // ADC A, [d]+Y
            0x98 => {
                let value = self.imm(bus);
                let addr = self.addr_dp(bus);
                let left = self.read(bus, addr);
                if let Some(result) = self.alu(Alu::Adc, left, value) {
                    self.write(bus, addr, result);
                };
                5
            } // ADC d, #i
            0x99 => {
                let left_addr = self.ind_x();
                let right_addr = self.ind_y();
                let left = self.read(bus, left_addr);
                let right = self.read(bus, right_addr);
                if let Some(result) = self.alu(Alu::Adc, left, right) {
                    self.write(bus, left_addr, result);
                };
                5
            } // ADC (X), (Y)
            0x9A => {
                let addr = self.addr_dp(bus);
                let value = self.read_word_dp(bus, addr);
                let diff = self.ya().wrapping_sub(value);
                self.set_flag(PSW_C, self.ya() >= value);
                self.set_flag(PSW_H, (self.ya() & 0x0FFF) >= (value & 0x0FFF));
                self.set_flag(
                    PSW_V,
                    ((self.ya() ^ value) & (self.ya() ^ diff) & 0x8000) != 0,
                );
                self.set_ya(diff);
                5
            } // SUBW YA, d
            0x9B => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let result = self.dec(value);
                self.write(bus, addr, result);
                5
            } // DEC d+X
            0x9C => {
                self.a = self.dec(self.a);
                2
            } // DEC A
            0x9D => {
                self.x = self.sp;
                self.set_nz(self.x);
                2
            } // MOV X, SP
            0x9E => {
                // Exact S-SMP division, including the overflow path used when
                // the quotient does not fit in V + A (bsnes spc700.cpp).
                let ya = self.ya();
                let x = u16::from(self.x);
                self.set_flag(PSW_H, (self.y & 0x0F) >= (self.x & 0x0F));
                self.set_flag(PSW_V, self.y >= self.x);
                if u16::from(self.y) < (x << 1) {
                    self.a = if x == 0 { 0xFF } else { (ya / x) as u8 };
                    self.y = if x == 0 { 0 } else { (ya % x) as u8 };
                } else {
                    let base = ya.wrapping_sub(x << 9);
                    let divisor = 256u16.wrapping_sub(x);
                    self.a = (255u16).wrapping_sub(base / divisor) as u8;
                    self.y = x.wrapping_add(base % divisor) as u8;
                }
                self.set_nz(self.a);
                12
            } // DIV YA, X
            0x9F => {
                self.a = self.a.rotate_left(4);
                self.set_nz(self.a);
                5
            } // XCN A
            0xA0 => {
                self.set_flag(PSW_I, true);
                3
            } // EI
            0xA1 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(20));
                8
            } // TCALL n
            0xA2 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(5);
                let value = self.read(bus, addr) | mask;
                self.write(bus, addr, value);
                4
            } // SET1 d.bit
            0xA3 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(5);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, set, 7, 5)
            } // BBS d.bit, r
            0xA4 => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.a = result;
                };
                3
            } // SBC A, d
            0xA5 => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.a = result;
                };
                4
            } // SBC A, !a
            0xA6 => {
                let value = self.read(bus, self.ind_x());
                let left = self.a;
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.a = result;
                };
                3
            } // SBC A, (X)
            0xA7 => {
                let addr = self.dp_x_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.a = result;
                };
                6
            } // SBC A, [d+X]
            0xA8 => {
                let value = self.imm(bus);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.a = result;
                };
                2
            } // SBC A, #i
            0xA9 => {
                let source = self.addr_dp(bus);
                let destination = self.addr_dp(bus);
                let left = self.read(bus, destination);
                let right = self.read(bus, source);
                if let Some(result) = self.alu(Alu::Sbc, left, right) {
                    self.write(bus, destination, result);
                };
                6
            } // SBC dd, ds
            0xAA => {
                let low = self.fetch(bus);
                let high = self.fetch(bus);
                let addr = u16::from(low) | (u16::from(high & 0x1F) << 8);
                let bit = high >> 5;
                let value = self.read(bus, addr) & Self::bit_mask(bit) != 0;
                self.set_flag(PSW_C, value);
                4
            } // MOV1 C, m.b
            0xAB => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                let result = self.inc(value);
                self.write(bus, addr, result);
                4
            } // INC d
            0xAC => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                let result = self.inc(value);
                self.write(bus, addr, result);
                5
            } // INC !a
            0xAD => {
                let value = self.imm(bus);
                self.set_flag(PSW_C, self.y >= value);
                self.set_nz(self.y.wrapping_sub(value));
                2
            } // CMP Y, #i
            0xAE => {
                self.a = self.pull(bus);
                4
            } // POP A
            0xAF => {
                let addr = self.ind_x();
                let value = self.a;
                self.write(bus, addr, value);
                self.x = self.x.wrapping_add(1);
                4
            } // MOV (X)+, A
            0xB0 => self.branch(bus, self.flag(PSW_C)), // BCS r
            0xB1 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(22));
                8
            } // TCALL n
            0xB2 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(5);
                let value = self.read(bus, addr) & !mask;
                self.write(bus, addr, value);
                4
            } // CLR1 d.bit
            0xB3 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(5);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, !set, 7, 5)
            } // BBC d.bit, r
            0xB4 => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.a = result;
                };
                4
            } // SBC A, d+X
            0xB5 => {
                let addr = self.addr_abs_x(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.a = result;
                };
                5
            } // SBC A, !a+X
            0xB6 => {
                let addr = self.addr_abs_y(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.a = result;
                };
                5
            } // SBC A, !a+Y
            0xB7 => {
                let addr = self.dp_y_ptr(bus);
                let value = self.read(bus, addr);
                let left = self.a;
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.a = result;
                };
                6
            } // SBC A, [d]+Y
            0xB8 => {
                let value = self.imm(bus);
                let addr = self.addr_dp(bus);
                let left = self.read(bus, addr);
                if let Some(result) = self.alu(Alu::Sbc, left, value) {
                    self.write(bus, addr, result);
                };
                5
            } // SBC d, #i
            0xB9 => {
                let left_addr = self.ind_x();
                let right_addr = self.ind_y();
                let left = self.read(bus, left_addr);
                let right = self.read(bus, right_addr);
                if let Some(result) = self.alu(Alu::Sbc, left, right) {
                    self.write(bus, left_addr, result);
                };
                5
            } // SBC (X), (Y)
            0xBA => {
                let addr = self.addr_dp(bus);
                let value = self.read_word_dp(bus, addr);
                self.set_ya(value);
                5
            } // MOVW YA, d
            0xBB => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                let result = self.inc(value);
                self.write(bus, addr, result);
                5
            } // INC d+X
            0xBC => {
                self.a = self.inc(self.a);
                2
            } // INC A
            0xBD => {
                self.sp = self.x;
                2
            } // MOV SP, X
            0xBE => {
                let carry = self.flag(PSW_C);
                let half = self.flag(PSW_H);
                let mut result = u16::from(self.a);
                if self.a > 0x99 || !carry {
                    result = result.wrapping_sub(0x60);
                    self.set_flag(PSW_C, false);
                }
                if (result as u8 & 0x0F) > 9 || !half {
                    result = result.wrapping_sub(0x06);
                }
                self.a = result as u8;
                self.set_nz(self.a);
                3
            } // DAS A
            0xBF => {
                let addr = self.ind_x();
                let value = self.read(bus, addr);
                self.x = self.x.wrapping_add(1);
                self.a = value;
                self.set_nz(self.a);
                4
            } // MOV A, (X)+
            0xC0 => {
                self.set_flag(PSW_I, false);
                3
            } // DI
            0xC1 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(24));
                8
            } // TCALL n
            0xC2 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(6);
                let value = self.read(bus, addr) | mask;
                self.write(bus, addr, value);
                4
            } // SET1 d.bit
            0xC3 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(6);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, set, 7, 5)
            } // BBS d.bit, r
            0xC4 => {
                let addr = self.addr_dp(bus);
                let value = self.a;
                self.write(bus, addr, value);
                4
            } // MOV d, A
            0xC5 => {
                let addr = self.addr_abs(bus);
                let value = self.a;
                self.write(bus, addr, value);
                5
            } // MOV !a, A
            0xC6 => {
                let addr = self.ind_x();
                let value = self.a;
                self.write(bus, addr, value);
                4
            } // MOV (X), A
            0xC7 => {
                let addr = self.dp_x_ptr(bus);
                let value = self.a;
                self.write(bus, addr, value);
                7
            } // MOV [d+X], A
            0xC8 => {
                let value = self.imm(bus);
                self.set_flag(PSW_C, self.x >= value);
                self.set_nz(self.x.wrapping_sub(value));
                2
            } // CMP X, #i
            0xC9 => {
                let addr = self.addr_abs(bus);
                let value = self.x;
                self.write(bus, addr, value);
                5
            } // MOV !a, X
            0xCA => {
                let low = self.fetch(bus);
                let high = self.fetch(bus);
                let addr = u16::from(low) | (u16::from(high & 0x1F) << 8);
                let bit = high >> 5;
                let mask = Self::bit_mask(bit);
                let value = self.read(bus, addr);
                let value = if self.flag(PSW_C) {
                    value | mask
                } else {
                    value & !mask
                };
                self.write(bus, addr, value);
                6
            } // MOV1 m.b, C
            0xCB => {
                let addr = self.addr_dp(bus);
                let value = self.y;
                self.write(bus, addr, value);
                4
            } // MOV d, Y
            0xCC => {
                let addr = self.addr_abs(bus);
                let value = self.y;
                self.write(bus, addr, value);
                5
            } // MOV !a, Y
            0xCD => {
                let value = self.imm(bus);
                self.x = value;
                self.set_nz(self.x);
                2
            } // MOV X, #i
            0xCE => {
                self.x = self.pull(bus);
                4
            } // POP X
            0xCF => {
                let product = u16::from(self.y) * u16::from(self.a);
                self.a = product as u8;
                self.y = (product >> 8) as u8;
                self.set_nz(self.y);
                9
            } // MUL YA
            0xD0 => self.branch(bus, !self.flag(PSW_Z)), // BNE r
            0xD1 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(26));
                8
            } // TCALL n
            0xD2 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(6);
                let value = self.read(bus, addr) & !mask;
                self.write(bus, addr, value);
                4
            } // CLR1 d.bit
            0xD3 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(6);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, !set, 7, 5)
            } // BBC d.bit, r
            0xD4 => {
                let addr = self.addr_dp_x(bus);
                let value = self.a;
                self.write(bus, addr, value);
                5
            } // MOV d+X, A
            0xD5 => {
                let addr = self.addr_abs_x(bus);
                let value = self.a;
                self.write(bus, addr, value);
                6
            } // MOV !a+X, A
            0xD6 => {
                let addr = self.addr_abs_y(bus);
                let value = self.a;
                self.write(bus, addr, value);
                6
            } // MOV !a+Y, A
            0xD7 => {
                let addr = self.dp_y_ptr(bus);
                let value = self.a;
                self.write(bus, addr, value);
                7
            } // MOV [d]+Y, A
            0xD8 => {
                let addr = self.addr_dp(bus);
                let value = self.x;
                self.write(bus, addr, value);
                4
            } // MOV d, X
            0xD9 => {
                let addr = self.addr_dp_y(bus);
                let value = self.x;
                self.write(bus, addr, value);
                5
            } // MOV d+Y, X
            0xDA => {
                let addr = self.addr_dp(bus);
                let value = self.ya();
                self.write_word_dp(bus, addr, value);
                5
            } // MOVW d, YA
            0xDB => {
                let addr = self.addr_dp_x(bus);
                let value = self.y;
                self.write(bus, addr, value);
                5
            } // MOV d+X, Y
            0xDC => {
                self.y = self.dec(self.y);
                2
            } // DEC Y
            0xDD => {
                self.a = self.y;
                self.set_nz(self.a);
                2
            } // MOV A, Y
            0xDE => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                self.branch_cycles(bus, self.a != value, 8, 6)
            } // CBNE d+X, r
            0xDF => {
                let carry = self.flag(PSW_C);
                let half = self.flag(PSW_H);
                let mut result = u16::from(self.a);
                if self.a > 0x99 || carry {
                    result = result.wrapping_add(0x60);
                    self.set_flag(PSW_C, true);
                }
                if (result as u8 & 0x0F) > 9 || half {
                    result = result.wrapping_add(0x06);
                }
                self.a = result as u8;
                self.set_nz(self.a);
                3
            } // DAA A
            0xE0 => {
                self.set_flag(PSW_V, false);
                self.set_flag(PSW_H, false);
                2
            } // CLRV
            0xE1 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(28));
                8
            } // TCALL n
            0xE2 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(7);
                let value = self.read(bus, addr) | mask;
                self.write(bus, addr, value);
                4
            } // SET1 d.bit
            0xE3 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(7);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, set, 7, 5)
            } // BBS d.bit, r
            0xE4 => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                self.a = value;
                self.set_nz(self.a);
                3
            } // MOV A, d
            0xE5 => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                self.a = value;
                self.set_nz(self.a);
                4
            } // MOV A, !a
            0xE6 => {
                let value = self.read(bus, self.ind_x());
                self.a = value;
                self.set_nz(self.a);
                3
            } // MOV A, (X)
            0xE7 => {
                let addr = self.dp_x_ptr(bus);
                let value = self.read(bus, addr);
                self.a = value;
                self.set_nz(self.a);
                6
            } // MOV A, [d+X]
            0xE8 => {
                let value = self.imm(bus);
                self.a = value;
                self.set_nz(self.a);
                2
            } // MOV A, #i
            0xE9 => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                self.x = value;
                self.set_nz(self.x);
                4
            } // MOV X, !a
            0xEA => {
                let low = self.fetch(bus);
                let high = self.fetch(bus);
                let addr = u16::from(low) | (u16::from(high & 0x1F) << 8);
                let bit = high >> 5;
                let mask = Self::bit_mask(bit);
                let value = self.read(bus, addr) ^ mask;
                self.write(bus, addr, value);
                5
            } // NOT1 m.b
            0xEB => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                self.y = value;
                self.set_nz(self.y);
                3
            } // MOV Y, d
            0xEC => {
                let addr = self.addr_abs(bus);
                let value = self.read(bus, addr);
                self.y = value;
                self.set_nz(self.y);
                4
            } // MOV Y, !a
            0xED => {
                self.psw ^= PSW_C;
                3
            } // NOTC
            0xEE => {
                self.y = self.pull(bus);
                4
            } // POP Y
            0xEF => {
                self.stopped = true;
                7
            } // SLEEP
            0xF0 => self.branch(bus, self.flag(PSW_Z)), // BEQ r
            0xF1 => {
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.read_word(bus, 0xFFDEu16.wrapping_sub(30));
                8
            } // TCALL n
            0xF2 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(7);
                let value = self.read(bus, addr) & !mask;
                self.write(bus, addr, value);
                4
            } // CLR1 d.bit
            0xF3 => {
                let addr = self.addr_dp(bus);
                let mask = Self::bit_mask(7);
                let set = self.read(bus, addr) & mask != 0;
                self.branch_cycles(bus, !set, 7, 5)
            } // BBC d.bit, r
            0xF4 => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                self.a = value;
                self.set_nz(self.a);
                4
            } // MOV A, d+X
            0xF5 => {
                let addr = self.addr_abs_x(bus);
                let value = self.read(bus, addr);
                self.a = value;
                self.set_nz(self.a);
                5
            } // MOV A, !a+X
            0xF6 => {
                let addr = self.addr_abs_y(bus);
                let value = self.read(bus, addr);
                self.a = value;
                self.set_nz(self.a);
                5
            } // MOV A, !a+Y
            0xF7 => {
                let addr = self.dp_y_ptr(bus);
                let value = self.read(bus, addr);
                self.a = value;
                self.set_nz(self.a);
                6
            } // MOV A, [d]+Y
            0xF8 => {
                let addr = self.addr_dp(bus);
                let value = self.read(bus, addr);
                self.x = value;
                self.set_nz(self.x);
                3
            } // MOV X, d
            0xF9 => {
                let addr = self.addr_dp_y(bus);
                let value = self.read(bus, addr);
                self.x = value;
                self.set_nz(self.x);
                4
            } // MOV X, d+Y
            0xFA => {
                let source = self.addr_dp(bus);
                let destination = self.addr_dp(bus);
                let value = self.read(bus, source);
                self.write(bus, destination, value);
                5
            } // MOV dd, ds
            0xFB => {
                let addr = self.addr_dp_x(bus);
                let value = self.read(bus, addr);
                self.y = value;
                self.set_nz(self.y);
                4
            } // MOV Y, d+X
            0xFC => {
                self.y = self.inc(self.y);
                2
            } // INC Y
            0xFD => {
                self.y = self.a;
                self.set_nz(self.y);
                2
            } // MOV Y, A
            0xFE => {
                self.y = self.y.wrapping_sub(1);
                self.branch_cycles(bus, self.y != 0, 6, 4)
            } // DBNZ Y, r
            0xFF => {
                self.stopped = true;
                7
            } // STOP
        }
    }
}
