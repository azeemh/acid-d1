//! Signalsmith Stretch wrapper.
//!
//! Provides a one-shot offline stretcher that matches the phase vocoder's
//! input/output contract (interleaved stereo f32 in, interleaved stereo f32
//! out), plus pitch, formant, and transient controls.

use signalsmith_stretch::Stretch as SsStretch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransientMode {
    /// Aggressive smoothing, like a phase vocoder. Kills transients.
    Smooth,
    /// Default. Good for loop material.
    Balanced,
    /// Maximum transient preservation. Sharp attacks, occasional artifacts.
    Sharp,
}

impl TransientMode {
    pub fn label(self) -> &'static str {
        match self {
            TransientMode::Smooth => "smooth",
            TransientMode::Balanced => "balanced",
            TransientMode::Sharp => "sharp",
        }
    }
    /// Tonality limit in Hz. Higher = more high-frequency detail preserved
    /// through transients. The value is the frequency above which the
    /// frequency map becomes non-linear.
    fn tonality_hz(self) -> f32 {
        match self {
            TransientMode::Smooth => 2000.0,
            TransientMode::Balanced => 8000.0,
            TransientMode::Sharp => 16000.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StretchOptions {
    /// Pitch shift in semitones on top of the time-stretch. 0 = preserve.
    pub pitch_semitones: f32,
    /// Formant shift in semitones. 0 = no shift. Only useful if you are
    /// pitch-shifting and want to alter the vocal-tract character.
    pub formant_semitones: f32,
    /// If true, formants stay at their original absolute frequency through
    /// a pitch shift. This is what vocalists usually want. If false, formants
    /// travel with the pitch (like speeding up a tape).
    pub preserve_formants: bool,
    pub transient: TransientMode,
}

impl Default for StretchOptions {
    fn default() -> Self {
        Self {
            pitch_semitones: 0.0,
            formant_semitones: 0.0,
            preserve_formants: false,
            transient: TransientMode::Balanced,
        }
    }
}

pub struct SignalsmithEngine {
    stretch: SsStretch,
    channels: usize,
    sample_rate: u32,
}

impl SignalsmithEngine {
    pub fn new(channels: u32, sample_rate: u32) -> Self {
        Self {
            stretch: SsStretch::preset_default(channels, sample_rate),
            channels: channels as usize,
            sample_rate,
        }
    }

    fn apply_options(&mut self, opts: &StretchOptions) {
        let tonality = opts.transient.tonality_hz() / self.sample_rate as f32;
        self.stretch
            .set_transpose_factor_semitones(opts.pitch_semitones, Some(tonality));

        let compensate = opts.preserve_formants && opts.pitch_semitones.abs() > 1e-3;
        if opts.formant_semitones.abs() > 1e-3 || compensate {
            self.stretch
                .set_formant_factor_semitones(opts.formant_semitones, compensate);
        }
    }

    /// One-shot stretch of an interleaved stereo buffer.
    ///
    /// `ratio` = output_length / input_length. ratio > 1 makes it longer.
    pub fn stretch(
        &mut self,
        input: &[f32],
        ratio: f64,
        opts: &StretchOptions,
    ) -> Vec<f32> {
        self.stretch.reset();
        self.apply_options(opts);

        let in_frames = input.len() / self.channels;
        if in_frames == 0 || ratio <= 0.0 {
            return Vec::new();
        }
        let out_frames = ((in_frames as f64) * ratio).round() as usize;

        // Allocate with a small cushion for output latency.
        let latency = self.stretch.output_latency();
        let alloc_frames = out_frames + latency;
        let mut output = vec![0.0f32; alloc_frames * self.channels];

        self.stretch.exact(input, &mut output);

        output.truncate(out_frames * self.channels);
        output
    }

    /// Streaming-friendly variant that processes in blocks. Same result as
    /// `stretch`, just broken up so a future real-time path can reuse it.
    pub fn stretch_blocked(
        &mut self,
        input: &[f32],
        ratio: f64,
        opts: &StretchOptions,
    ) -> Vec<f32> {
        self.stretch.reset();
        self.apply_options(opts);

        let in_frames = input.len() / self.channels;
        if in_frames == 0 || ratio <= 0.0 {
            return Vec::new();
        }
        let out_frames = ((in_frames as f64) * ratio).round() as usize;

        const BLOCK_IN: usize = 8192;
        let mut output = Vec::with_capacity(out_frames * self.channels);
        let mut pos = 0usize;
        while pos < in_frames {
            let n_in = (in_frames - pos).min(BLOCK_IN);
            let n_out = ((n_in as f64) * ratio).round().max(1.0) as usize;
            let s = pos * self.channels;
            let e = (pos + n_in) * self.channels;
            let mut buf = vec![0.0f32; n_out * self.channels];
            self.stretch.process(&input[s..e], &mut buf);
            output.extend_from_slice(&buf);
            pos += n_in;
        }

        let lat = self.stretch.output_latency();
        if lat > 0 {
            let mut tail = vec![0.0f32; lat * self.channels];
            self.stretch.flush(&mut tail);
            output.extend_from_slice(&tail);
        }

        output.truncate(out_frames * self.channels);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sine(frames: usize, rate: u32, freq: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; frames * 2];
        let two_pi = std::f32::consts::TAU;
        for i in 0..frames {
            let s = (two_pi * freq * i as f32 / rate as f32).sin() * 0.25;
            v[i * 2] = s;
            v[i * 2 + 1] = s;
        }
        v
    }

    #[test]
    fn length_ratio_is_correct() {
        let rate = 44100;
        let input = make_sine(rate as usize, rate, 440.0);
        let mut eng = SignalsmithEngine::new(2, rate);
        let opts = StretchOptions::default();
        for ratio in [0.5f64, 0.75, 1.5, 2.0] {
            let out = eng.stretch(&input, ratio, &opts);
            let expected = (rate as f64 * ratio).round() as usize;
            let got = out.len() / 2;
            assert!(
                (got as i64 - expected as i64).abs() <= 2,
                "ratio={}: expected {} frames, got {}",
                ratio,
                expected,
                got
            );
        }
    }

    #[test]
    fn pitch_preserved_by_default() {
        let rate = 44100;
        let input = make_sine(rate as usize, rate, 440.0);
        let mut eng = SignalsmithEngine::new(2, rate);
        let opts = StretchOptions::default();
        let out = eng.stretch(&input, 1.5, &opts);

        // Measure dominant frequency via zero crossings on channel 0.
        let frames = out.len() / 2;
        let mut zc = 0usize;
        for i in 1..frames {
            let a = out[(i - 1) * 2];
            let b = out[i * 2];
            if (a < 0.0) != (b < 0.0) {
                zc += 1;
            }
        }
        let dur = frames as f32 / rate as f32;
        let f = (zc as f32 / 2.0) / dur;
        assert!(
            (f - 440.0).abs() < 25.0,
            "expected ~440 Hz, got {:.1} Hz",
            f
        );
    }

    #[test]
    fn pitch_shift_semitones_changes_pitch() {
        let rate = 44100;
        let input = make_sine(rate as usize, rate, 440.0);
        let mut eng = SignalsmithEngine::new(2, rate);
        let mut opts = StretchOptions::default();
        opts.pitch_semitones = 12.0; // one octave up = 880 Hz
        let out = eng.stretch(&input, 1.0, &opts);

        let frames = out.len() / 2;
        let mut zc = 0usize;
        for i in 1..frames {
            let a = out[(i - 1) * 2];
            let b = out[i * 2];
            if (a < 0.0) != (b < 0.0) {
                zc += 1;
            }
        }
        let dur = frames as f32 / rate as f32;
        let f = (zc as f32 / 2.0) / dur;
        assert!(
            (f - 880.0).abs() < 60.0,
            "expected ~880 Hz, got {:.1} Hz",
            f
        );
    }

    #[test]
    fn blocked_matches_oneshot_length() {
        let rate = 44100;
        let input = make_sine(rate as usize, rate, 440.0);
        let mut eng = SignalsmithEngine::new(2, rate);
        let opts = StretchOptions::default();
        let a = eng.stretch(&input, 1.3, &opts);
        let b = eng.stretch_blocked(&input, 1.3, &opts);
        assert_eq!(a.len(), b.len());
    }
}
