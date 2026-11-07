//! Scanline renderer, register file and video memory.

use serde::{Deserialize, Serialize};

/// Visible width in pixels.
pub const SCREEN_WIDTH: usize = 256;

/// Size of one BGR555 palette entry in bits.
const BGR555_MASK: u16 = 0x7FFF;

/// Hardware layer order, top to bottom, as `(layer, priority)` pairs. The
/// sprite layer is index 4. Background tiles only carry one priority bit;
/// sprites carry two (3 = highest).
const ORDER_MODE0: [(u8, u8); 12] = [
    (4, 3),
    (0, 1),
    (1, 1),
    (4, 2),
    (0, 0),
    (1, 0),
    (4, 1),
    (2, 1),
    (3, 1),
    (4, 0),
    (2, 0),
    (3, 0),
];

const ORDER_MODE1: [(u8, u8); 10] = [
    (4, 3),
    (0, 1),
    (1, 1),
    (4, 2),
    (0, 0),
    (1, 0),
    (4, 1),
    (2, 1),
    (4, 0),
    (2, 0),
];

/// Mode 1 with the BG3 priority bit (`BGMODE` bit 3) set.
const ORDER_MODE1_HIGH: [(u8, u8); 10] = [
    (2, 1),
    (4, 3),
    (0, 1),
    (1, 1),
    (4, 2),
    (0, 0),
    (1, 0),
    (4, 1),
    (4, 0),
    (2, 0),
];

const ORDER_MODE234: [(u8, u8); 8] = [
    (4, 3),
    (0, 1),
    (4, 2),
    (1, 1),
    (4, 1),
    (0, 0),
    (4, 0),
    (1, 0),
];

const ORDER_MODE7: [(u8, u8); 5] = [(4, 3), (4, 2), (4, 1), (0, 3), (4, 0)];

/// Frame geometry for one video standard.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Timing {
    /// Scanlines in a frame, including vertical blanking.
    pub lines_per_frame: u16,
    /// Scanlines that produce visible pixels.
    pub visible_lines: u16,
    /// First scanline of vertical blanking (where NMI fires).
    pub vblank_line: u16,
    /// Master cycles per scanline.
    pub line_cycles: u64,
}

impl Timing {
    /// NTSC geometry: 262 lines, 224 visible, vblank at 225.
    #[must_use]
    pub const fn ntsc() -> Self {
        Self {
            lines_per_frame: 262,
            visible_lines: 224,
            vblank_line: 225,
            line_cycles: 1364,
        }
    }

    /// PAL geometry: 312 lines, 239 visible, vblank at 240.
    #[must_use]
    pub const fn pal() -> Self {
        Self {
            lines_per_frame: 312,
            visible_lines: 239,
            vblank_line: 240,
            line_cycles: 1364,
        }
    }
}

/// Palette indices and priorities for the two screens of one scanline.
struct LineBuffers {
    bg_index: [[u16; SCREEN_WIDTH]; 4],
    bg_priority: [[u8; SCREEN_WIDTH]; 4],
    obj_index: [u16; SCREEN_WIDTH],
    obj_priority: [u8; SCREEN_WIDTH],
}

/// The S-PPU.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Ppu {
    timing: Timing,
    vram: Vec<u8>,
    cgram: Vec<u16>,
    oam: Vec<u8>,
    framebuffer: Vec<u8>,

    inidisp: u8,
    obsel: u8,
    bgmode: u8,
    mosaic: u8,
    bgsc: [u8; 4],
    bgnba: [u8; 2],
    bghofs: [u16; 4],
    bgvofs: [u16; 4],
    hv_latch: u8,
    h_latch: u8,
    m7_latch: u8,
    m7_hofs: u16,
    m7_vofs: u16,
    m7sel: u8,
    m7: [i16; 8],
    m7_low: [bool; 8],
    setini: u8,
    tm: u8,
    ts: u8,
    tmw: u8,
    tsw: u8,
    w12sel: u8,
    w34sel: u8,
    wobjsel: u8,
    wbglog: u8,
    wobjlog: u8,
    wh: [u8; 4],
    cgwsel: u8,
    cgadsub: u8,
    coldata: [u8; 3],

    vmain: u8,
    vmaddr: u16,
    vram_latch: u16,
    vram_read_buffer: u16,
    cgaddr: u8,
    ophct: u16,
    opvct: u16,
    ophct_high: bool,
    opvct_high: bool,
    cgram_latch: u8,
    cgram_high: bool,
    oamaddr: u16,

    line_counter: u64,
    clock: u64,
    frame_ready: bool,
    nmi: bool,
    io_latch: u8,
}

impl Ppu {
    /// Create a PPU with the given frame geometry.
    #[must_use]
    pub fn new(timing: Timing) -> Self {
        let height = usize::from(timing.visible_lines);
        Self {
            timing,
            vram: vec![0; 0x10000],
            cgram: vec![0; 256],
            oam: vec![0; 0x220],
            framebuffer: vec![0; SCREEN_WIDTH * height * 3],

            inidisp: 0x80,
            obsel: 0,
            bgmode: 0,
            mosaic: 0,
            bgsc: [0; 4],
            bgnba: [0; 2],
            bghofs: [0; 4],
            bgvofs: [0; 4],
            hv_latch: 0,
            h_latch: 0,
            m7_latch: 0,
            m7_hofs: 0,
            m7_vofs: 0,
            m7sel: 0,
            m7: [0; 8],
            m7_low: [false; 8],
            setini: 0,
            tm: 0,
            ts: 0,
            tmw: 0,
            tsw: 0,
            w12sel: 0,
            w34sel: 0,
            wobjsel: 0,
            wbglog: 0,
            wobjlog: 0,
            wh: [0; 4],
            cgwsel: 0,
            cgadsub: 0,
            coldata: [0; 3],

            vmain: 0,
            vmaddr: 0,
            vram_latch: 0,
            vram_read_buffer: 0,
            cgaddr: 0,
            ophct: 0,
            opvct: 0,
            ophct_high: false,
            opvct_high: false,
            cgram_latch: 0,
            cgram_high: false,
            oamaddr: 0,

            line_counter: 0,
            clock: 0,
            frame_ready: false,
            nmi: false,
            io_latch: 0,
        }
    }

    /// Frame geometry.
    #[must_use]
    pub const fn timing(&self) -> Timing {
        self.timing
    }

    /// Visible framebuffer, RGB888, `SCREEN_WIDTH * visible_lines` pixels.
    #[must_use]
    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    /// Raw VRAM, for debugging.
    #[must_use]
    pub fn vram(&self) -> &[u8] {
        &self.vram
    }

    /// Raw CGRAM, for debugging.
    #[must_use]
    pub fn cgram(&self) -> &[u16] {
        &self.cgram
    }

    /// Raw OAM, for debugging.
    #[must_use]
    pub fn oam(&self) -> &[u8] {
        &self.oam
    }

    /// Reset timing while keeping memory and registers.
    pub fn reset_timing(&mut self) {
        self.line_counter = 0;
        self.clock = 0;
        self.frame_ready = false;
        self.nmi = false;
    }

    /// True once the last scanline of a frame has been rendered.
    #[must_use]
    pub const fn frame_ready(&self) -> bool {
        self.frame_ready
    }

    /// Scanlines processed since reset.
    #[must_use]
    pub const fn line_counter(&self) -> u64 {
        self.line_counter
    }

    /// Visible scanlines per frame.
    #[must_use]
    pub const fn visible_lines(&self) -> u16 {
        self.timing.visible_lines
    }

    /// True while the beam is inside vertical blanking.
    #[must_use]
    pub fn in_vblank(&self) -> bool {
        let line = (self.line_counter % u64::from(self.timing.lines_per_frame)) as u16;
        line >= self.timing.vblank_line
    }

