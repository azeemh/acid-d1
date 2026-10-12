use crate::wav::RiffFile;
use std::fmt;

#[derive(Debug)]
pub enum AcidError {
    ChunkMissing,
    TooShort(usize),
}

impl fmt::Display for AcidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AcidError::ChunkMissing => write!(f, "no 'acid' chunk found in file"),
            AcidError::TooShort(n) => write!(f, "acid chunk is only {} bytes, need at least 24", n),
        }
    }
}

impl std::error::Error for AcidError {}

/// Metadata stored in the WAV "acid" chunk.
///
/// Layout (little-endian, 24 bytes minimum):
///   0..4   u32   flags (bit 0: 1 = loop, 0 = one-shot)
///   4..8   f32   root_note (MIDI note number, e.g. 60.0 for middle C)
///   8..12  u32   reserved (0)
///   12..16 f32   beats
///   16..18 u16   meter denominator
///   18..20 u16   meter numerator
///   20..24 f32   tempo (BPM)
#[derive(Debug, Clone, Copy)]
pub struct AcidInfo {
    pub flags: u32,
    pub is_loop: bool,
    pub root_note: f32,
    pub beats: f32,
    pub meter_den: u16,
    pub meter_num: u16,
    pub tempo: f32,
}

impl AcidInfo {
    pub fn parse(data: &[u8]) -> Result<Self, AcidError> {
        if data.len() < 24 {
            return Err(AcidError::TooShort(data.len()));
        }
        let flags = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let root_note = f32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let beats = f32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let meter_den = u16::from_le_bytes([data[16], data[17]]);
        let meter_num = u16::from_le_bytes([data[18], data[19]]);
        let tempo = f32::from_le_bytes([data[20], data[21], data[22], data[23]]);
        Ok(AcidInfo {
            flags,
            is_loop: (flags & 1) == 1,
            root_note,
            beats,
            meter_den,
            meter_num,
            tempo,
        })
    }

    pub fn from_riff(riff: &RiffFile) -> Result<Self, AcidError> {
        let chunk = riff.find(b"acid").ok_or(AcidError::ChunkMissing)?;
        Self::parse(&chunk.data)
    }

    /// Convert the stored root note to the nearest MIDI note number.
    pub fn root_midi(&self) -> i32 {
        self.root_note.round() as i32
    }

    /// Convert the stored root note to an approximate frequency in Hz.
    /// Uses A4 = 440 Hz at MIDI note 69.
    pub fn root_hz(&self) -> f32 {
        440.0 * 2f32.powf((self.root_note - 69.0) / 12.0)
    }

    pub fn note_name(&self) -> String {
        let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        let n = self.root_midi();
        let octave = n / 12 - 1;
        let name = names[(n.rem_euclid(12)) as usize];
        format!("{}{}", name, octave)
    }
}

/// Build the 24-byte body of an ACID chunk from the given metadata.
/// Used for generating synthetic test files.
pub fn build_acid_chunk_body(info: &AcidInfo) -> [u8; 24] {
    let mut out = [0u8; 24];
    out[0..4].copy_from_slice(&info.flags.to_le_bytes());
    out[4..8].copy_from_slice(&info.root_note.to_le_bytes());
    // 8..12 reserved (zero)
    out[12..16].copy_from_slice(&info.beats.to_le_bytes());
    out[16..18].copy_from_slice(&info.meter_den.to_le_bytes());
    out[18..20].copy_from_slice(&info.meter_num.to_le_bytes());
    out[20..24].copy_from_slice(&info.tempo.to_le_bytes());
    out
}
