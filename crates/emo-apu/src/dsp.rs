//! The S-DSP: eight BRR voices, ADSR/gain envelopes, noise, echo and mixing.
//!
//! The voice pipeline mirrors the hardware state machine described in
//! anomie's S-DSP document: a 12-entry sample ring fed four BRR samples at a
//! time, a 14-bit pitch accumulator, gaussian interpolation and a shared
//! envelope timer.

/// Number of voices.
const VOICES: usize = 8;

/// Envelope maximum (11-bit level).
const ENV_MAX: i32 = 0x7FF;

/// Samples between timer updates for each 5-bit rate value, at the 32 kHz DSP
/// sample clock. Rate 0 stops the counter.
const RATE_PERIODS: [u32; 32] = [
    u32::MAX,
    2048,
    1536,
    1280,
    1024,
    768,
    640,
    512,
    384,
    320,
    256,
    192,
    160,
    128,
    96,
    80,
    64,
    48,
    40,
    32,
    24,
    20,
    16,
    12,
    10,
    8,
    6,
    5,
    4,
    3,
    2,
    1,
];

const GAUSS: [i16; 512] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2,
    2, 2, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 8, 8, 8, 9, 9, 9, 10, 10,
    10, 11, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 15, 16, 16, 17, 17, 18, 19, 19, 20, 20, 21, 21,
    22, 23, 23, 24, 24, 25, 26, 27, 27, 28, 29, 29, 30, 31, 32, 32, 33, 34, 35, 36, 36, 37, 38, 39,
    40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 58, 59, 60, 61, 62, 64, 65,
    66, 67, 69, 70, 71, 73, 74, 76, 77, 78, 80, 81, 83, 84, 86, 87, 89, 90, 92, 94, 95, 97, 99,
    100, 102, 104, 106, 107, 109, 111, 113, 115, 117, 118, 120, 122, 124, 126, 128, 130, 132, 134,
    137, 139, 141, 143, 145, 147, 150, 152, 154, 156, 159, 161, 163, 166, 168, 171, 173, 175, 178,
    180, 183, 186, 188, 191, 193, 196, 199, 201, 204, 207, 210, 212, 215, 218, 221, 224, 227, 230,
    233, 236, 239, 242, 245, 248, 251, 254, 257, 260, 263, 267, 270, 273, 276, 280, 283, 286, 290,
    293, 297, 300, 304, 307, 311, 314, 318, 321, 325, 328, 332, 336, 339, 343, 347, 351, 354, 358,
    362, 366, 370, 374, 378, 381, 385, 389, 393, 397, 401, 405, 410, 414, 418, 422, 426, 430, 434,
    439, 443, 447, 451, 456, 460, 464, 469, 473, 477, 482, 486, 491, 495, 499, 504, 508, 513, 517,
    522, 527, 531, 536, 540, 545, 550, 554, 559, 563, 568, 573, 577, 582, 587, 592, 596, 601, 606,
    611, 615, 620, 625, 630, 635, 640, 644, 649, 654, 659, 664, 669, 674, 678, 683, 688, 693, 698,
    703, 708, 713, 718, 723, 728, 732, 737, 742, 747, 752, 757, 762, 767, 772, 777, 782, 787, 792,
    797, 802, 806, 811, 816, 821, 826, 831, 836, 841, 846, 851, 855, 860, 865, 870, 875, 880, 884,
    889, 894, 899, 904, 908, 913, 918, 923, 927, 932, 937, 941, 946, 951, 955, 960, 965, 969, 974,
    978, 983, 988, 992, 997, 1001, 1005, 1010, 1014, 1019, 1023, 1027, 1032, 1036, 1040, 1045,
    1049, 1053, 1057, 1061, 1066, 1070, 1074, 1078, 1082, 1086, 1090, 1094, 1098, 1102, 1106, 1109,
    1113, 1117, 1121, 1125, 1128, 1132, 1136, 1139, 1143, 1146, 1150, 1153, 1157, 1160, 1164, 1167,
    1170, 1174, 1177, 1180, 1183, 1186, 1190, 1193, 1196, 1199, 1202, 1205, 1207, 1210, 1213, 1216,
    1219, 1221, 1224, 1227, 1229, 1232, 1234, 1237, 1239, 1241, 1244, 1246, 1248, 1251, 1253, 1255,
    1257, 1259, 1261, 1263, 1265, 1267, 1269, 1270, 1272, 1274, 1275, 1277, 1279, 1280, 1282, 1283,
    1284, 1286, 1287, 1288, 1290, 1291, 1292, 1293, 1294, 1295, 1296, 1297, 1297, 1298, 1299, 1300,
    1300, 1301, 1302, 1302, 1303, 1303, 1303, 1304, 1304, 1304, 1304, 1304, 1305, 1305,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
