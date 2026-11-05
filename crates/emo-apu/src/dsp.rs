//! The S-DSP: eight BRR voices, ADSR/gain envelopes, noise and stereo mixing.

/// Number of voices.
const VOICES: usize = 8;

/// Envelope maximum (11-bit level).
const ENV_MAX: i32 = 0x7FF;

/// Samples between envelope/noise steps for each 5-bit rate value, at the
/// 32 kHz DSP sample clock. Rate 0 stops the counter.
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
    position: u32,
    buffer: [i16; 16],
    index: usize,
    brr_address: u16,
    loop_address: u16,
    previous: i16,
    older: i16,
    envelope: i32,
    envelope_counter: u32,
    state: EnvelopeState,
    output: i16,
}

impl Voice {
    const fn new() -> Self {
        Self {
            pitch: 0,
            position: 0,
            buffer: [0; 16],
            index: 16,
            brr_address: 0,
            loop_address: 0,
            previous: 0,
            older: 0,
            envelope: 0,
            envelope_counter: 0,
            state: EnvelopeState::Off,
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
    noise_lfsr: u16,
    noise_clock: u32,
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
            noise_lfsr: 1,
            noise_clock: 0,
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

        // KON and the end-flag ack have write side effects; the other writes
        // just update the register file.
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
                    // Soft reset keys off every voice and clears the levels.
                    for voice in &mut self.voices {
                        voice.state = EnvelopeState::Off;
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

            let state = &mut self.voices[voice];
            state.envelope = 0;
            state.envelope_counter = 0;
            state.state = EnvelopeState::Attack;
            state.position = 0;
            state.index = 16;
            state.previous = 0;
            state.older = 0;
            state.output = 0;
            state.loop_address = 0;
            let source = usize::from(self.registers[voice * 0x10 + 4]);
            let entry = directory + source * 4;
            let start = if entry + 1 < aram.len() {
                u16::from(aram[entry]) | (u16::from(aram[entry + 1]) << 8)
            } else {
                0
            };
            state.brr_address = start;
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

    /// Decode the next BRR block of a voice and advance its address.
    fn fetch_block(&mut self, voice: usize, aram: &[u8]) {
        let state = &mut self.voices[voice];
        let source = usize::from(state.brr_address);
        if source + 9 > aram.len() {
            state.state = EnvelopeState::Off;
            state.envelope = 0;
            self.endx |= 1 << voice;
            return;
        }

        let block = &aram[source..source + 9];
        let flags = block[0];
        let looping = flags & 0x02 != 0;
        let end = flags & 0x01 != 0;

        if looping {
            state.loop_address = state.brr_address;
        }

        let mut decoded = [0i16; 16];
        decode_brr(block, &mut decoded, &mut state.previous, &mut state.older);
        state.buffer = decoded;
        state.index = 0;
        state.brr_address = state.brr_address.wrapping_add(9);

        if end {
            if looping {
                state.brr_address = state.loop_address;
            } else {
                // A one-shot sample's end block mutes the voice.
                state.state = EnvelopeState::Off;
                state.envelope = 0;
                state.output = 0;
                self.endx |= 1 << voice;
            }
        }
    }

    /// Generate one stereo sample at 32 kHz.
    pub fn sample(&mut self, aram: &[u8]) -> (i16, i16) {
        self.clock_noise();

        let mut left = 0i32;
        let mut right = 0i32;

        for voice in 0..VOICES {
            if self.voices[voice].state == EnvelopeState::Off {
                self.voices[voice].output = 0;
                continue;
            }

            if self.voices[voice].index >= 16 {
                self.fetch_block(voice, aram);
                if self.voices[voice].state == EnvelopeState::Off {
                    continue;
                }
            }

            let pitch = u32::from(self.voices[voice].pitch & 0x3FFF);
            let advanced = self.voices[voice].position + pitch;
            self.voices[voice].position = advanced & 0x0FFF;
            let index = self.voices[voice].index + (advanced >> 12) as usize;
            if index >= 16 {
                self.fetch_block(voice, aram);
                if self.voices[voice].state == EnvelopeState::Off {
                    continue;
                }
                self.voices[voice].index = (index - 16).min(15);
            } else {
                self.voices[voice].index = index;
            }

            self.step_envelope(voice);
            if self.voices[voice].state == EnvelopeState::Off {
                continue;
            }

            let current = if self.registers[0x3D] & (1 << voice) != 0 {
                // Noise mode: the LFSR replaces the BRR sample value.
                if self.noise_lfsr & 1 == 0 {
                    0x7FFF
                } else {
                    -0x8000
                }
            } else {
                i32::from(self.voices[voice].buffer[self.voices[voice].index.min(15)])
            };

            let envelope = self.voices[voice].envelope;
            let output = ((current * envelope) >> 11) as i16;
            self.voices[voice].output = output;

            let volume_left = i32::from(self.registers[voice * 0x10] as i8);
            let volume_right = i32::from(self.registers[voice * 0x10 + 1] as i8);
            left += i32::from(output) * volume_left / 128;
            right += i32::from(output) * volume_right / 128;
        }

        if self.registers[0x6C] & 0x40 != 0 {
            // FLG bit 6 mutes the amplifier output.
            return (0, 0);
        }

        let master_left = i32::from(self.registers[0x0C] as i8);
        let master_right = i32::from(self.registers[0x1C] as i8);
        left = left * master_left / 128;
        right = right * master_right / 128;

        (
            left.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
            right.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        )
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
            // 15-bit LFSR with taps at bits 0 and 1.
            let feedback = (self.noise_lfsr ^ (self.noise_lfsr >> 1)) & 1;
            self.noise_lfsr = (self.noise_lfsr >> 1) | (feedback << 14);
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

        let period = if adsr1 & 0x80 != 0 {
            // ADSR mode.
            match state.state {
                EnvelopeState::Attack => RATE_PERIODS[usize::from((adsr1 & 0x0F) * 2 + 1)],
                EnvelopeState::Decay => RATE_PERIODS[usize::from(((adsr1 >> 4) & 0x07) * 2 + 16)],
                EnvelopeState::Sustain => RATE_PERIODS[usize::from(adsr2 & 0x1F)],
                _ => return,
            }
        } else if gain & 0x80 == 0 {
            // Direct gain: a fixed level, no stepping.
            state.envelope = i32::from(gain & 0x7F) * 16;
            return;
        } else {
            RATE_PERIODS[usize::from(gain & 0x1F)]
        };

        state.envelope_counter += 1;
        if state.envelope_counter < period {
            return;
        }
        state.envelope_counter = 0;

        if adsr1 & 0x80 != 0 {
            match state.state {
                EnvelopeState::Attack => {
                    let rate = (adsr1 & 0x0F) * 2 + 1;
                    let step = if rate == 31 { 1024 } else { 32 };
                    state.envelope += step;
                    if state.envelope >= 0x7E0 {
                        state.envelope = state.envelope.min(ENV_MAX);
                        state.state = EnvelopeState::Decay;
                    }
                }
                EnvelopeState::Decay | EnvelopeState::Sustain => {
                    let step = -(((state.envelope - 1) >> 8) + 1);
                    state.envelope = (state.envelope + step).max(0);
                    if state.state == EnvelopeState::Decay {
                        let boundary = (i32::from(adsr2 >> 5) + 1) << 8;
                        if state.envelope <= boundary {
                            state.state = EnvelopeState::Sustain;
                        }
                    }
                }
                _ => {}
            }
        } else {
            // Custom gain mode.
            let step = match (gain >> 5) & 0x03 {
                0 => -32,
                1 => -(((state.envelope - 1) >> 8) + 1),
                2 => 32,
                _ => {
                    if state.envelope < 0x600 {
                        32
                    } else {
                        8
                    }
                }
            };
            state.envelope = (state.envelope + step).clamp(0, ENV_MAX);
            if state.envelope == 0 {
                state.state = EnvelopeState::Off;
                state.output = 0;
                self.endx |= 1 << voice;
            }
        }
    }
}

fn decode_brr(block: &[u8], output: &mut [i16; 16], previous: &mut i16, older: &mut i16) {
    let header = block[0];
    let range = i32::from((header >> 4) & 0x0F);
    let filter = (header >> 2) & 0x03;

    for (index, byte) in block[1..9].iter().enumerate() {
        for (half, nibble) in [(0usize, (byte >> 4) & 0x0F), (1usize, byte & 0x0F)] {
            let signed = if nibble & 0x08 != 0 {
                i32::from(nibble) - 16
            } else {
                i32::from(nibble)
            };
            let sample = (signed << range) >> 1;

            let filtered = match filter {
                0 => sample,
                1 => sample + (i32::from(*previous) * 15 / 16),
                2 => sample + (i32::from(*previous) * 61 / 32) - (i32::from(*older) * 15 / 16),
                _ => sample + (i32::from(*previous) * 115 / 64) - (i32::from(*older) * 13 / 16),
            }
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;

            let slot = index * 2 + half;
            output[slot] = filtered;
            *older = *previous;
            *previous = filtered;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aram_with_sample(start: u16, header: u8) -> Vec<u8> {
        let mut aram = vec![0u8; 0x10000];
        // Sample directory at $0200, source 0 -> start address.
        aram[0x200] = start as u8;
        aram[0x201] = (start >> 8) as u8;
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
        dsp.set_address(0x4C);
        dsp.write_data(aram, 1 << voice);
    }

    #[test]
    fn brr_decodes_sixteen_samples() {
        let mut block = [0u8; 9];
        block[0] = 0x10;
        for byte in &mut block[1..] {
            *byte = 0x11;
        }

        let mut output = [0i16; 16];
        let mut previous = 0;
        let mut older = 0;
        decode_brr(&block, &mut output, &mut previous, &mut older);

        assert!(output.iter().all(|sample| *sample == 1));
    }

    #[test]
    fn key_on_starts_an_envelope() {
        let mut dsp = Dsp::new();
        let mut aram = aram_with_sample(0x0300, 0x00);
        dsp.set_address(0x05);
        dsp.write_data(&mut aram, 0x80 | 0x0F);
        key_on_voice(&mut dsp, &mut aram, 0);

        assert_eq!(dsp.voices[0].state, EnvelopeState::Attack);
        dsp.sample(&aram);
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
            dsp.sample(&aram);
        }
        assert_eq!(dsp.voices[0].state, EnvelopeState::Sustain);
        assert!(dsp.voices[0].envelope > 0);

        dsp.set_address(0x5C);
        dsp.write_data(&mut aram, 0x01);
        for _ in 0..1000 {
            dsp.sample(&aram);
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
            dsp.sample(&aram);
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
            dsp.sample(&aram);
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
            dsp.sample(&aram);
        }
        assert_ne!(dsp.sample(&aram), (0, 0));

        dsp.set_address(0x6C);
        dsp.write_data(&mut aram, 0x40);
        assert_eq!(dsp.sample(&aram), (0, 0));
    }

    #[test]
    fn endx_write_acknowledges_the_flags() {
        let mut dsp = Dsp::new();
        let mut aram = aram_with_sample(0x0300, 0x01);
        key_on_voice(&mut dsp, &mut aram, 0);
        for _ in 0..32 {
            dsp.sample(&aram);
        }
        dsp.set_address(0x7C);
        assert_eq!(dsp.read_data(&aram), 0x01);

        dsp.set_address(0x7C);
        dsp.write_data(&mut aram, 0x01);
        assert_eq!(dsp.read_data(&aram), 0x00);
    }
}