    /// True while the PPU is in horizontal blanking.
    ///
    /// The renderer works per scanline, so the last 340 master cycles of the
    /// line are treated as HBlank (the visible width is 256 pixels).
    #[must_use]
    pub fn in_hblank(&self) -> bool {
        (self.clock % self.timing.line_cycles) >= 1024
    }

    /// Clear the frame-complete flag after the frontend consumed the frame.
    pub fn clear_frame_ready(&mut self) {
        self.frame_ready = false;
    }

    /// Display control 1 (`$2100`).
    #[must_use]
    pub const fn display_control(&self) -> u8 {
        self.inidisp
    }

    /// Main screen designation (`$212C`).
    #[must_use]
    pub const fn main_screen(&self) -> u8 {
        self.tm
    }

    /// Sub-screen designation (`$212D`).
    #[must_use]
    pub const fn sub_screen(&self) -> u8 {
        self.ts
    }

    /// Object size and base (`$2101`).
    #[must_use]
    pub const fn obsel(&self) -> u8 {
        self.obsel
    }

    /// Raw `VMAIN` register, for debugging.
    #[must_use]
    pub const fn vmain(&self) -> u8 {
        self.vmain
    }

    /// Background mode (`$2105` bits 2-0).
    #[must_use]
    pub const fn background_mode(&self) -> u8 {
        self.bgmode & 0x07
    }

    /// Screen base register for a background layer (`$2107-$210A`).
    #[must_use]
    pub const fn background_screen(&self, layer: usize) -> u8 {
        self.bgsc[layer]
    }

    /// Character base word address for a background layer.
    #[must_use]
    pub fn background_char_base(&self, layer: usize) -> u16 {
        let nibble = if layer.is_multiple_of(2) {
            self.bgnba[layer / 2] & 0x0F
        } else {
            self.bgnba[layer / 2] >> 4
        };
        u16::from(nibble) << 12
    }

    /// True when an NMI request is pending.
    #[must_use]
    pub const fn nmi_pending(&self) -> bool {
        self.nmi
    }

    /// Take the latched NMI request, if any.
    pub fn take_nmi(&mut self) -> bool {
        let pending = self.nmi;
        self.nmi = false;
        pending
    }

    /// Render every scanline up to `clock` (absolute master cycles).
    pub fn advance_to(&mut self, clock: u64) {
        self.clock = clock;
        let line_cycles = self.timing.line_cycles;
        let lines = u64::from(self.timing.lines_per_frame);
        let target = clock / line_cycles;

        while self.line_counter < target {
            let line = (self.line_counter % lines) as u16;

            if line < self.timing.visible_lines {
                self.render_line(line);
            }

            self.line_counter += 1;
            let next = (self.line_counter % lines) as u16;
            if next == self.timing.vblank_line {
                self.nmi = true;
            }
            if next == 0 {
                self.frame_ready = true;
            }
        }
    }

    /* ---------- Register interface ---------- */

    /// Read a PPU register (`$2100-$213F`).
    pub fn read_register(&mut self, addr: u16) -> u8 {
        let value = match addr {
            0x2138 => {
                let index = Self::oam_index(self.oamaddr);
                let value = self.oam[index];
                self.oamaddr = self.oamaddr.wrapping_add(1) & 0x3FF;
                value
            }
            0x2139 | 0x213A => {
                let value = if addr == 0x2139 {
                    (self.vram_read_buffer & 0x00FF) as u8
                } else {
                    (self.vram_read_buffer >> 8) as u8
                };
                self.vram_read_buffer = self.read_vram_word(self.vram_address());
                if (self.vmain & 0x80 != 0) == (addr == 0x213A) {
                    self.increment_vram();
                }
                value
            }
            0x213B => {
                let value = self.cgram[self.cgaddr as usize] as u8;
                self.cgaddr = self.cgaddr.wrapping_add(1);
                value
            }
            0x2137 => {
                self.ophct = ((self.clock % self.timing.line_cycles) / 4) as u16;
                self.opvct = (self.line_counter % u64::from(self.timing.lines_per_frame)) as u16;
                self.ophct_high = false;
                self.opvct_high = false;
                0
            }
            0x213C => {
                if self.ophct_high {
                    self.ophct_high = false;
                    (self.ophct >> 8) as u8
                } else {
                    self.ophct_high = true;
                    self.ophct as u8
                }
            }
            0x213D => {
                if self.opvct_high {
                    self.opvct_high = false;
                    (self.opvct >> 8) as u8
                } else {
                    self.opvct_high = true;
                    self.opvct as u8
                }
            }
            0x213E => 0x01,
            0x213F => 0x03,
            _ => self.io_latch,
        };
        self.io_latch = value;
        value
    }

    /// Write a PPU register (`$2100-$213F`).
    pub fn write_register(&mut self, addr: u16, value: u8) {
        self.io_latch = value;

        match addr {
            0x2100 => self.inidisp = value,
            0x2101 => self.obsel = value,
            0x2102 => self.oamaddr = (self.oamaddr & 0x100) | u16::from(value),
            0x2103 => self.oamaddr = (self.oamaddr & 0x0FF) | (u16::from(value & 1) << 8),
            0x2104 => {
                let index = Self::oam_index(self.oamaddr);
                self.oam[index] = value;
                self.oamaddr = self.oamaddr.wrapping_add(1) & 0x3FF;
            }
            0x2105 => self.bgmode = value,
            0x2106 => self.mosaic = value,
            0x2107..=0x210A => self.bgsc[usize::from(addr - 0x2107)] = value,
            0x210B => self.bgnba[0] = value,
            0x210C => self.bgnba[1] = value,
            0x210D..=0x2114 => self.write_bg_scroll(addr, value),
            0x2115 => self.vmain = value,
            0x2116 => self.vmaddr = (self.vmaddr & 0x7F00) | u16::from(value),
            // VRAM has 32K words: the high register contributes address bits
            // 8-14 (bit 7 and above are not connected).
            0x2117 => self.vmaddr = (self.vmaddr & 0x00FF) | (u16::from(value & 0x7F) << 8),
            0x2118 => {
                let address = self.vram_address();
                self.write_vram_byte(address, value, false);
                if self.vmain & 0x80 == 0 {
                    self.increment_vram();
                }
            }
            0x2119 => {
                let address = self.vram_address();
                self.write_vram_byte(address, value, true);
                if self.vmain & 0x80 != 0 {
                    self.increment_vram();
                }
            }
            0x211A => self.m7sel = value,
            0x211B..=0x2120 => self.write_m7(addr, value),
            0x2121 => self.cgaddr = value,
            0x2122 => {
                if self.cgram_high {
                    let color = u16::from(self.cgram_latch) | (u16::from(value) << 8);
                    self.cgram[self.cgaddr as usize] = color & BGR555_MASK;
                    self.cgaddr = self.cgaddr.wrapping_add(1);
                    self.cgram_high = false;
                } else {
                    self.cgram_latch = value;
                    self.cgram_high = true;
                }
            }
            0x2123 => self.w12sel = value,
            0x2124 => self.w34sel = value,
            0x2125 => self.wobjsel = value,
            0x2126..=0x2129 => self.wh[usize::from(addr - 0x2126)] = value,
            0x212A => self.wbglog = value,
            0x212B => self.wobjlog = value,
            0x212C => self.tm = value,
            0x212D => self.ts = value,
            0x212E => self.tmw = value,
            0x212F => self.tsw = value,
            0x2130 => self.cgwsel = value,
            0x2131 => self.cgadsub = value,
            0x2132 => {
                if value & 0x20 != 0 {
                    self.coldata[0] = value & 0x1F;
                }
                if value & 0x40 != 0 {
                    self.coldata[1] = value & 0x1F;
                }
                if value & 0x80 != 0 {
                    self.coldata[2] = value & 0x1F;
                }
            }
            0x2133 => self.setini = value,
            _ => self.io_latch = value,
        }
    }