enum EnvelopeState {
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Voice {
    pitch: u16,
    interpolation_pos: i32,
    buffer: [i32; 12],
    buffer_pos: usize,
    brr_address: u16,
    loop_address: u16,
    brr_offset: u8,
    brr_header: u8,
    envelope: i32,
    envelope_counter: u32,
    state: EnvelopeState,
    key_on_delay: u8,
    output: i32,
}

impl Voice {
    const fn new() -> Self {
        Self {
            pitch: 0,
            interpolation_pos: 0,
            buffer: [0; 12],
            buffer_pos: 0,
            brr_address: 0,
            loop_address: 0,
            brr_offset: 1,
            brr_header: 0,
            envelope: 0,
            envelope_counter: 0,
            state: EnvelopeState::Off,
            key_on_delay: 0,
            output: 0,
        }
    }
}

/// The S-DSP register file and voice engine.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Dsp {
    address: u8,
    registers: Vec<u8>,
    voices: [Voice; VOICES],
    /// Voice-end flags, exposed through `$7C`.
    endx: u8,
    noise_lfsr: i16,
    noise_clock: u32,
    echo_offset: u32,
    echo_history: [[i16; 2]; 8],
    echo_history_pos: usize,
}

impl Default for Dsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Dsp {
    /// Create a DSP with all voices off.
    #[must_use]
    pub fn new() -> Self {
        Self {
            address: 0,
            registers: vec![0; 128],
            voices: std::array::from_fn(|_| Voice::new()),
            endx: 0,
            noise_lfsr: 0x4000,
            noise_clock: 0,
            echo_offset: 0,
            echo_history: [[0; 2]; 8],
            echo_history_pos: 0,
        }
    }

    /// Current DSP register address.
    #[must_use]
    pub const fn address(&self) -> u8 {
        self.address
    }

    /// Select a DSP register.
    pub fn set_address(&mut self, value: u8) {
        self.address = value & 0x7F;
    }

    /// Raw register value (for diagnostics and tests).
    #[must_use]
    pub fn register(&self, index: u8) -> u8 {
        self.registers[usize::from(index) & 0x7F]
    }

    /// Master volume, left channel.
    #[must_use]
    pub fn volume_left(&self) -> u8 {
        self.registers[0x0C]
    }

    /// Raw register file, for state dumps and diagnostics.
    #[must_use]
    pub fn raw_registers(&self) -> &[u8] {
        &self.registers
    }

    /// Read the selected register.
    pub fn read_data(&self, _aram: &[u8]) -> u8 {
        let address = usize::from(self.address);

        match address {
            0x7C => self.endx,
            0x00..=0x7F if (address & 0x0F) == 0x08 => {
                let voice = address / 0x10;
                (self.voices[voice].envelope >> 4) as u8
            }
            0x00..=0x7F if (address & 0x0F) == 0x09 => {
                let voice = address / 0x10;
                (self.voices[voice].output >> 8) as u8
            }
            _ => self.registers[address],
        }
    }

