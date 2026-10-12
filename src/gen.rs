//! Generate synthetic ACIDized WAV files for testing.

use crate::acid::{build_acid_chunk_body, AcidInfo};

pub struct WavSpec {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub num_samples: u32,
}

impl Default for WavSpec {
    fn default() -> Self {
        Self {
            channels: 2,
            sample_rate: 44100,
            bits_per_sample: 16,
            num_samples: 44100,
        }
    }
}

impl WavSpec {
    /// Number of samples required to hold `beats` at `tempo` BPM.
    pub fn samples_for_beats(beats: f32, tempo: f32, sample_rate: u32) -> u32 {
        let seconds = beats * 60.0 / tempo;
        (seconds * sample_rate as f32).round() as u32
    }
}

pub fn build_acid_wav(spec: &WavSpec, acid: &AcidInfo) -> Vec<u8> {
    let bytes_per_sample = (spec.bits_per_sample / 8) as u32;
    let block_align = spec.channels as u32 * bytes_per_sample;
    let byte_rate = spec.sample_rate * block_align;
    let data_size = spec.num_samples * block_align;

    let mut fmt = Vec::with_capacity(16);
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&spec.channels.to_le_bytes());
    fmt.extend_from_slice(&spec.sample_rate.to_le_bytes());
    fmt.extend_from_slice(&byte_rate.to_le_bytes());
    fmt.extend_from_slice(&(block_align as u16).to_le_bytes());
    fmt.extend_from_slice(&spec.bits_per_sample.to_le_bytes());

    let mut data = Vec::with_capacity(data_size as usize);
    let two_pi = std::f32::consts::TAU;
    for i in 0..spec.num_samples {
        let t = i as f32 / spec.sample_rate as f32;
        let sample = (two_pi * 440.0 * t).sin() * 0.25;
        let s16 = (sample * i16::MAX as f32) as i16;
        for _ in 0..spec.channels {
            data.extend_from_slice(&s16.to_le_bytes());
        }
    }

    let acid_body = build_acid_chunk_body(acid);

    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    push_chunk(&mut body, b"fmt ", &fmt);
    push_chunk(&mut body, b"acid", &acid_body);
    push_chunk(&mut body, b"data", &data);

    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Build a clicky loop: continuous sine at `sine_hz` with a sharp attack at the
/// start of every beat.
pub fn build_click_loop(
    beats: f32,
    tempo: f32,
    sine_hz: f32,
    sample_rate: u32,
    channels: u16,
) -> Vec<u8> {
    let num_samples = WavSpec::samples_for_beats(beats, tempo, sample_rate);
    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        num_samples,
    };
    let acid = AcidInfo {
        flags: 1,
        is_loop: true,
        root_note: 60.0,
        beats,
        meter_den: 4,
        meter_num: 4,
        tempo,
    };

    let bytes_per_sample = (spec.bits_per_sample / 8) as u32;
    let block_align = spec.channels as u32 * bytes_per_sample;
    let byte_rate = spec.sample_rate * block_align;
    let data_size = spec.num_samples * block_align;

    let mut fmt = Vec::with_capacity(16);
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&spec.channels.to_le_bytes());
    fmt.extend_from_slice(&spec.sample_rate.to_le_bytes());
    fmt.extend_from_slice(&byte_rate.to_le_bytes());
    fmt.extend_from_slice(&(block_align as u16).to_le_bytes());
    fmt.extend_from_slice(&spec.bits_per_sample.to_le_bytes());

    let samples_per_beat = sample_rate as f32 * 60.0 / tempo;

    let mut data = Vec::with_capacity(data_size as usize);
    let two_pi = std::f32::consts::TAU;
    for i in 0..spec.num_samples {
        let t = i as f32 / sample_rate as f32;
        let phase_in_beat = (i as f32) % samples_per_beat;
        let t_in_beat = phase_in_beat / sample_rate as f32;

        // Envelope: full for 5ms after the beat, then exponential decay.
        let env = if t_in_beat < 0.005 {
            1.0
        } else {
            (-(t_in_beat - 0.005) / 0.08).exp()
        };
        let amp = 0.15 + 0.55 * env;

        let sample = (two_pi * sine_hz * t).sin() * amp;
        let s16 = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        for _ in 0..spec.channels {
            data.extend_from_slice(&s16.to_le_bytes());
        }
    }

    let acid_body = build_acid_chunk_body(&acid);

    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    push_chunk(&mut body, b"fmt ", &fmt);
    push_chunk(&mut body, b"acid", &acid_body);
    push_chunk(&mut body, b"data", &data);

    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}
