//! Scanline renderer, register file and video memory.

use serde::{Deserialize, Serialize};

/// Visible width in pixels.
pub const SCREEN_WIDTH: usize = 256;

/// Size of one BGR555 palette entry in bits.
const BGR555_MASK: u16 = 0x7FFF;

/// Layer order used to break priority ties, matching the hardware.
const PRIORITY_RANK: [[u8; 5]; 4] = [
    [10, 7, 5, 4, 14],
    [15, 11, 8, 6, 18],
    [19, 16, 12, 9, 21],
    [22, 20, 17, 13, 23],
];

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
    hofs_low: [bool; 4],
    vofs_low: [bool; 4],
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
    cgwsel: u8,
    cgadsub: u8,
    coldata: [u8; 3],

    vmain: u8,
    vmaddr: u16,
    vram_latch: u16,
    vram_read_buffer: u16,
    cgaddr: u8,
    cgram_latch: u8,
    cgram_high: bool,
    oamaddr: u16,

    line_counter: u64,
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
            vram: vec![0; 0x8000],
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
            hofs_low: [false; 4],
            vofs_low: [false; 4],
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
            cgwsel: 0,
            cgadsub: 0,
            coldata: [0; 3],

            vmain: 0,
            vmaddr: 0,
            vram_latch: 0,
            vram_read_buffer: 0,
            cgaddr: 0,
            cgram_latch: 0,
            cgram_high: false,
            oamaddr: 0,

            line_counter: 0,
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

    /// Clear the frame-complete flag after the frontend consumed the frame.
    pub fn clear_frame_ready(&mut self) {
        self.frame_ready = false;
    }

    /// Take the latched NMI request, if any.
    pub fn take_nmi(&mut self) -> bool {
        let pending = self.nmi;
        self.nmi = false;
        pending
    }

    /// Render every scanline up to `clock` (absolute master cycles).
    pub fn advance_to(&mut self, clock: u64) {
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
                let value = self.oam[self.oamaddr as usize & 0x1FF];
                self.oamaddr = self.oamaddr.wrapping_add(1) & 0x1FF;
                value
            }
            0x2139 | 0x213A => {
                let value = if addr == 0x2139 {
                    (self.vram_read_buffer & 0x00FF) as u8
                } else {
                    (self.vram_read_buffer >> 8) as u8
                };
                self.vram_read_buffer = self.read_vram_word(self.vmaddr);
                value
            }
            0x213B => {
                let value = self.cgram[self.cgaddr as usize] as u8;
                self.cgaddr = self.cgaddr.wrapping_add(1);
                value
            }
            0x213E => 0,
            0x213F => 0,
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
                self.oam[self.oamaddr as usize & 0x1FF] = value;
                self.oamaddr = self.oamaddr.wrapping_add(1) & 0x1FF;
            }
            0x2105 => self.bgmode = value,
            0x2106 => self.mosaic = value,
            0x2107..=0x210A => self.bgsc[usize::from(addr - 0x2107)] = value,
            0x210B => self.bgnba[0] = value,
            0x210C => self.bgnba[1] = value,
            0x210D..=0x2114 => self.write_bg_scroll(addr, value),
            0x2115 => self.vmain = value,
            0x2116 => self.vmaddr = (self.vmaddr & 0xFF00) | u16::from(value),
            0x2117 => self.vmaddr = (self.vmaddr & 0x00FF) | (u16::from(value & 1) << 8),
            0x2118 => {
                self.write_vram_byte(self.vmaddr, value, false);
                if self.vmain & 0x80 == 0 {
                    self.increment_vram();
                }
            }
            0x2119 => {
                self.write_vram_byte(self.vmaddr, value, true);
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
            0x2126 => self.wbglog = value,
            0x2127 => self.wobjlog = value,
            0x2128..=0x212B => {}
            0x212C => self.tm = value,
            0x212D => self.ts = value,
            0x212E => self.tmw = value,
            0x212F => self.tsw = value,
            0x2130 => self.cgwsel = value,
            0x2131 => self.cgadsub = value,
            0x2132 => {
                let index = match value >> 5 {
                    0 => 0,
                    1 => 1,
                    2 => 2,
                    _ => 3,
                };
                if index < 3 {
                    self.coldata[index] = value & 0x1F;
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
            if self.hofs_low[layer] {
                self.bghofs[layer] = (self.bghofs[layer] & 0x00FF) | (u16::from(value) << 8);
            } else {
                self.bghofs[layer] = (self.bghofs[layer] & 0xFF00) | u16::from(value);
            }
            self.hofs_low[layer] = !self.hofs_low[layer];
        } else {
            if self.vofs_low[layer] {
                self.bgvofs[layer] = (self.bgvofs[layer] & 0x00FF) | (u16::from(value) << 8);
            } else {
                self.bgvofs[layer] = (self.bgvofs[layer] & 0xFF00) | u16::from(value);
            }
            self.vofs_low[layer] = !self.vofs_low[layer];
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

        for x in 0..SCREEN_WIDTH {
            let (r, g, b) = self.pixel_color(x as u16, y);
            let base = start + x * 3;
            self.framebuffer[base] = r;
            self.framebuffer[base + 1] = g;
            self.framebuffer[base + 2] = b;
        }
    }

    fn pixel_color(&self, x: u16, y: u16) -> (u8, u8, u8) {
        let mode = self.bgmode & 0x07;
        let mut best_rank = 0u8;
        let mut index = 0u16;

        for (layer, ranks) in PRIORITY_RANK.iter().enumerate() {
            if self.tm & (1 << layer) == 0 {
                continue;
            }
            if let Some((palette_index, priority)) = self.bg_pixel(layer, x, y, mode) {
                let rank = ranks[usize::from(priority & 3)];
                if rank > best_rank {
                    best_rank = rank;
                    index = palette_index;
                }
            }
        }

        if self.tm & 0x10 != 0
            && let Some((palette_index, priority)) = self.sprite_pixel(x, y)
        {
            let rank = PRIORITY_RANK[usize::from(priority & 3)][4];
            if rank > best_rank {
                best_rank = rank;
                index = palette_index;
            }
        }

        let color = self.cgram[index as usize & 0xFF];
        let (mut r, mut g, mut b) = bgr555_to_rgb(color);

        if self.cgadsub & 0x80 != 0 && self.cgwsel & 0x02 == 0 && best_rank != 0 {
            let fixed = (
                (self.coldata[0] << 3) | (self.coldata[0] >> 2),
                (self.coldata[1] << 3) | (self.coldata[1] >> 2),
                (self.coldata[2] << 3) | (self.coldata[2] >> 2),
            );
            let subtract = self.cgadsub & 0x20 != 0;
            let half = self.cgadsub & 0x40 != 0;
            r = math(r, fixed.0, subtract, half);
            g = math(g, fixed.1, subtract, half);
            b = math(b, fixed.2, subtract, half);
        }

        let brightness = u16::from(self.inidisp & 0x0F) + 1;
        (
            (u16::from(r) * brightness / 16) as u8,
            (u16::from(g) * brightness / 16) as u8,
            (u16::from(b) * brightness / 16) as u8,
        )
    }

    fn bg_pixel(&self, layer: usize, x: u16, y: u16, mode: u8) -> Option<(u16, u8)> {
        if mode == 7 {
            return if layer == 0 {
                self.mode7_pixel(x, y)
            } else {
                None
            };
        }

        let mosaic = u16::from((self.mosaic >> 4) & 0x0F);
        let (x, y) = if mosaic > 0 {
            (x - x % (mosaic + 1), y - y % (mosaic + 1))
        } else {
            (x, y)
        };

        let (bpp, palette_base) = match (mode, layer) {
            (0, _) => (2, layer as u16 * 8),
            (1, 0 | 1) => (4, 0),
            (1, 2) => (2, 0),
            (2, 0 | 1) => (4, 0),
            (3, 0 | 1) => (8, 0),
            (4, 0) => (8, 0),
            (4, 1) => (4, 0),
            (5 | 6, 0 | 1) => (4, 0),
            _ => return None,
        };

        let screen = self.bgsc[layer];
        let size = screen & 0x03;
        let base_words = u16::from(screen & 0xFC) << 8;

        let sx = (x.wrapping_add(self.bghofs[layer])) & 0x03FF;
        let sy = (y.wrapping_add(self.bgvofs[layer])) & 0x03FF;

        let mut tile_x = sx >> 3;
        let mut tile_y = sy >> 3;
        if size & 0x01 != 0 {
            tile_x &= 0x3F;
        } else {
            tile_x &= 0x1F;
        }
        if size & 0x02 != 0 {
            tile_y &= 0x3F;
        } else {
            tile_y &= 0x1F;
        }

        let mut map_offset = (tile_y & 0x1F) * 32 + (tile_x & 0x1F);
        if size & 0x01 != 0 && tile_x >= 32 {
            map_offset += 0x400;
        }
        if size & 0x02 != 0 && tile_y >= 32 {
            map_offset += 0x800;
        }

        let entry = self.read_vram_word(base_words.wrapping_add(map_offset));
        let tile = entry & 0x03FF;
        let palette = (entry >> 10) & 0x07;
        let priority = ((entry >> 13) & 0x03) as u8;
        let hflip = entry & 0x4000 != 0;
        let vflip = entry & 0x8000 != 0;

        let mut px = (sx & 7) as usize;
        let mut py = (sy & 7) as usize;
        if hflip {
            px = 7 - px;
        }
        if vflip {
            py = 7 - py;
        }

        let nibble = if layer.is_multiple_of(2) {
            self.bgnba[layer / 2] & 0x0F
        } else {
            self.bgnba[layer / 2] >> 4
        };
        let char_base = u16::from(nibble) << 12;
        let words_per_tile = (bpp as u16) * 4;
        let tile_addr = char_base.wrapping_add(tile.wrapping_mul(words_per_tile));

        let pixel = tile_pixel(&self.vram, tile_addr, bpp, px, py);
        if pixel == 0 {
            return None;
        }

        let index = match bpp {
            2 => palette_base + palette * 4 + u16::from(pixel),
            4 => palette * 16 + u16::from(pixel),
            _ => u16::from(pixel),
        };
        Some((index, priority))
    }

    fn mode7_pixel(&self, x: u16, y: u16) -> Option<(u16, u8)> {
        let m7x = sign13(self.m7[4] as u16);
        let m7y = sign13(self.m7[5] as u16);
        let hofs = sign13(self.bghofs[0]);
        let vofs = sign13(self.bgvofs[0]);

        let mut screen_x = i32::from(x);
        let mut screen_y = i32::from(y);
        if self.m7sel & 0x01 != 0 {
            screen_x = 255 - screen_x;
        }
        if self.m7sel & 0x02 != 0 {
            screen_y = 255 - screen_y;
        }

        let a = i32::from(self.m7[0]);
        let b = i32::from(self.m7[1]);
        let c = i32::from(self.m7[2]);
        let d = i32::from(self.m7[3]);

        let org_x = screen_x + hofs - m7x;
        let org_y = screen_y + vofs - m7y;

        let vram_x = a * org_x + b * org_y + m7x * 256;
        let vram_y = c * org_x + d * org_y + m7y * 256;

        let mut px = vram_x >> 8;
        let mut py = vram_y >> 8;

        if self.m7sel & 0x80 != 0 {
            px &= 0x3FF;
            py &= 0x3FF;
        } else if !(0..1024).contains(&px) || !(0..1024).contains(&py) {
            return None;
        }

        let tile = self.vram[((((py >> 3) & 0x7F) * 128 + ((px >> 3) & 0x7F)) * 2) as usize];
        let address = usize::from(tile) * 128 + ((py & 7) * 16 + (px & 7) * 2 + 1) as usize;
        let pixel = self.vram[address];

        if pixel == 0 {
            None
        } else {
            Some((u16::from(pixel), 3))
        }
    }

    fn sprite_pixel(&self, x: u16, y: u16) -> Option<(u16, u8)> {
        let mosaic = u16::from(self.mosaic & 0x0F);
        let (x, y) = if mosaic > 0 {
            (x - x % (mosaic + 1), y - y % (mosaic + 1))
        } else {
            (x, y)
        };

        let size_pair = (self.obsel >> 5) & 0x07;
        let (small, large) = match size_pair {
            0 => (8u16, 16u16),
            1 => (8, 32),
            2 => (8, 64),
            3 => (16, 32),
            4 => (16, 64),
            5 => (32, 64),
            _ => (8, 8),
        };
        let base_words = u16::from(self.obsel & 0x07) << 12;

        let mut best: Option<(u16, u8)> = None;

        for sprite in 0..128 {
            let offset = sprite * 4;
            let sx = u16::from(self.oam[offset]);
            let sy = u16::from(self.oam[offset + 1]);
            let tile = u16::from(self.oam[offset + 2]);
            let attr = self.oam[offset + 3];

            let size = if attr & 0x01 != 0 { large } else { small };
            if x < sx || x >= sx + size || y < sy || y >= sy + size {
                continue;
            }

            let mut px = x - sx;
            let mut py = y - sy;
            if attr & 0x40 != 0 {
                px = size - 1 - px;
            }
            if attr & 0x80 != 0 {
                py = size - 1 - py;
            }

            let (cx, cy) = (px / 8, py / 8);
            let tile_index = tile.wrapping_add(cy * 16 + cx);
            let tile_addr = base_words.wrapping_add(tile_index.wrapping_mul(16));
            let pixel = tile_pixel(
                &self.vram,
                tile_addr,
                4,
                (px % 8) as usize,
                (py % 8) as usize,
            );
            if pixel == 0 {
                continue;
            }

            let palette = (attr >> 1) & 0x07;
            let priority = (attr >> 4) & 0x03;
            let index = 128 + u16::from(palette) * 16 + u16::from(pixel);

            best = match best {
                Some((_, best_priority)) if priority <= best_priority => best,
                _ => Some((index, priority)),
            };
        }

        best
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
        (value + fixed).min(31 << 3)
    };
    let result = if half { result / 2 } else { result };
    result.min(255) as u8
}

fn bgr555_to_rgb(color: u16) -> (u8, u8, u8) {
    let r = ((color & 0x1F) << 3) as u8;
    let g = (((color >> 5) & 0x1F) << 3) as u8;
    let b = (((color >> 10) & 0x1F) << 3) as u8;
    (r, g, b)
}

fn tile_pixel(vram: &[u8], base: u16, bpp: usize, x: usize, y: usize) -> u8 {
    let words_per_row = bpp / 2;
    let mut pixel = 0u8;

    for pair in 0..words_per_row {
        let word = base.wrapping_add((y * words_per_row + pair) as u16);
        let index = usize::from(word) * 2;
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

        assert_eq!(&ppu.framebuffer()[..3], &[248, 0, 0]);
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

        assert_eq!(&ppu.framebuffer()[..3], &[0, 248, 0]);
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

        assert_eq!(&ppu.framebuffer()[..3], &[248, 0, 0]);
    }

    #[test]
    fn mosaic_quantises_background_sampling() {
        let mut ppu = ppu();
        set_color(&mut ppu, 1, 31, 0, 0);

        ppu.write_register(0x2107, 0x00);
        ppu.write_register(0x210B, 0x01);
        ppu.write_register(0x2105, 0x00);
        ppu.write_register(0x212C, 0x01);
        ppu.write_register(0x2106, 0x70); // BG mosaic size 8

        for row in 0..8 {
            ppu.write_vram_word(0x1000 + row, 0x00FF);
        }
        ppu.write_vram_word(0, 0x0000);

        ppu.advance_to(ppu.timing().line_cycles * 2);

        assert_eq!(&ppu.framebuffer()[..3], &[248, 0, 0]);
        assert_eq!(&ppu.framebuffer()[7 * 3..8 * 3], &[248, 0, 0]);
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