    /// Write the selected register.
    pub fn write_data(&mut self, aram: &mut [u8], value: u8) {
        let address = usize::from(self.address);

        // KON, KOFF and the end-flag acknowledge have write side effects; the
        // other writes just update the register file.
        match address {
            0x4C => {
                self.key_on(value, aram);
                return;
            }
            0x5C => {
                self.key_off(value);
                return;
            }
            0x6C => {
                self.registers[address] = value;
                if value & 0x80 != 0 {
                    // Soft reset releases every voice and clears the levels.
                    for voice in &mut self.voices {
                        voice.state = EnvelopeState::Release;
                        voice.envelope = 0;
                        voice.output = 0;
                    }
                }
                return;
            }
            0x7C => {
                // Writing ENDX acknowledges (clears) the selected flags.
                self.endx &= !value;
                return;
            }
            _ => {}
        }

        self.registers[address] = value;
        if (address & 0x0F) <= 0x07 && address <= 0x7F {
            let voice = address / 0x10;
            match address & 0x0F {
                0x02 => {
                    self.voices[voice].pitch =
                        (self.voices[voice].pitch & 0x0F00) | u16::from(value)
                }
                0x03 => {
                    self.voices[voice].pitch =
                        (self.voices[voice].pitch & 0x00FF) | (u16::from(value & 0x3F) << 8)
                }
                _ => {}
            }
        }
    }

    fn key_on(&mut self, mask: u8, aram: &[u8]) {
        let directory = usize::from(self.registers[0x5D]) << 8;

        for voice in 0..VOICES {
            if mask & (1 << voice) == 0 {
                continue;
            }

            let source = usize::from(self.registers[voice * 0x10 + 4]);
            let entry = (directory + source * 4) & 0xFFFF;
            let read = |offset: usize| -> u16 {
                let index = (entry + offset) & 0xFFFF;
                u16::from(aram[index]) | (u16::from(aram[(index + 1) & 0xFFFF]) << 8)
            };
            let state = &mut self.voices[voice];
            state.envelope = 0;
            state.envelope_counter = 0;
            state.state = EnvelopeState::Attack;
            state.key_on_delay = 5;
            state.interpolation_pos = 0;
            state.output = 0;
            state.buffer = [0; 12];
            state.buffer_pos = 0;
            state.brr_offset = 1;
            state.brr_header = 0;
            state.brr_address = read(0);
            // The second word of the directory entry is the loop target.
            state.loop_address = read(2);
            self.endx &= !(1 << voice);
        }
    }

    fn key_off(&mut self, mask: u8) {
        for voice in 0..VOICES {
            if mask & (1 << voice) != 0 && self.voices[voice].state != EnvelopeState::Off {
                self.voices[voice].state = EnvelopeState::Release;
            }
        }
    }

    /// Decode the next four BRR samples into the ring buffer.
    fn decode_voice(&mut self, voice: usize, aram: &[u8]) {
        let state = &mut self.voices[voice];
        let header = aram[usize::from(state.brr_address)];
        state.brr_header = header;
        let range = i32::from((header >> 4) & 0x0F);
        let filter = (header >> 2) & 0x03;

        let first = usize::from(state.brr_address.wrapping_add(u16::from(state.brr_offset)));
        let b0 = aram[first & 0xFFFF];
        let b1 = aram[(first + 1) & 0xFFFF];
        let nibbles = [(b0 >> 4), b0 & 0x0F, (b1 >> 4), b1 & 0x0F];

        let previous1_index = (state.buffer_pos + 11) % 12;
        let previous2_index = (state.buffer_pos + 10) % 12;
        let mut previous1 = state.buffer[previous1_index] >> 1;
        let mut previous2 = state.buffer[previous2_index] >> 1;

        for (index, nibble) in nibbles.iter().enumerate() {
            let raw = if nibble & 0x08 != 0 {
                i32::from(*nibble) - 16
            } else {
                i32::from(*nibble)
            };
            let mut sample = (raw << range) >> 1;
            if range >= 0x0D {
                sample = if sample < 0 { -0x800 } else { 0 };
            }
            match filter {
                0 => {}
                1 => sample += previous1 + ((-previous1) >> 4),
                2 => {
                    sample += (previous1 << 1) + ((-((previous1 << 1) + previous1)) >> 5)
                        - previous2
                        + (previous2 >> 4)
                }
                _ => {
                    sample += (previous1 << 1)
                        + ((-(previous1 + (previous1 << 2) + (previous1 << 3))) >> 6)
                        - previous2
                        + (((previous2 << 1) + previous2) >> 4)
                }
            }

            // The hardware clamps to 16 bits and keeps the sample doubled for
            // interpolation headroom; the filter taps halve it again.
            let stored = clamp16(sample) * 2;
            state.buffer[state.buffer_pos + index] = stored;
            previous2 = previous1;
            previous1 = stored >> 1;
        }

        state.buffer_pos = if state.buffer_pos <= 4 {
            state.buffer_pos + 4
        } else {
            0
        };

        if state.brr_offset >= 7 {
            if state.brr_header & 0x01 != 0 {
                // End block: restart at the directory's loop target.
                state.brr_address = state.loop_address;
                self.endx |= 1 << voice;
                if state.brr_header & 0x02 == 0 {
                    state.state = EnvelopeState::Release;
                    state.envelope = 0;
                }
            } else {
                state.brr_address = state.brr_address.wrapping_add(9);
            }
            state.brr_offset = 1;
        } else {
            state.brr_offset += 2;
        }
    }

