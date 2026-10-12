use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug)]
pub enum WavError {
    TooShort,
    NotRiff,
    NotWave,
}
impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WavError::TooShort => write!(f, "file too short"),
            WavError::NotRiff => write!(f, "missing RIFF"),
            WavError::NotWave => write!(f, "not a WAVE file"),
        }
    }
}
impl std::error::Error for WavError {}

#[derive(Clone)]
pub struct RiffChunk {
    pub id: [u8; 4],
    pub data: Vec<u8>,
}
impl RiffChunk {
    pub fn id_str(&self) -> String {
        String::from_utf8_lossy(&self.id).to_string()
    }
}

pub struct RiffFile {
    pub chunks: Vec<RiffChunk>,
}
impl RiffFile {
    pub fn parse(data: &[u8]) -> Result<Self, WavError> {
        if data.len() < 12 {
            return Err(WavError::TooShort);
        }
        if &data[0..4] != b"RIFF" {
            return Err(WavError::NotRiff);
        }
        if &data[8..12] != b"WAVE" {
            return Err(WavError::NotWave);
        }
        let mut chunks = Vec::new();
        let mut pos = 12usize;
        while pos + 8 <= data.len() {
            let mut id = [0u8; 4];
            id.copy_from_slice(&data[pos..pos + 4]);
            let size = u32::from_le_bytes([
                data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7],
            ]) as usize;
            pos += 8;
            if pos + size > data.len() {
                break;
            }
            let chunk_data = data[pos..pos + size].to_vec();
            chunks.push(RiffChunk { id, data: chunk_data });
            pos += size;
            if size % 2 == 1 {
                pos += 1;
            }
        }
        Ok(RiffFile { chunks })
    }
    pub fn find(&self, id: &[u8; 4]) -> Option<&RiffChunk> {
        self.chunks.iter().find(|c| &c.id == id)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PcmFormat {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
}
impl PcmFormat {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 16 {
            return None;
        }
        let channels = u16::from_le_bytes([data[2], data[3]]);
        let sample_rate = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let bits_per_sample = u16::from_le_bytes([data[14], data[15]]);
        Some(PcmFormat { channels, sample_rate, bits_per_sample })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitDepth {
    Int16,
    Int24,
    Float32,
}
impl BitDepth {
    pub fn bits(self) -> u16 {
        match self {
            BitDepth::Int16 => 16,
            BitDepth::Int24 => 24,
            BitDepth::Float32 => 32,
        }
    }
    pub fn bytes_per_sample(self) -> u32 {
        match self {
            BitDepth::Int16 => 2,
            BitDepth::Int24 => 3,
            BitDepth::Float32 => 4,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            BitDepth::Int16 => "16-bit int",
            BitDepth::Int24 => "24-bit int",
            BitDepth::Float32 => "32-bit float",
        }
    }
}

pub struct AcidWavSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub tempo: f32,
    pub beats: f32,
    pub meter_num: u16,
    pub meter_den: u16,
    pub root_note: f32,
    pub bit_depth: BitDepth,
}

pub fn write_wav_to_vec(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let spec = AcidWavSpec {
        sample_rate,
        channels,
        tempo: 120.0,
        beats: 0.0,
        meter_num: 4,
        meter_den: 4,
        root_note: 60.0,
        bit_depth: BitDepth::Int16,
    };
    // Write without the ACID chunk by zeroing it out. Simple approach:
    // build a version with an empty acid chunk instead. Actually we just
    // want plain 16-bit; reuse the container writer below.
    write_plain_wav(samples, spec.sample_rate, channels, BitDepth::Int16)
}

pub fn write_wav_to_file(
    path: &Path,
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
) -> io::Result<()> {
    fs::write(path, write_wav_to_vec(samples, sample_rate, channels))
}

fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

fn build_fmt(spec_rate: u32, channels: u16, depth: BitDepth) -> Vec<u8> {
    let bits = depth.bits();
    let bytes = depth.bytes_per_sample();
    let block_align = channels as u32 * bytes;
    let byte_rate = spec_rate * block_align;

    let mut fmt = Vec::with_capacity(40);
    if depth == BitDepth::Int16 {
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&spec_rate.to_le_bytes());
        fmt.extend_from_slice(&byte_rate.to_le_bytes());
        fmt.extend_from_slice(&(block_align as u16).to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
    } else {
        // WAVEFORMATEXTENSIBLE
        fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&spec_rate.to_le_bytes());
        fmt.extend_from_slice(&byte_rate.to_le_bytes());
        fmt.extend_from_slice(&(block_align as u16).to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&0x3u32.to_le_bytes());
        let guid: [u8; 16] = match depth {
            BitDepth::Float32 => [
                0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00,
                0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
            ],
            _ => [
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00,
                0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
            ],
        };
        fmt.extend_from_slice(&guid);
    }
    fmt
}

fn encode_samples(samples: &[f32], depth: BitDepth) -> Vec<u8> {
    let mut data = Vec::with_capacity(samples.len() * depth.bytes_per_sample() as usize);
    match depth {
        BitDepth::Int16 => {
            for &s in samples {
                let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
                data.extend_from_slice(&v.to_le_bytes());
            }
        }
        BitDepth::Int24 => {
            const SCALE: f32 = 8_388_607.0;
            for &s in samples {
                let v = (s.clamp(-1.0, 1.0) * SCALE).round() as i32;
                let b = v.to_le_bytes();
                data.extend_from_slice(&b[0..3]);
            }
        }
        BitDepth::Float32 => {
            for &s in samples {
                data.extend_from_slice(&s.to_le_bytes());
            }
        }
    }
    data
}

fn build_acid(spec: &AcidWavSpec) -> [u8; 24] {
    let mut acid = [0u8; 24];
    let flags: u32 = 1;
    acid[0..4].copy_from_slice(&flags.to_le_bytes());
    acid[4..8].copy_from_slice(&spec.root_note.to_le_bytes());
    acid[12..16].copy_from_slice(&spec.beats.to_le_bytes());
    acid[16..18].copy_from_slice(&spec.meter_den.to_le_bytes());
    acid[18..20].copy_from_slice(&spec.meter_num.to_le_bytes());
    acid[20..24].copy_from_slice(&spec.tempo.to_le_bytes());
    acid
}

pub fn write_plain_wav(samples: &[f32], sample_rate: u32, channels: u16, depth: BitDepth) -> Vec<u8> {
    let fmt = build_fmt(sample_rate, channels, depth);
    let data = encode_samples(samples, depth);
    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    push_chunk(&mut body, b"fmt ", &fmt);
    push_chunk(&mut body, b"data", &data);
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn write_acid_wav_to_vec(samples: &[f32], spec: &AcidWavSpec) -> Vec<u8> {
    let fmt = build_fmt(spec.sample_rate, spec.channels, spec.bit_depth);
    let acid = build_acid(spec);
    let data = encode_samples(samples, spec.bit_depth);
    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    push_chunk(&mut body, b"fmt ", &fmt);
    push_chunk(&mut body, b"acid", &acid);
    push_chunk(&mut body, b"data", &data);
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn write_acid_wav_to_file(
    path: &Path,
    samples: &[f32],
    spec: &AcidWavSpec,
) -> io::Result<()> {
    fs::write(path, write_acid_wav_to_vec(samples, spec))
}
