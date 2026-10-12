//! Peak envelope cache for waveform display.

pub struct WaveformCache {
    pub mins: Vec<f32>,
    pub maxes: Vec<f32>,
    pub samples_per_bucket: usize,
}

impl WaveformCache {
    pub fn build(samples: &[f32], samples_per_bucket: usize) -> Self {
        let spb = samples_per_bucket.max(1);
        let frames = samples.len() / 2;
        if frames == 0 {
            return Self {
                mins: Vec::new(),
                maxes: Vec::new(),
                samples_per_bucket: spb,
            };
        }
        let n_buckets = (frames + spb - 1) / spb;
        let mut mins = Vec::with_capacity(n_buckets);
        let mut maxes = Vec::with_capacity(n_buckets);
        for b in 0..n_buckets {
            let start = b * spb;
            let end = ((b + 1) * spb).min(frames);
            let mut mn = f32::MAX;
            let mut mx = f32::MIN;
            for f in start..end {
                let v = (samples[f * 2] + samples[f * 2 + 1]) * 0.5;
                if v < mn {
                    mn = v;
                }
                if v > mx {
                    mx = v;
                }
            }
            if mn > mx {
                mn = 0.0;
                mx = 0.0;
            }
            mins.push(mn);
            maxes.push(mx);
        }
        Self {
            mins,
            maxes,
            samples_per_bucket: spb,
        }
    }

    pub fn num_buckets(&self) -> usize {
        self.mins.len()
    }
}