    fn interpolate(&self, voice: usize) -> i32 {
        let state = &self.voices[voice];
        let position = ((state.interpolation_pos >> 12) as usize + state.buffer_pos) % 12;
        let offset = ((state.interpolation_pos >> 4) & 0xFF) as usize;

        let tap = |index: usize| state.buffer[(position + index) % 12];
        let partial = ((i32::from(GAUSS[255 - offset]) * tap(0)) >> 11)
            + ((i32::from(GAUSS[511 - offset]) * tap(1)) >> 11)
            + ((i32::from(GAUSS[256 + offset]) * tap(2)) >> 11);
        let partial = i32::from(partial as i16);
        let output = partial + ((i32::from(GAUSS[offset]) * tap(3)) >> 11);
        clamp16(output) & !0x01
    }

    /// Generate one stereo sample at 32 kHz.
    pub fn sample(&mut self, aram: &mut [u8]) -> (i16, i16) {
        self.clock_noise();

        let mut left = 0i32;
        let mut right = 0i32;
        let mut echo_left = 0i32;
        let mut echo_right = 0i32;
        let mut previous_output = 0i32;

        for voice in 0..VOICES {
            if self.voices[voice].state == EnvelopeState::Off {
                self.voices[voice].output = 0;
                previous_output = 0;
                continue;
            }

            let mut pitch = i32::from(self.voices[voice].pitch & 0x3FFF);
            if voice > 0 && self.registers[0x2D] & (1 << voice) != 0 {
                // Pitch modulation: the previous voice's output scales the
                // step by up to +-50%.
                pitch += ((previous_output >> 5) * pitch) >> 10;
            }

            if self.voices[voice].key_on_delay > 0 {
                let state = &mut self.voices[voice];
                state.key_on_delay -= 1;
                state.envelope = 0;
                state.interpolation_pos = if state.key_on_delay & 0x03 != 0 {
                    0x4000
                } else {
                    0
                };
                pitch = 0;
            } else {
                self.step_envelope(voice);
                if self.voices[voice].state == EnvelopeState::Off {
                    previous_output = 0;
                    continue;
                }
            }

            let mut output = self.interpolate(voice);
            if self.registers[0x3D] & (1 << voice) != 0 {
                output = i32::from(self.noise_lfsr.wrapping_mul(2));
            }

            let envelope = self.voices[voice].envelope;
            let voice_output = ((output * envelope) >> 11) & !0x01;
            self.voices[voice].output = voice_output;
            previous_output = voice_output;

            let volume_left = i32::from(self.registers[voice * 0x10] as i8);
            let volume_right = i32::from(self.registers[voice * 0x10 + 1] as i8);
            let voice_left = (voice_output * volume_left) >> 7;
            let voice_right = (voice_output * volume_right) >> 7;
            left = clamp16(left + voice_left);
            right = clamp16(right + voice_right);
            if self.registers[0x4D] & (1 << voice) != 0 {
                echo_left = clamp16(echo_left + voice_left);
                echo_right = clamp16(echo_right + voice_right);
            }

            if self.voices[voice].interpolation_pos >= 0x4000 {
                self.decode_voice(voice, aram);
            }
            self.voices[voice].interpolation_pos =
                (self.voices[voice].interpolation_pos & 0x3FFF) + pitch;
            // Keep the accumulator from running away during pitch modulation.
            self.voices[voice].interpolation_pos = self.voices[voice].interpolation_pos.min(0x7FFF);
        }

        let (echo_in_left, echo_in_right) = self.echo(aram, (echo_left, echo_right));

        let master_left = i32::from(self.registers[0x0C] as i8);
        let master_right = i32::from(self.registers[0x1C] as i8);
        let echo_volume_left = i32::from(self.registers[0x2C] as i8);
        let echo_volume_right = i32::from(self.registers[0x3C] as i8);
        let out_left =
            clamp16(((left * master_left) >> 7) + ((echo_in_left * echo_volume_left) >> 7));
        let out_right =
            clamp16(((right * master_right) >> 7) + ((echo_in_right * echo_volume_right) >> 7));

        if self.registers[0x6C] & 0x40 != 0 {
            // FLG bit 6 mutes the amplifier output.
            return (0, 0);
        }

        (out_left as i16, out_right as i16)
    }