    fn write_bg_scroll(&mut self, addr: u16, value: u8) {
        let offset = addr - 0x210D;
        let layer = (offset / 2) as usize;
        let horizontal = offset.is_multiple_of(2);

        if horizontal {
            if addr == 0x210D {
                self.m7_hofs = ((u16::from(value) << 8) | u16::from(self.m7_latch)) & 0x1FFF;
                self.m7_latch = value;
            }
            // The hardware keeps two shared latches: the low byte of a
            // horizontal write is the previous vertical write's high bits
            // combined with the previous horizontal write's low bits.
            self.bghofs[layer] = ((u16::from(value) << 8)
                | u16::from(self.hv_latch & !0x07)
                | u16::from(self.h_latch & 0x07))
                & 0x3FF;
            self.hv_latch = value;
            self.h_latch = value;
        } else {
            if addr == 0x210E {
                self.m7_vofs = ((u16::from(value) << 8) | u16::from(self.m7_latch)) & 0x1FFF;
                self.m7_latch = value;
            }
            self.bgvofs[layer] = ((u16::from(value) << 8) | u16::from(self.hv_latch)) & 0x3FF;
            self.hv_latch = value;
        }
    }

    fn write_m7(&mut self, addr: u16, value: u8) {
        let index = usize::from(addr - 0x211B);
        if self.m7_low[index] {
            let raw = (self.m7[index] as u16 & 0x00FF) | (u16::from(value) << 8);
            self.m7[index] = raw as i16;
        } else {
            let raw = (self.m7[index] as u16 & 0xFF00) | u16::from(value);
            self.m7[index] = raw as i16;
        }
        self.m7_low[index] = !self.m7_low[index];
    }

    /// Word address used by the next VRAM access, with the `VMAIN` address
    /// translation applied (fullsnes: rotate the low 8/9/10 bits left by 3).
    fn vram_address(&self) -> u16 {
        let bits = match (self.vmain >> 2) & 0x03 {
            0 => return self.vmaddr,
            1 => 8,
            2 => 9,
            _ => 10,
        };
        let mask = (1u16 << bits) - 1;
        let low = self.vmaddr & mask;
        let rotated = ((low << 3) | (low >> (bits - 3))) & mask;
        (self.vmaddr & !mask) | rotated
    }

    fn increment_vram(&mut self) {
        let step = match self.vmain & 0x03 {
            0 => 1,
            1 => 32,
            2 => 128,
            _ => 128,
        };
        self.vmaddr = self.vmaddr.wrapping_add(step) & 0x7FFF;
    }

    fn read_vram_word(&self, addr: u16) -> u16 {
        let index = usize::from(addr & 0x7FFF) * 2;
        u16::from(self.vram[index]) | (u16::from(self.vram[index + 1]) << 8)
    }

    #[cfg(test)]
    fn write_vram_word(&mut self, addr: u16, value: u16) {
        let index = usize::from(addr & 0x7FFF) * 2;
        self.vram[index] = value as u8;
        self.vram[index + 1] = (value >> 8) as u8;
    }

    fn write_vram_byte(&mut self, addr: u16, value: u8, high: bool) {
        let index = usize::from(addr & 0x7FFF) * 2 + usize::from(high);
        self.vram[index] = value;
    }

    /* ---------- Rendering ---------- */

    fn render_line(&mut self, y: u16) {
        let start = usize::from(y) * SCREEN_WIDTH * 3;

        if self.inidisp & 0x80 != 0 {
            self.framebuffer[start..start + SCREEN_WIDTH * 3].fill(0);
            return;
        }

        let mode = self.bgmode & 0x07;
        let mut line = LineBuffers {
            bg_index: [[0u16; SCREEN_WIDTH]; 4],
            bg_priority: [[0u8; SCREEN_WIDTH]; 4],
            obj_index: [0u16; SCREEN_WIDTH],
            obj_priority: [0u8; SCREEN_WIDTH],
        };

        if mode == 7 {
            let mosaic = u16::from(self.mosaic & 0x0F);
            let sample_y = if mosaic > 0 { y - y % (mosaic + 1) } else { y };
            for (x, slot) in line.bg_index[0].iter_mut().enumerate() {
                let screen_x = x as u16;
                let sample_x = if mosaic > 0 {
                    screen_x - screen_x % (mosaic + 1)
                } else {
                    screen_x
                };
                if let Some((index, priority)) = self.mode7_pixel(sample_x, sample_y) {
                    *slot = index;
                    line.bg_priority[0][x] = priority;
                }
            }
        } else {
            for layer in 0..4 {
                if (self.tm | self.ts) & (1 << layer) == 0 {
                    continue;
                }
                self.build_bg_line(
                    layer,
                    y,
                    mode,
                    &mut line.bg_index[layer],
                    &mut line.bg_priority[layer],
                );
            }
        }

        if (self.tm | self.ts) & 0x10 != 0 {
            self.build_sprite_line(y, &mut line.obj_index, &mut line.obj_priority);
        }

        for x in 0..SCREEN_WIDTH {
            let main = self.compose(x, self.tm, self.tmw, &line);
            let sub = self.compose(x, self.ts, self.tsw, &line);
            let (r, g, b) = self.shade(main, sub, x as u16);
            if std::env::var_os("EMO_SPR").is_some() && y == 160 && (106..=137).contains(&x) {
                eprintln!(
                    "x={x} obj={:?} objprio={} main={main:?} sub={sub:?} rgb=({r},{g},{b})",
                    line.obj_index[x], line.obj_priority[x]
                );
            }
            let base = start + x * 3;
            self.framebuffer[base] = r;
            self.framebuffer[base + 1] = g;
            self.framebuffer[base + 2] = b;
        }
    }

    /// Rank of a layer pixel, following the hardware priority chart for the
    /// current background mode. Higher is closer to the front.
    fn layer_rank(&self, layer: usize, priority: u8) -> u8 {
        let mode = self.bgmode & 0x07;
        let order: &[(u8, u8)] = match mode {
            0 => &ORDER_MODE0,
            1 if self.bgmode & 0x08 != 0 => &ORDER_MODE1_HIGH,
            1 => &ORDER_MODE1,
            7 => &ORDER_MODE7,
            _ => &ORDER_MODE234,
        };
        let priority = priority & 0x03;
        let total = order.len() as u8;
        order
            .iter()
            .position(|&(entry_layer, entry_priority)| {
                entry_layer == layer as u8 && entry_priority == priority
            })
            .map_or(0, |index| total - index as u8)
    }

    /// Front-most opaque pixel of one screen at column `x`, with the layers
    /// selected by `screen` and force-disabled by `windows` inside the window
    /// area. Returns the palette index, its priority rank and its `CGADSUB` bit.
    fn compose(
        &self,
        x: usize,
        screen: u8,
        windows: u8,
        line: &LineBuffers,
    ) -> Option<(u16, u8, u8)> {
        let mut best_rank = 0u8;
        let mut result = None;

        for layer in 0..4 {
            if screen & (1 << layer) == 0 || line.bg_index[layer][x] == 0 {
                continue;
            }
            if windows & (1 << layer) != 0 && self.layer_window(layer, x as u16) {
                continue;
            }
            let rank = self.layer_rank(layer, line.bg_priority[layer][x]);
            if rank > best_rank {
                best_rank = rank;
                result = Some((line.bg_index[layer][x], rank, 1 << layer));
            }
        }

        if screen & 0x10 != 0
            && line.obj_index[x] != 0
            && (windows & 0x10 == 0 || !self.layer_window(4, x as u16))
        {
            let rank = self.layer_rank(4, line.obj_priority[x]);
            if rank > best_rank {
                result = Some((line.obj_index[x], rank, 0x10));
            }
        }

        result
    }

