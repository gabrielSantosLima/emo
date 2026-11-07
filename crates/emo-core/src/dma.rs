//! DMA and HDMA channel state.
//!
//! A channel is a small state machine: `DMAP` selects the transfer pattern,
//! `BBAD` the B-bus address, `A1T/A1B` the source, `DAS` the byte count.
//! HDMA reuses the same channel registers plus the `A2A/A2B` table pointer.

use serde::{Deserialize, Serialize};

/// One of the eight DMA/HDMA channels.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DmaChannel {
    /// Transfer mode, direction and A-bus increment mode.
    pub dmap: u8,
    /// B-bus address low byte (`$21xx`).
    pub bbad: u8,
    /// A-bus address low word.
    pub a1t: u16,
    /// A-bus bank.
    pub a1b: u8,
    /// Byte count, or the indirect pointer during HDMA.
    pub das: u16,
    /// Indirect pointer bank.
    pub dasb: u8,
    /// HDMA table address.
    pub a2a: u16,
    /// HDMA table bank.
    pub a2b: u8,

    /// Current position in the HDMA table.
    pub table_index: u16,
    /// Bank of the current HDMA table.
    pub table_bank: u8,
    /// Lines left for the current table entry.
    pub remaining: u8,
    /// Current HDMA data pointer (indirect mode).
    pub pointer: u16,
    /// Bank of the indirect data pointer.
    pub pointer_bank: u8,
    /// True when the table ended or the channel finished.
    pub finished: bool,
    /// True while the current entry uses indirect addressing.
    pub indirect: bool,
    /// True when the current line should transfer a unit.
    pub do_transfer: bool,
}

impl DmaChannel {
    /// Bytes transferred per B-bus unit.
    #[must_use]
    pub const fn unit_size(&self) -> u16 {
        match self.dmap & 0x07 {
            0 => 1,
            1 | 2 | 6 => 2,
            _ => 4,
        }
    }

    /// B-bus offsets used inside one unit.
    #[must_use]
    pub const fn b_offsets(&self) -> &'static [u8] {
        match self.dmap & 0x07 {
            0 => &[0],
            1 => &[0, 1],
            2 => &[0, 0],
            3 => &[0, 1, 2, 3],
            4 => &[0, 1, 2, 3],
            5 => &[0, 1, 0, 1],
            6 => &[0, 0],
            _ => &[0, 0, 0, 0],
        }
    }

    /// True when the transfer goes from the B-bus to the A-bus.
    #[must_use]
    pub const fn reverse(&self) -> bool {
        self.dmap & 0x80 != 0
    }

    /// A-bus step after each byte.
    ///
    /// `DMAP` bits 4-3: 0 = increment, 2 = decrement, 1 and 3 = fixed.
    #[must_use]
    pub const fn a_step(&self) -> i32 {
        match (self.dmap >> 3) & 0x03 {
            0 => 1,
            2 => -1,
            _ => 0,
        }
    }

    /// Reset the HDMA cursors before the first line of a frame.
    pub fn reset_hdma(&mut self) {
        // The table starts at `A1T`, not at the running `A2A` pointer.
        self.table_index = self.a1t;
        self.table_bank = self.a1b;
        self.remaining = 0;
        self.finished = false;
        self.indirect = self.dmap & 0x40 != 0;
        self.pointer = 0;
        self.pointer_bank = self.dasb;
        self.do_transfer = false;
    }
}

/// B-bus register address for a channel.
#[must_use]
pub const fn b_address(channel: &DmaChannel) -> u16 {
    0x2100 | channel.bbad as u16
}