    /// Run the echo unit for one sample: FIR over the ring buffer, add the
    /// feedback and write it back. Returns the filtered echo input.
    fn echo(&mut self, aram: &mut [u8], voice_echo: (i32, i32)) -> (i32, i32) {
        let edl = self.registers[0x7D] & 0x0F;
        let length = u32::from(edl) << 11;

        self.echo_history_pos = (self.echo_history_pos + 1) & 0x07;
        let base = u32::from(self.registers[0x6D]) << 8;
        let pointer = ((base + self.echo_offset) & 0xFFFF) as usize;
        let read = |aram: &[u8], offset: usize| -> i16 {
            let index = (pointer + offset) & 0xFFFF;
            i16::from_le_bytes([aram[index], aram[(index + 1) & 0xFFFF]])
        };
        let left = read(aram, 0) >> 1;
        let right = read(aram, 2) >> 1;
        self.echo_history[self.echo_history_pos] = [left, right];

        let mut filtered_left = 0i32;
        let mut filtered_right = 0i32;
        for tap in 0..8 {
            let coefficient = i32::from(self.registers[0x0F + tap * 0x10] as i8);
            let sample = self.echo_history[(self.echo_history_pos + tap + 1) & 0x07];
            filtered_left += (i32::from(sample[0]) * coefficient) >> 6;
            filtered_right += (i32::from(sample[1]) * coefficient) >> 6;
        }
        let filtered_left = clamp16(filtered_left) & !0x01;
        let filtered_right = clamp16(filtered_right) & !0x01;

        let feedback = i32::from(self.registers[0x0D] as i8);
        let left_out = clamp16(voice_echo.0 + ((filtered_left * feedback) >> 7)) & !0x01;
        let right_out = clamp16(voice_echo.1 + ((filtered_right * feedback) >> 7)) & !0x01;

        if self.registers[0x6C] & 0x20 == 0 && length > 0 {
            aram[pointer] = left_out as u8;
            aram[(pointer + 1) & 0xFFFF] = (left_out >> 8) as u8;
            aram[(pointer + 2) & 0xFFFF] = right_out as u8;
            aram[(pointer + 3) & 0xFFFF] = (right_out >> 8) as u8;
        }

        self.echo_offset = (self.echo_offset + 4) % length.max(4);

        (filtered_left, filtered_right)
    }

    fn clock_noise(&mut self) {
        let rate = usize::from(self.registers[0x6C] & 0x1F);
        let period = RATE_PERIODS[rate];
        if period == u32::MAX {
            return;
        }
        self.noise_clock += 1;
        if self.noise_clock >= period {
            self.noise_clock = 0;
            let new_bit =
                ((self.noise_lfsr.wrapping_shl(14)) ^ (self.noise_lfsr.wrapping_shl(13))) & 0x4000;
            self.noise_lfsr = new_bit ^ (self.noise_lfsr >> 1);
        }
    }