    /// Decode one background scanline into palette indices, one span per tile.
    fn build_bg_line(
        &mut self,
        layer: usize,
        y: u16,
        mode: u8,
        index: &mut [u16; SCREEN_WIDTH],
        priority: &mut [u8; SCREEN_WIDTH],
    ) {
        let Some((bpp, palette_base)) = Self::bg_format(mode, layer) else {
            return;
        };

        let screen = self.bgsc[layer];
        let size = screen & 0x03;
        let base_words = u16::from(screen & 0xFC) << 8;

        // 16x16 background tiles use four 8x8 characters; the low map bits
        // select the quadrant (bsnes background.cpp).
        let tile_size = if self.bgmode & (1 << (4 + layer)) != 0 {
            16usize
        } else {
            8usize
        };
        let shift = if tile_size == 16 { 1 } else { 0 };
        let hsize = 256usize << shift << usize::from(size & 0x01);
        let vsize = 256usize << shift << usize::from((size >> 1) & 0x01);

        let mosaic = u16::from(self.mosaic & 0x0F);
        let sample_y = if mosaic > 0 { y - y % (mosaic + 1) } else { y };
        let sy = (usize::from(sample_y) + usize::from(self.bgvofs[layer])) & (vsize - 1);

        let nibble = if layer.is_multiple_of(2) {
            self.bgnba[layer / 2] & 0x0F
        } else {
            self.bgnba[layer / 2] >> 4
        };
        let char_base = u16::from(nibble) << 12;
        let words_per_tile = (bpp as u16) * 4;

        let tile_y_full = (sy / tile_size) & 0x3F;
        let mut x = 0usize;
        while x < SCREEN_WIDTH {
            let sample_x = if mosaic > 0 {
                let screen_x = x as u16;
                screen_x - screen_x % mosaic
            } else {
                x as u16
            };
            let sx = (usize::from(sample_x) + usize::from(self.bghofs[layer])) & (hsize - 1);
            let span = if mosaic > 0 {
                usize::from(mosaic)
            } else {
                tile_size - sx % tile_size
            }
            .min(SCREEN_WIDTH - x);

            let tile_x_full = (sx / tile_size) & 0x3F;

            let mut map_offset = (tile_y_full & 0x1F) * 32 + (tile_x_full & 0x1F);
            if tile_x_full >= 32 {
                map_offset += 0x400;
            }
            if tile_y_full >= 32 {
                // A 64x64 map puts the bottom pages at +$800; a 32x64 map
                // only has one page per row pair and uses +$400 instead.
                map_offset += if size & 0x01 != 0 { 0x800 } else { 0x400 };
            }

            let entry = self.read_vram_word(base_words.wrapping_add(map_offset as u16));
            let mut character = entry & 0x03FF;
            let palette = (entry >> 10) & 0x07;
            let entry_priority = ((entry >> 13) & 0x01) as u8;
            let hflip = entry & 0x4000 != 0;
            let vflip = entry & 0x8000 != 0;

            if tile_size == 16 {
                if ((sx & 8) != 0) != hflip {
                    character = character.wrapping_add(1);
                }
                if ((sy & 8) != 0) != vflip {
                    character = character.wrapping_add(16);
                }
            }

            let py = if vflip {
                tile_size - 1 - (sy % tile_size)
            } else {
                sy % tile_size
            };
            let tile_addr = char_base.wrapping_add(character.wrapping_mul(words_per_tile));

            // In mosaic mode every pixel of the block samples the same point.
            let mosaic_pixel = if mosaic > 0 {
                let in_tile = sx % tile_size;
                let px = if hflip {
                    tile_size - 1 - in_tile
                } else {
                    in_tile
                };
                Some(tile_pixel(&self.vram, tile_addr, bpp, px & 7, py & 7))
            } else {
                None
            };
            let pixels = if mosaic == 0 {
                Some(tile_row(&self.vram, tile_addr, bpp, py & 7))
            } else {
                None
            };

            for offset in 0..span {
                let pixel = if let Some(value) = mosaic_pixel {
                    value
                } else {
                    let in_tile = sx % tile_size + offset;
                    let px = if hflip {
                        tile_size - 1 - in_tile
                    } else {
                        in_tile
                    };
                    pixels.unwrap_or([0; 8])[px & 7]
                };
                if pixel == 0 {
                    continue;
                }
                index[x + offset] = match bpp {
                    2 => palette_base + palette * 4 + u16::from(pixel),
                    4 => palette * 16 + u16::from(pixel),
                    _ if self.cgwsel & 0x01 != 0 => 0x100 | u16::from(pixel) | (palette << 9),
                    _ => u16::from(pixel),
                };
                priority[x + offset] = entry_priority;
            }
            x += span;
        }
    }

    /// OAM byte index: the first 512 bytes hold sprites, the next 32 bytes hold
    /// the X high bits and sizes. Addresses above `$21F` mirror the extension.
    fn oam_index(address: u16) -> usize {
        let address = address & 0x3FF;
        if address < 0x200 {
            usize::from(address)
        } else {
            0x200 + usize::from(address - 0x200) % 0x20
        }
    }

    fn sprite_extra(&self, sprite: usize) -> u8 {
        self.oam[0x200 + (sprite >> 2)]
    }

    /// Signed 9-bit sprite X coordinate (range -256..=255).
    fn sprite_x(&self, sprite: usize) -> i32 {
        let high = (self.sprite_extra(sprite) >> ((sprite & 3) * 2)) & 1;
        let value = u16::from(self.oam[sprite * 4]) | (u16::from(high) << 8);
        if value & 0x100 != 0 {
            i32::from(value) - 512
        } else {
            i32::from(value)
        }
    }

    /// Sprite size selected by `OBSEL` and the extended OAM size bit.
    fn sprite_size(&self, sprite: usize, small: (u16, u16), large: (u16, u16)) -> (u16, u16) {
        if (self.sprite_extra(sprite) >> ((sprite & 3) * 2 + 1)) & 1 != 0 {
            large
        } else {
            small
        }
    }

    fn mode7_pixel(&self, x: u16, y: u16) -> Option<(u16, u8)> {
        // Mode 7 clips the transformed origin to 10 bits and truncates the
        // scroll terms to tile boundaries before stepping along the line
        // (Mesen2 `RenderTilemapMode7`).
        let clip = |value: i32| {
            if value & 0x2000 != 0 {
                value | !0x3FF
            } else {
                value & 0x3FF
            }
        };

        let hscroll = sign13(self.m7_hofs);
        let vscroll = sign13(self.m7_vofs);
        let center_x = sign13(self.m7[4] as u16);
        let center_y = sign13(self.m7[5] as u16);
        let a = i32::from(self.m7[0]);
        let b = i32::from(self.m7[1]);
        let c = i32::from(self.m7[2]);
        let d = i32::from(self.m7[3]);

        let screen_x = if self.m7sel & 0x01 != 0 {
            255 - i32::from(x)
        } else {
            i32::from(x)
        };
        let screen_y = if self.m7sel & 0x02 != 0 {
            255 - i32::from(y)
        } else {
            i32::from(y)
        };

        let origin_x = ((a * clip(hscroll - center_x)) & !63)
            + ((b * screen_y) & !63)
            + ((b * clip(vscroll - center_y)) & !63)
            + (center_x << 8);
        let origin_y = ((c * clip(hscroll - center_x)) & !63)
            + ((d * screen_y) & !63)
            + ((d * clip(vscroll - center_y)) & !63)
            + (center_y << 8);

        let mut px = (origin_x + a * screen_x) >> 8;
        let mut py = (origin_y + c * screen_x) >> 8;

        let large_map = self.m7sel & 0x80 != 0;
        if large_map && (!(0..=0x3FF).contains(&px) || !(0..=0x3FF).contains(&py)) {
            if self.m7sel & 0x40 == 0 {
                return None;
            }
            px = 0;
            py = 0;
        }
        px &= 0x3FF;
        py &= 0x3FF;

        let tile = self.vram[((((py >> 3) & 0x7F) * 128 + ((px >> 3) & 0x7F)) * 2) as usize];
        let address = usize::from(tile) * 128 + ((py & 7) * 16 + (px & 7) * 2 + 1) as usize;
        let pixel = self.vram[address];

        if pixel == 0 {
            None
        } else if self.cgwsel & 0x01 != 0 {
            Some((0x100 | u16::from(pixel), 3))
        } else {
            Some((u16::from(pixel), 3))
        }
    }

