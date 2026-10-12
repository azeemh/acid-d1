//! Simple additive mixer for interleaved stereo f32 audio.

pub struct Mixer {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Mixer {
    pub fn new(sample_rate: u32, initial_frames: usize) -> Self {
        Self {
            samples: vec![0.0; initial_frames * 2],
            sample_rate,
        }
    }

    pub fn len_frames(&self) -> usize {
        self.samples.len() / 2
    }

    /// Grow the internal buffer if needed, then add `input` at the given frame offset.
    pub fn add_at(&mut self, input: &[f32], offset_frames: usize, gain: f32) {
        let input_frames = input.len() / 2;
        let required = (offset_frames + input_frames) * 2;
        if required > self.samples.len() {
            self.samples.resize(required, 0.0);
        }
        let start = offset_frames * 2;
        for i in 0..input_frames * 2 {
            self.samples[start + i] += input[i] * gain;
        }
    }

    /// Find the peak absolute sample value.
    pub fn peak(&self) -> f32 {
        self.samples
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, f32::max)
    }

    /// Scale the buffer so the peak equals `target_peak`.
    pub fn normalize_to(&mut self, target_peak: f32) {
        let p = self.peak();
        if p > 1e-9 {
            let scale = target_peak / p;
            for s in &mut self.samples {
                *s *= scale;
            }
        }
    }

    /// Hard-clip everything to [-1, 1]. Fallback if not normalizing.
    pub fn clip(&mut self) {
        for s in &mut self.samples {
            *s = s.clamp(-1.0, 1.0);
        }
    }
}