    fn step_envelope(&mut self, voice: usize) {
        let adsr1 = self.registers[voice * 0x10 + 5];
        let adsr2 = self.registers[voice * 0x10 + 6];
        let gain = self.registers[voice * 0x10 + 7];
        let state = &mut self.voices[voice];

        if state.state == EnvelopeState::Release {
            state.envelope = (state.envelope - 8).max(0);
            if state.envelope == 0 {
                state.state = EnvelopeState::Off;
                state.output = 0;
                self.endx |= 1 << voice;
            }
            return;
        }

        let mut envelope = state.envelope;
        let rate;

        if adsr1 & 0x80 != 0 {
            let sustain = adsr2;
            match state.state {
                EnvelopeState::Attack => {
                    if adsr1 & 0x0F == 0x0F {
                        rate = 31;
                        envelope += 1024;
                    } else {
                        rate = (i32::from(adsr1 & 0x0F) << 1) | 1;
                        envelope += 32;
                    }
                }
                EnvelopeState::Decay => {
                    envelope -= ((envelope - 1) >> 8) + 1;
                    rate = i32::from(((adsr1 >> 3) & 0x0E) | 0x10);
                }
                EnvelopeState::Sustain => {
                    envelope -= ((envelope - 1) >> 8) + 1;
                    rate = i32::from(adsr2 & 0x1F);
                }
                _ => return,
            }
            if state.state == EnvelopeState::Decay && (envelope >> 8) == i32::from(sustain >> 5) {
                state.state = EnvelopeState::Sustain;
            }
        } else if gain & 0x80 != 0 {
            rate = i32::from(gain & 0x1F);
            match gain & 0x60 {
                0x00 => envelope -= 32,
                0x20 => envelope -= ((envelope - 1) >> 8) + 1,
                0x40 => envelope += 32,
                _ => {
                    envelope += if envelope < 0x600 { 32 } else { 8 };
                }
            }
        } else {
            // Direct gain: a fixed level, no stepping.
            state.envelope = i32::from(gain & 0x7F) << 4;
            return;
        }

        // Clip rather than wrap; a clipped attack moves on to decay.
        if !(0..=ENV_MAX).contains(&envelope) {
            envelope = envelope.clamp(0, ENV_MAX);
            if state.state == EnvelopeState::Attack {
                state.state = EnvelopeState::Decay;
            }
        }

        state.envelope_counter += 1;
        if state.envelope_counter >= RATE_PERIODS[rate as usize] {
            state.envelope_counter = 0;
            state.envelope = envelope;
        }
    }
}