    /// Decode the sprite pixels covering one scanline, keeping the winning
    /// sprite per column (highest priority number, then lowest OAM index).
    fn build_sprite_line(
        &self,
        y: u16,
        index: &mut [u16; SCREEN_WIDTH],
        priority: &mut [u8; SCREEN_WIDTH],
    ) {
        // OBSEL bits 0-2 select an 8 KiB word page for sprite characters.
        let base_words = u16::from(self.obsel & 0x07) << 13;
        let page_offset = (u16::from((self.obsel >> 3) & 0x03) + 1) << 12;
        let sample_y = y;

        for sprite in 0..128 {
            let offset = sprite * 4;
            let sx = self.sprite_x(sprite);
            let sy = i32::from(self.oam[offset + 1]);
            let (width, height) =
                self.sprite_size(sprite, small_size(self.obsel), large_size(self.obsel));
            let draw_x = if sx == -256 { 0 } else { sx };
            if draw_x + i32::from(width) <= 0 || draw_x > 255 {
                continue;
            }
            let y_gap = (i32::from(sample_y) - sy) & 0xFF;
            if y_gap >= i32::from(height) {
                continue;
            }

            let attr = self.oam[offset + 3];
            let palette = (attr >> 1) & 0x07;
            let entry_priority = (attr >> 4) & 0x03;
            let hflip = attr & 0x40 != 0;
            let vflip = attr & 0x80 != 0;
            let tile = self.oam[offset + 2];
            let page = attr & 0x01 != 0;
            let local_y = if vflip {
                i32::from(height) - 1 - y_gap
            } else {
                y_gap
            };

            let first_x = draw_x.max(0);
            let last_x = (draw_x + i32::from(width) - 1).min(255);
            let mut screen_x = first_x;
            while screen_x <= last_x {
                let block_start = screen_x as u16;
                let span_end = screen_x + 1;
                let local_x = i32::from(block_start) - draw_x;
                if local_x >= 0 && local_x < i32::from(width) {
                    let mirrored_x = if hflip {
                        i32::from(width) - 1 - local_x
                    } else {
                        local_x
                    };
                    let column = (i32::from(tile & 0x0F) + mirrored_x / 8) & 0x0F;
                    let row = (i32::from(tile >> 4) + local_y / 8) & 0x0F;
                    let tile_index = (row << 4 | column) as u16;
                    let tile_addr = base_words
                        .wrapping_add(tile_index * 16)
                        .wrapping_add(if page { page_offset } else { 0 });
                    let pixel = tile_pixel(
                        &self.vram,
                        tile_addr,
                        4,
                        (mirrored_x & 7) as usize,
                        (local_y & 7) as usize,
                    );
                    if pixel != 0 {
                        let entry = 128 + u16::from(palette) * 16 + u16::from(pixel);
                        for target in screen_x..span_end {
                            let column_index = target as usize;
                            if index[column_index] == 0 || entry_priority > priority[column_index] {
                                index[column_index] = entry;
                                priority[column_index] = entry_priority;
                            }
                        }
                    }
                }
                screen_x = span_end;
            }
        }
    }

    /// Pixel format of a background layer for the given mode.
    fn bg_format(mode: u8, layer: usize) -> Option<(usize, u16)> {
        match (mode, layer) {
            (0, _) => Some((2, layer as u16 * 0x20)),
            (1, 0 | 1) => Some((4, 0)),
            (1, 2) => Some((2, 0)),
            (2, 0 | 1) => Some((4, 0)),
            (3, 0 | 1) => Some((8, 0)),
            (4, 0) => Some((8, 0)),
            (4, 1) => Some((4, 0)),
            (5 | 6, 0 | 1) => Some((4, 0)),
            _ => None,
        }
    }

    /// Resolve a palette index into RGB, including the CGRAM-less direct-color
    /// encoding used by 256-color backgrounds.
    fn palette_rgb(&self, index: u16) -> (u8, u8, u8) {
        if index & 0x100 != 0 {
            let pixel = (index & 0xFF) as u8;
            let palette = ((index >> 9) & 0x07) as u8;
            let red = ((pixel & 0x07) << 2) | ((palette & 0x01) << 1);
            let green = (((pixel >> 3) & 0x07) << 2) | (palette & 0x02);
            let blue = (((pixel >> 6) & 0x03) << 3) | (palette & 0x04);
            return (red << 3, green << 3, blue << 3);
        }
        bgr555_to_rgb(self.cgram[usize::from(index) & 0xFF])
    }

    /// Whether color math applies at this column, from `CGWSEL` bits 5-4
    /// (0 = always, 1 = inside the math window, 2 = outside, 3 = never).
    fn math_enabled(&self, inside: bool) -> bool {
        match (self.cgwsel >> 4) & 0x03 {
            0 => true,
            1 => inside,
            2 => !inside,
            _ => false,
        }
    }

    /// Resolve the winning main pixel into RGB: color math against the sub
    /// screen (or the fixed `COLDATA` color) and the master brightness.
    fn shade(
        &self,
        main: Option<(u16, u8, u8)>,
        sub: Option<(u16, u8, u8)>,
        x: u16,
    ) -> (u8, u8, u8) {
        // A transparent main pixel is the backdrop, which is still eligible
        // for color math when `CGADSUB` bit 5 is set.
        let (index, _rank, math_bit) = main.unwrap_or((0, 0, 0x20));
        let (mut r, mut g, mut b) = self.palette_rgb(index);

        let inside = self.math_window(x);
        let fixed = (
            (self.coldata[0] << 3) | (self.coldata[0] >> 2),
            (self.coldata[1] << 3) | (self.coldata[1] >> 2),
            (self.coldata[2] << 3) | (self.coldata[2] >> 2),
        );

        let mut half = self.cgadsub & 0x40 != 0;
        let clip = (self.cgwsel >> 6) & 0x03;
        let clipped = match clip {
            1 => !inside,
            2 => inside,
            3 => true,
            _ => false,
        };
        if clipped {
            r = 0;
            g = 0;
            b = 0;
            if clip != 3 {
                half = false;
            }
        }

        if self.cgadsub & math_bit != 0 && self.math_enabled(inside) {
            let use_sub = self.cgwsel & 0x02 != 0;
            let (operand, halve) = if use_sub {
                match sub {
                    Some((sub_index, sub_rank, _)) if sub_rank > 0 => {
                        (self.palette_rgb(sub_index), half)
                    }
                    // Nothing on the sub screen: hardware falls back to the
                    // fixed color and disables the halve step.
                    _ => (fixed, false),
                }
            } else {
                (fixed, half)
            };
            let subtract = self.cgadsub & 0x80 != 0;
            r = math(r, operand.0, subtract, halve);
            g = math(g, operand.1, subtract, halve);
            b = math(b, operand.2, subtract, halve);
        }

        let brightness = u16::from(self.inidisp & 0x0F) + 1;
        (
            (u16::from(r) * brightness / 16) as u8,
            (u16::from(g) * brightness / 16) as u8,
            (u16::from(b) * brightness / 16) as u8,
        )
    }

    fn window_inside(&self, window: usize, x: u16) -> bool {
        let left = u16::from(self.wh[window * 2]);
        let right = u16::from(self.wh[window * 2 + 1]);
        let x = x & 0xff;
        // A zero or negative width means an empty inside area (fullsnes).
        left <= right && x >= left && x <= right
    }

    /// Window enable/invert field for a BG/OBJ layer and window number.
    fn window_field(&self, layer: usize, window: usize) -> u8 {
        let (selector, shift) = match layer {
            0 => (self.w12sel, 0),
            1 => (self.w12sel, 2),
            2 => (self.w34sel, 0),
            3 => (self.w34sel, 2),
            // `WOBJSEL` bits 0-3 hold the two OBJ windows.
            _ => (self.wobjsel, 0),
        };
        (selector >> (shift + window * 2)) & 0x03
    }

