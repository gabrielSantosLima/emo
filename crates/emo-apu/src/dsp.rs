//! The S-DSP: eight BRR voices, envelopes and stereo mixing.

/// Number of voices.
const VOICES: usize = 8;

/// Envelope maximum.
const ENV_MAX: i32 = 0x7FF;

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

    /// Master volume, left channel.
    #[must_use]
    pub fn volume_left(&self) -> u8 {
        self.registers[0x0C]
    }

    /// Read the selected register.
    pub fn read_data(&self, _aram: &[u8]) -> u8 {
        let address = usize::from(self.address);

        match address {
            0x00..=0x7F if (address & 0x0F) == 0x08 => {
                let voice = address / 0x10;
                (self.voices[voice].envelope >> 4) as u8
            }
            0x00..=0x7F if (address & 0x0F) == 0x09 => {
                let voice = address / 0x10;
                self.voices[voice].output as u8
            }
            _ => self.registers[address],
        }
    }

    /// Write the selected register.
    pub fn write_data(&mut self, aram: &mut [u8], value: u8) {
        let address = usize::from(self.address);
        self.registers[address] = value;

        match address {
            0x4C => self.key_on(value, aram),
            0x5C => self.key_off(value),
            0x00..=0x7F if (address & 0x0F) <= 0x07 => {
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
            _ => {}
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
            state.state = EnvelopeState::Attack;
            state.position = 0;
            state.index = 16;
            state.previous = 0;
            state.older = 0;
            let source = usize::from(self.registers[voice * 0x10 + 4]);
            let entry = directory + source * 4;
            let start = if entry + 1 < aram.len() {
                u16::from(aram[entry]) | (u16::from(aram[entry + 1]) << 8)
            } else {
                0
            };
            state.brr_address = start;
            state.loop_address = 0;
        }

        self.registers[0x7C] = 0;
    }

    fn key_off(&mut self, mask: u8) {
        for voice in 0..VOICES {
            if mask & (1 << voice) != 0 && self.voices[voice].state != EnvelopeState::Off {
                self.voices[voice].state = EnvelopeState::Release;
            }
        }
    }

    /// Generate one stereo sample at 32 kHz.
    pub fn sample(&mut self, aram: &[u8]) -> (i16, i16) {
        let mut left = 0i32;
        let mut right = 0i32;

        for voice in 0..VOICES {
            let state = &mut self.voices[voice];
            if state.state == EnvelopeState::Off {
                state.output = 0;
                continue;
            }

            let source = usize::from(state.brr_address);
            if state.index >= 16 && source + 8 < aram.len() {
                let block = &aram[source..source + 9];
                let looping = block[0] & 0x02 != 0;
                let end = block[0] & 0x01 != 0;

                if looping && state.loop_address == 0 {
                    state.loop_address = state.brr_address;
                }

                let mut decoded = [0i16; 16];
                decode_brr(block, &mut decoded, &mut state.previous, &mut state.older);
                state.buffer = decoded;
                state.index = 0;
                state.brr_address = state.brr_address.wrapping_add(9);

                if end && !looping {
                    state.brr_address = state.loop_address;
                }
            }

            state.position += u32::from(state.pitch).min(0x3FFF);
            while state.position >= 0x1000 && state.index < 16 {
                state.position -= 0x1000;
                state.index += 1;
            }

            let current = state.buffer[state.index.min(15)];
            step_envelope(state);

            let envelope = state.envelope;
            let voice_index = voice;
            state.output = ((i32::from(current) * envelope) >> 11) as i16;

            let volume_left = i32::from(self.registers[voice_index * 0x10] as i8);
            let volume_right = i32::from(self.registers[voice_index * 0x10 + 1] as i8);
            left += i32::from(state.output) * volume_left / 128;
            right += i32::from(state.output) * volume_right / 128;
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
}

fn step_envelope(voice: &mut Voice) {
    match voice.state {
        EnvelopeState::Off => {}
        EnvelopeState::Attack => {
            voice.envelope += 8;
            if voice.envelope >= ENV_MAX {
                voice.envelope = ENV_MAX;
                voice.state = EnvelopeState::Decay;
            }
        }
        EnvelopeState::Decay => {
            voice.envelope -= 4;
            if voice.envelope <= 0x400 {
                voice.envelope = 0x400;
                voice.state = EnvelopeState::Sustain;
            }
        }
        EnvelopeState::Sustain => {}
        EnvelopeState::Release => {
            voice.envelope -= 8;
            if voice.envelope <= 0 {
                voice.envelope = 0;
                voice.state = EnvelopeState::Off;
                voice.output = 0;
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
        dsp.set_address(0x4C);
        let mut aram = vec![0u8; 0x10000];
        dsp.write_data(&mut aram, 0x01);

        assert_eq!(dsp.voices[0].state, EnvelopeState::Attack);
        dsp.sample(&aram);
        assert!(dsp.voices[0].envelope > 0);
    }
}