fn clamp16(value: i32) -> i32 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aram_with_sample(start: u16, header: u8) -> Vec<u8> {
        let mut aram = vec![0u8; 0x10000];
        // Sample directory at $0200, source 0 -> start address.
        aram[0x200] = start as u8;
        aram[0x201] = (start >> 8) as u8;
        // Loop/restart target.
        aram[0x202] = start as u8;
        aram[0x203] = (start >> 8) as u8;
        let offset = usize::from(start);
        aram[offset] = header;
        // Full-scale positive nibbles.
        for byte in &mut aram[offset + 1..offset + 9] {
            *byte = 0x77;
        }
        aram
    }

    fn key_on_voice(dsp: &mut Dsp, aram: &mut [u8], voice: u8) {
        dsp.set_address(0x5D);
        dsp.write_data(aram, 0x02);
        // Native pitch and full volume so the voice advances every output sample.
        dsp.set_address(voice * 0x10 + 0x02);
        dsp.write_data(aram, 0x00);
        dsp.set_address(voice * 0x10 + 0x03);
        dsp.write_data(aram, 0x10);
        dsp.set_address(voice * 0x10);
        dsp.write_data(aram, 0x7F);
        dsp.set_address(voice * 0x10 + 0x01);
        dsp.write_data(aram, 0x7F);
        dsp.set_address(0x4C);
        dsp.write_data(aram, 1 << voice);
    }

    #[test]
    fn brr_decodes_and_doubles_the_samples() {
        let mut dsp = Dsp::new();
        let mut aram = aram_with_sample(0x0300, 0x10);
        key_on_voice(&mut dsp, &mut aram, 0);

        for _ in 0..8 {
            dsp.sample(&mut aram);
        }

        assert!(dsp.voices[0].buffer.iter().all(|sample| *sample == 14));
    }

    #[test]
    fn key_on_starts_an_envelope() {
        let mut dsp = Dsp::new();
        let mut aram = aram_with_sample(0x0300, 0x00);
        dsp.set_address(0x05);
        dsp.write_data(&mut aram, 0x80 | 0x0F);
        key_on_voice(&mut dsp, &mut aram, 0);

        assert_eq!(dsp.voices[0].state, EnvelopeState::Attack);
        for _ in 0..16 {
            dsp.sample(&mut aram);
        }
        assert!(dsp.voices[0].envelope > 0);
    }

    #[test]
    fn adsr_reaches_sustain_and_release_clears_the_voice() {
        let mut dsp = Dsp::new();
        let mut aram = aram_with_sample(0x0300, 0x00);
        dsp.set_address(0x00);
        dsp.write_data(&mut aram, 0x7F); // volume left
        dsp.set_address(0x02);
        dsp.write_data(&mut aram, 0x00); // pitch = $1000
        dsp.set_address(0x03);
        dsp.write_data(&mut aram, 0x10);
        dsp.set_address(0x05);
        dsp.write_data(&mut aram, 0x80 | 0x0F | 0x70); // ADSR, fast attack/decay
        dsp.set_address(0x06);
        dsp.write_data(&mut aram, 0x40); // sustain level 2, sustain rate 0 (stop)
        key_on_voice(&mut dsp, &mut aram, 0);

        for _ in 0..4000 {
            dsp.sample(&mut aram);
        }
        assert_eq!(dsp.voices[0].state, EnvelopeState::Sustain);
        assert!(dsp.voices[0].envelope > 0);

        dsp.set_address(0x5C);
        dsp.write_data(&mut aram, 0x01);
        for _ in 0..1000 {
            dsp.sample(&mut aram);
        }
        assert_eq!(dsp.voices[0].state, EnvelopeState::Off);
        dsp.set_address(0x7C);
        assert_eq!(dsp.read_data(&aram), 0x01);
    }

    #[test]
    fn one_shot_end_block_sets_endx_and_stops_the_voice() {
        let mut dsp = Dsp::new();
        let mut aram = aram_with_sample(0x0300, 0x01); // end, no loop
        key_on_voice(&mut dsp, &mut aram, 0);

        for _ in 0..32 {
            dsp.sample(&mut aram);
        }

        assert_eq!(dsp.voices[0].state, EnvelopeState::Off);
        dsp.set_address(0x7C);
        assert_eq!(dsp.read_data(&aram), 0x01);
    }

    #[test]
    fn loop_block_returns_to_the_loop_address() {
        let mut dsp = Dsp::new();
        let mut aram = aram_with_sample(0x0300, 0x03); // end + loop
        key_on_voice(&mut dsp, &mut aram, 0);

        for _ in 0..32 {
            dsp.sample(&mut aram);
        }

        assert_ne!(dsp.voices[0].state, EnvelopeState::Off);
        assert_eq!(dsp.voices[0].brr_address, 0x0300);
    }

    #[test]
    fn mute_bit_silences_the_mixer() {
        let mut dsp = Dsp::new();
        let mut aram = aram_with_sample(0x0300, 0xC3);
        dsp.set_address(0x00);
        dsp.write_data(&mut aram, 0x7F);
        dsp.set_address(0x0C);
        dsp.write_data(&mut aram, 0x7F);
        dsp.set_address(0x02);
        dsp.write_data(&mut aram, 0x00);
        dsp.set_address(0x03);
        dsp.write_data(&mut aram, 0x10);
        dsp.set_address(0x05);
        dsp.write_data(&mut aram, 0x80 | 0x0F | 0x70);
        dsp.set_address(0x06);
        dsp.write_data(&mut aram, 0x40);
        key_on_voice(&mut dsp, &mut aram, 0);

        for _ in 0..100 {
            dsp.sample(&mut aram);
        }
        assert_ne!(dsp.sample(&mut aram), (0, 0));

        dsp.set_address(0x6C);
        dsp.write_data(&mut aram, 0x40);
        assert_eq!(dsp.sample(&mut aram), (0, 0));
    }

    #[test]
    fn endx_write_acknowledges_the_flags() {
        let mut dsp = Dsp::new();
        let mut aram = aram_with_sample(0x0300, 0x01);
        key_on_voice(&mut dsp, &mut aram, 0);
        for _ in 0..32 {
            dsp.sample(&mut aram);
        }
        dsp.set_address(0x7C);
        assert_eq!(dsp.read_data(&aram), 0x01);

        dsp.set_address(0x7C);
        dsp.write_data(&mut aram, 0x01);
        assert_eq!(dsp.read_data(&aram), 0x00);
    }
}