    /// Window enable/invert field for the color-math window, taken from the
    /// upper half of `WOBJSEL`.
    fn math_window_field(&self, window: usize) -> u8 {
        (self.wobjsel >> (4 + window * 2)) & 0x03
    }

    /// Combine the two window areas with the given mask logic. Per fullsnes,
    /// the logic is only applied when both windows are enabled; a single
    /// enabled window is used as-is and two disabled windows produce an empty
    /// area.
    fn window_area(&self, fields: [u8; 2], logic: u8, x: u16) -> bool {
        // Each two-bit field is bit 0 = invert, bit 1 = enable. A window
        // whose edges do not form a valid range covers no columns at all.
        let window_mask = |field: u8, window: usize| {
            if field & 0x02 == 0 {
                return None;
            }
            let left = self.wh[window * 2];
            let right = self.wh[window * 2 + 1];
            if left >= right {
                return None;
            }
            Some(self.window_inside(window, x) != (field & 0x01 != 0))
        };
        let masks = [window_mask(fields[0], 0), window_mask(fields[1], 1)];
        match (masks[0], masks[1]) {
            (None, None) => false,
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (Some(a), Some(b)) => match logic & 0x03 {
                0 => a || b,
                1 => a && b,
                2 => a ^ b,
                _ => !(a ^ b),
            },
        }
    }

    fn layer_window(&self, layer: usize, x: u16) -> bool {
        let logic = if layer == 4 {
            self.wobjlog & 0x03
        } else {
            (self.wbglog >> (layer * 2)) & 0x03
        };
        self.window_area(
            [self.window_field(layer, 0), self.window_field(layer, 1)],
            logic,
            x,
        )
    }

    /// The color-math window uses the `MATH` fields of `WOBJSEL` and the
    /// `WOBJLOG` bits 2-3 for its mask logic.
    fn math_window(&self, x: u16) -> bool {
        let fields = [self.math_window_field(0), self.math_window_field(1)];
        // A window whose left and right edges coincide covers no columns.
        // Treat it as unrestricted instead of masking a single column, which
        // is what the games that leave a flat window buffer expect.
        let mut active = false;
        let mut degenerate = true;
        for (window, field) in fields.iter().enumerate() {
            if field & 0x02 != 0 {
                active = true;
                if self.wh[window * 2] < self.wh[window * 2 + 1] {
                    degenerate = false;
                }
            }
        }
        if active && degenerate {
            return true;
        }
        self.window_area(fields, self.wobjlog >> 2, x)
    }
}

/// Small sprite size selected by `OBSEL` bits 5-7.
fn small_size(obsel: u8) -> (u16, u16) {
    match (obsel >> 5) & 0x07 {
        0..=2 => (8, 8),
        3 | 4 => (16, 16),
        5 => (32, 32),
        _ => (16, 32),
    }
}

/// Large sprite size selected by `OBSEL` bits 5-7.
fn large_size(obsel: u8) -> (u16, u16) {
    match (obsel >> 5) & 0x07 {
        0 => (16, 16),
        1 => (32, 32),
        2 => (64, 64),
        3 => (32, 32),
        4 => (64, 64),
        5 => (64, 64),
        6 => (32, 64),
        _ => (32, 32),
    }
}

fn sign13(value: u16) -> i32 {
    let value = i32::from(value & 0x1FFF);
    if value & 0x1000 != 0 {
        value - 0x2000
    } else {
        value
    }
}

fn math(channel: u8, fixed: u8, subtract: bool, half: bool) -> u8 {
    let value = u16::from(channel);
    let fixed = u16::from(fixed);
    let result = if subtract {
        value.saturating_sub(fixed)
    } else {
        (value + fixed).min(255)
    };
    if half {
        (result / 2) as u8
    } else {
        result as u8
    }
}

fn bgr555_to_rgb(color: u16) -> (u8, u8, u8) {
    // Replicate the high bits so 31 maps to full white instead of 248.
    let expand = |value: u16| (((value & 0x1F) << 3) | ((value & 0x1F) >> 2)) as u8;
    (expand(color), expand(color >> 5), expand(color >> 10))
}

fn tile_row(vram: &[u8], base: u16, bpp: usize, y: usize) -> [u8; 8] {
    let mut row = [0u8; 8];
    for (x, pixel) in row.iter_mut().enumerate() {
        *pixel = tile_pixel(vram, base, bpp, x, y);
    }
    row
}

fn tile_pixel(vram: &[u8], base: u16, bpp: usize, x: usize, y: usize) -> u8 {
    let plane_pairs = bpp / 2;
    let mut pixel = 0u8;

    // Planes are stored in pairs: pair 0 covers rows 0-7, pair 1 the next
    // 8 rows (bytes 10h-1Fh), and so on (fullsnes VRAM tile format).
    for pair in 0..plane_pairs {
        let word = base.wrapping_add((pair * 8 + y) as u16);
        // VRAM holds 32K words; higher addresses mirror the same array.
        let index = usize::from(word & 0x7FFF) * 2;
        let low = vram[index];
        let high = vram[index + 1];
        let bit = 7 - x;
        pixel |= ((low >> bit) & 1) << (pair * 2);
        pixel |= ((high >> bit) & 1) << (pair * 2 + 1);
    }

    pixel
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ppu() -> Ppu {
        let mut ppu = Ppu::new(Timing::ntsc());
        ppu.write_register(0x2100, 0x0F); // full brightness, display on
        ppu
    }

    fn set_color(ppu: &mut Ppu, index: u8, red: u8, green: u8, blue: u8) {
        let word =
            u16::from(red & 0x1F) | (u16::from(green & 0x1F) << 5) | (u16::from(blue & 0x1F) << 10);
        ppu.write_register(0x2121, index);
        ppu.write_register(0x2122, word as u8);
        ppu.write_register(0x2122, (word >> 8) as u8);
    }

    #[test]
    fn forced_blank_renders_black() {
        let mut ppu = ppu();
        ppu.write_register(0x2100, 0x80);

        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert!(ppu.framebuffer().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn backdrop_uses_cgram_color_zero() {
        let mut ppu = ppu();
        set_color(&mut ppu, 0, 31, 0, 0);

        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert_eq!(&ppu.framebuffer()[..3], &[255, 0, 0]);
    }

    #[test]
    fn simple_2bpp_tile_renders() {
        let mut ppu = ppu();
        set_color(&mut ppu, 1, 0, 31, 0);

        // BG1 map at word 0, characters at word 0x1000.
        ppu.write_register(0x2107, 0x00); // 32x32 map at word 0
        ppu.write_register(0x210B, 0x01); // BG1 characters at 0x1000 words
        ppu.write_register(0x2105, 0x00); // mode 0
        ppu.write_register(0x212C, 0x01); // BG1 on main screen

        // Tile 0: all pixels = 1 (plane 0 set).
        for row in 0..8 {
            ppu.write_vram_word(0x1000 + row, 0x00FF);
        }
        // Map entry for tile 0, palette 0, priority 0.
        ppu.write_vram_word(0, 0x0000);

        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert_eq!(&ppu.framebuffer()[..3], &[0, 255, 0]);
    }

    #[test]
    fn coldata_selects_channels_with_the_high_bits() {
        let mut ppu = ppu();
        ppu.write_register(0x2132, 0x20 | 0x10);
        ppu.write_register(0x2132, 0x40 | 0x08);
        assert_eq!(ppu.coldata, [0x10, 0x08, 0]);

        ppu.write_register(0x2132, 0xE0 | 0x04);
        assert_eq!(ppu.coldata, [0x04, 0x04, 0x04]);
    }

    #[test]
    fn color_math_only_applies_to_enabled_layers() {
        let mut ppu = ppu();
        set_color(&mut ppu, 1, 0, 31, 0); // BG1 green
        ppu.write_register(0x2107, 0x00);
        ppu.write_register(0x210B, 0x01);
        ppu.write_register(0x2105, 0x00);
        ppu.write_register(0x212C, 0x01);
        for row in 0..8 {
            ppu.write_vram_word(0x1000 + row, 0x00FF);
        }
        ppu.write_vram_word(0, 0x0000);

        ppu.write_register(0x2132, 0x20 | 31); // fixed color: red
        ppu.write_register(0x2130, 0x00); // fixed color, no window
        ppu.write_register(0x2131, 0x01); // math + BG1
        ppu.advance_to(ppu.timing().line_cycles * 2);
        assert_eq!(&ppu.framebuffer()[..3], &[255, 255, 0]);

        ppu.write_register(0x2131, 0x10); // math + OBJ only
        let next_frame = (u64::from(ppu.timing().lines_per_frame) + 2) * ppu.timing().line_cycles;
        ppu.advance_to(next_frame);
        assert_eq!(&ppu.framebuffer()[..3], &[0, 255, 0]);
    }

    #[test]
    fn mode7_identity_matrix_samples_the_map() {
        let mut ppu = ppu();
        set_color(&mut ppu, 5, 31, 0, 0);

        ppu.write_register(0x2105, 0x07);
        ppu.write_register(0x212C, 0x01);
        ppu.write_register(0x211A, 0x00);

        // M7A = 0x0100, M7D = 0x0100, B = C = 0, centers at 0.
        let matrix = [0x00u8, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
        for (offset, value) in matrix.into_iter().enumerate() {
            ppu.write_register(0x211B + offset as u16, value);
        }
        ppu.write_register(0x211F, 0x00);
        ppu.write_register(0x211F, 0x00);
        ppu.write_register(0x2120, 0x00);
        ppu.write_register(0x2120, 0x00);

        // Map entry 0 -> tile 1. Tile 1, pixel (0, 0) -> color 5.
        ppu.write_register(0x2115, 0x80);
        ppu.write_register(0x2116, 0x00);
        ppu.write_register(0x2117, 0x00);
        ppu.write_register(0x2118, 0x01);
        ppu.write_register(0x2119, 0x00);
        ppu.write_register(0x2116, 0x40);
        ppu.write_register(0x2117, 0x00);
        ppu.write_register(0x2118, 0x00);
        ppu.write_register(0x2119, 0x05);

        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert_eq!(&ppu.framebuffer()[..3], &[255, 0, 0]);
    }

    #[test]
    fn mosaic_quantises_background_sampling() {
        let mut ppu = ppu();
        set_color(&mut ppu, 1, 31, 0, 0);

        ppu.write_register(0x2107, 0x00);
        ppu.write_register(0x210B, 0x01);
        ppu.write_register(0x2105, 0x00);
        ppu.write_register(0x212C, 0x01);
        ppu.write_register(0x2106, 0x71); // BG1 mosaic, size 8

        for row in 0..8 {
            ppu.write_vram_word(0x1000 + row, 0x00FF);
        }
        ppu.write_vram_word(0, 0x0000);

        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert_eq!(&ppu.framebuffer()[..3], &[255, 0, 0]);
        assert_eq!(&ppu.framebuffer()[7 * 3..8 * 3], &[255, 0, 0]);
    }

    #[test]
    fn direct_color_bypasses_cgram_for_256_color_backgrounds() {
        let mut ppu = ppu();
        set_color(&mut ppu, 7, 0, 31, 0); // would be green through CGRAM

        ppu.write_register(0x2105, 0x03); // mode 3, BG1 is 256-color
        ppu.write_register(0x212C, 0x01);
        ppu.write_register(0x2107, 0x04); // map at word $0400
        ppu.write_register(0x210B, 0x01); // characters at word $1000
        ppu.write_vram_word(0x0400, 0x0000); // map entry: tile 0, palette 0

        // Pixel value 7 at (0,0): planes 0 and 1 live in the first plane
        // pair, plane 2 starts at word +8 (fullsnes tile layout).
        ppu.write_vram_word(0x1000, 0x8080);
        ppu.write_vram_word(0x1008, 0x0080);

        ppu.write_register(0x2130, 0x01); // direct color
        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert_eq!(&ppu.framebuffer()[..3], &[224, 0, 0]);
    }

    #[test]
    fn vram_address_uses_all_seven_high_bits() {
        let mut ppu = ppu();
        ppu.write_register(0x2116, 0x34);
        ppu.write_register(0x2117, 0x20);
        assert_eq!(ppu.vmaddr, 0x2034);

        ppu.write_register(0x2118, 0xAB);
        assert_eq!(ppu.vram()[0x2034 * 2], 0xAB);
    }

    #[test]
    fn vmain_rotates_the_low_address_bits() {
        let mut ppu = ppu();
        ppu.write_register(0x2115, 0x04); // 8-bit translation
        ppu.write_register(0x2116, 0x15);
        ppu.write_register(0x2117, 0x00);
        assert_eq!(ppu.vram_address(), 0x00A8);

        ppu.write_register(0x2118, 0x5A);
        assert_eq!(ppu.vram()[0x00A8 * 2], 0x5A);
        // The address register itself is not translated (only incremented).
        assert_eq!(ppu.vmaddr, 0x0016);
    }

    #[test]
    fn vram_reads_increment_according_to_vmain() {
        let mut ppu = ppu();
        ppu.write_vram_word(0x0010, 0x1234);
        ppu.write_register(0x2115, 0x80); // increment after the high byte
        ppu.write_register(0x2116, 0x10);
        ppu.write_register(0x2117, 0x00);

        assert_eq!(ppu.read_register(0x213A), 0x00);
        assert_eq!(ppu.vmaddr, 0x0011);
        assert_eq!(ppu.read_register(0x213A), 0x12);
    }

    #[test]
    fn vram_covers_all_32768_words() {
        let mut ppu = ppu();

        ppu.write_vram_word(0x4000, 0x1234);
        ppu.write_vram_word(0x7fff, 0xabcd);

        assert_eq!(ppu.read_vram_word(0x4000), 0x1234);
        assert_eq!(ppu.read_vram_word(0x7fff), 0xabcd);
    }

    fn set_sprite(ppu: &mut Ppu, index: usize, x: u8, y: u8, tile: u8, attr: u8) {
        ppu.write_register(0x2102, (index * 4) as u8);
        ppu.write_register(0x2103, 0);
        for byte in [x, y, tile, attr] {
            ppu.write_register(0x2104, byte);
        }
    }

    /// Sprite with a single opaque pixel at its top-left corner.
    fn solid_sprite_tile(ppu: &mut Ppu, tile: u8) {
        ppu.write_vram_word(u16::from(tile) * 16, 0x0080);
    }

    /// Fill BG1 with a single opaque colour across the whole visible line.
    fn fill_bg1(ppu: &mut Ppu) {
        ppu.write_register(0x2107, 0x00);
        ppu.write_register(0x210B, 0x01);
        ppu.write_register(0x2105, 0x00);
        ppu.write_register(0x212C, 0x01);
        for entry in 0..32 {
            ppu.write_vram_word(entry, 0);
        }
        for row in 0..8 {
            ppu.write_vram_word(0x1000 + row, 0x0080);
        }
    }

    #[test]
    fn subscreen_color_math_adds_the_sub_layer() {
        let mut ppu = ppu();
        set_color(&mut ppu, 1, 0, 31, 0); // main BG1: green
        set_color(&mut ppu, 0x21, 31, 0, 0); // sub BG2: red (mode 0 palette 1)
        fill_bg1(&mut ppu);

        // Sub screen: BG2 with its own map and tile base.
        ppu.write_register(0x212D, 0x02);
        ppu.write_register(0x2108, 0x04);
        ppu.write_register(0x210B, 0x11);
        for entry in 0..32 {
            // Priority 1 so the sub pixel outranks the main BG1 pixel.
            ppu.write_vram_word(0x400 + entry, 0x2000);
        }

        ppu.write_register(0x2130, 0x02); // sub BG/OBJ enable, math always
        ppu.write_register(0x2131, 0x01); // add, math on BG1

        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert_eq!(&ppu.framebuffer()[..3], &[255, 255, 0]);
    }

    #[test]
    fn backdrop_gets_color_math_from_the_fixed_color() {
        let mut ppu = ppu();
        // Nothing on the main screen: only the backdrop is visible.
        ppu.write_register(0x2130, 0x00); // fixed color, math always
        ppu.write_register(0x2131, 0x20); // add to the backdrop
        ppu.write_register(0x2132, 0x20 | 31); // red

        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert_eq!(&ppu.framebuffer()[..3], &[255, 0, 0]);
    }

    #[test]
    fn math_window_uses_the_upper_wobjsel_fields() {
        let mut ppu = ppu();
        ppu.write_register(0x2125, 0x20); // math window 1 enabled, not inverted
        ppu.write_register(0x2126, 0x10);
        ppu.write_register(0x2127, 0x20);
        ppu.write_register(0x212B, 0x00);

        assert!(ppu.math_window(0x18));
        assert!(!ppu.math_window(0x08));
        assert_eq!(ppu.math_window_field(0), 2);

        ppu.write_register(0x2125, 0x00);
        assert!(!ppu.math_window(0x18));
    }

    #[test]
    fn color_math_can_be_limited_to_the_math_window() {
        let mut ppu = ppu();
        set_color(&mut ppu, 1, 0, 31, 0);
        fill_bg1(&mut ppu);

        ppu.write_register(0x2125, 0x20); // math window 1: inside
        ppu.write_register(0x2126, 0x10);
        ppu.write_register(0x2127, 0x1F);
        ppu.write_register(0x2130, 0x10); // math only inside the window
        ppu.write_register(0x2131, 0x01);
        ppu.write_register(0x2132, 0x20 | 31); // fixed colour: red

        ppu.advance_to(ppu.timing().line_cycles * 2);

        // Inside the window (x = 0x18): green + red.
        let inside = (0x18 * 3) as usize;
        assert_eq!(&ppu.framebuffer()[inside..inside + 3], &[255, 255, 0]);
        // Outside the window (x = 0x08): raw green.
        let outside = (0x08 * 3) as usize;
        assert_eq!(&ppu.framebuffer()[outside..outside + 3], &[0, 255, 0]);
    }

    #[test]
    fn forced_black_applies_inside_the_math_window() {
        let mut ppu = ppu();
        set_color(&mut ppu, 1, 0, 31, 0);
        fill_bg1(&mut ppu);

        ppu.write_register(0x2125, 0x20); // math window 1: inside
        ppu.write_register(0x2126, 0x10);
        ppu.write_register(0x2127, 0x1F);
        ppu.write_register(0x2130, 0x80); // force black inside the window

        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert_eq!(&ppu.framebuffer()[(0x18 * 3) as usize..][..3], &[0, 0, 0]);
        assert_eq!(&ppu.framebuffer()[(0x08 * 3) as usize..][..3], &[0, 255, 0]);
    }

    #[test]
    fn sprite_priority_uses_the_priority_number() {
        let mut ppu = ppu();
        set_color(&mut ppu, 129, 31, 0, 0); // sprite palette 0, colour 1: red
        set_color(&mut ppu, 145, 0, 31, 0); // sprite palette 1, colour 1: green

        ppu.write_register(0x212C, 0x10); // OBJ on the main screen
        solid_sprite_tile(&mut ppu, 1);
        solid_sprite_tile(&mut ppu, 2);

        // Priority 3 is the highest, so the first sprite wins even though the
        // second one has the lower OAM index.
        set_sprite(&mut ppu, 0, 10, 10, 1, 0x30);
        set_sprite(&mut ppu, 1, 10, 10, 2, 0x02);

        ppu.advance_to(ppu.timing().line_cycles * 12);

        let offset = (10 * SCREEN_WIDTH + 10) * 3;
        assert_eq!(&ppu.framebuffer()[offset..offset + 3], &[255, 0, 0]);
    }

    #[test]
    fn equal_sprite_priority_keeps_the_lower_oam_index() {
        let mut ppu = ppu();
        set_color(&mut ppu, 129, 31, 0, 0);
        set_color(&mut ppu, 145, 0, 31, 0);

        ppu.write_register(0x212C, 0x10);
        solid_sprite_tile(&mut ppu, 1);
        solid_sprite_tile(&mut ppu, 2);

        set_sprite(&mut ppu, 0, 10, 10, 1, 0x00);
        set_sprite(&mut ppu, 1, 10, 10, 2, 0x02);

        ppu.advance_to(ppu.timing().line_cycles * 12);

        let offset = (10 * SCREEN_WIDTH + 10) * 3;
        assert_eq!(&ppu.framebuffer()[offset..offset + 3], &[255, 0, 0]);
    }

    #[test]
    fn window_registers_have_the_correct_addresses() {
        let mut ppu = ppu();
        ppu.write_register(0x2126, 0x10);
        ppu.write_register(0x2127, 0x20);
        ppu.write_register(0x2128, 0x30);
        ppu.write_register(0x2129, 0x40);
        ppu.write_register(0x212A, 0x55);
        ppu.write_register(0x212B, 0xAA);

        assert_eq!(ppu.wh, [0x10, 0x20, 0x30, 0x40]);
        assert_eq!(ppu.wbglog, 0x55);
        assert_eq!(ppu.wobjlog, 0xAA);
    }

    #[test]
    fn window_masks_disable_the_selected_layer() {
        let mut ppu = ppu();
        ppu.write_register(0x2126, 0x10); // window 1 left
        ppu.write_register(0x2127, 0x20); // window 1 right
        ppu.write_register(0x2123, 0x02); // BG1: window 1 enabled, not inverted
        ppu.write_register(0x212E, 0x01); // TMW: disable BG1 inside the window

        assert!(ppu.layer_window(0, 0x18));
        assert!(!ppu.layer_window(0, 0x08));
        assert!(!ppu.layer_window(1, 0x18));

        ppu.write_register(0x2123, 0x03); // inverted: outside is masked
        assert!(!ppu.layer_window(0, 0x18));
        assert!(ppu.layer_window(0, 0x08));
    }

    #[test]
    fn slhv_latches_the_h_and_v_counters() {
        let mut ppu = ppu();
        let line_cycles = ppu.timing().line_cycles;
        ppu.advance_to(line_cycles * 10 + 400);
        ppu.read_register(0x2137);

        let h =
            u16::from(ppu.read_register(0x213C)) | (u16::from(ppu.read_register(0x213C) & 1) << 8);
        let v =
            u16::from(ppu.read_register(0x213D)) | (u16::from(ppu.read_register(0x213D) & 1) << 8);

        assert_eq!(h, 100);
        assert_eq!(v, 10);
    }

    #[test]
    fn hblank_is_reported_after_the_visible_pixels() {
        let mut ppu = ppu();
        let line_cycles = ppu.timing().line_cycles;

        ppu.advance_to(line_cycles / 2);
        assert!(!ppu.in_hblank());

        ppu.advance_to(line_cycles - 1);
        assert!(ppu.in_hblank());
    }

    #[test]
    fn vblank_requests_nmi_once() {
        let mut ppu = ppu();
        let vblank_clock = u64::from(ppu.timing().vblank_line) * ppu.timing().line_cycles;

        ppu.advance_to(vblank_clock);

        assert!(ppu.take_nmi());
        assert!(!ppu.take_nmi());
    }

    #[test]
    fn frame_completes_after_last_scanline() {
        let mut ppu = ppu();
        let frame_clock = u64::from(ppu.timing().lines_per_frame) * ppu.timing().line_cycles;

        ppu.advance_to(frame_clock - 1);
        assert!(!ppu.frame_ready());

        ppu.advance_to(frame_clock);
        assert!(ppu.frame_ready());
    }
}
