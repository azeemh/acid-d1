//! Beat and tempo detection, grid fitting, and metronome click synthesis.
//!
//! `analyze` runs STFT + spectral flux + autocorrelation and returns the raw
//! onset envelope plus a candidate tempo and first beat. The wizard UI uses
//! the envelope to snap the user's downbeat to the nearest onset.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

pub const ONSET_HOP: usize = 512;

#[derive(Clone, Debug)]
pub struct BeatAnalysis {
    /// Positive spectral flux per frame, mean-subtracted.
    pub onset_env: Vec<f32>,
    /// Frames per second of the onset envelope.
    pub onset_fps: f32,
    /// Total duration of the analyzed audio.
    pub total_secs: f32,
    /// Detected tempo in BPM.
    pub tempo: f32,
    /// Detected position of the first beat, in seconds.
    pub first_beat_secs: f32,
    /// Raw autocorrelation peak height; higher = more confident.
    pub confidence: f32,
}

#[derive(Clone, Debug)]
pub struct BeatMap {
    pub tempo: f32,
    pub beat_positions_secs: Vec<f32>,
    pub confidence: f32,
}

/// Analyze an audio buffer and return the onset envelope and detected tempo.
pub fn analyze(samples: &[f32], sample_rate: u32) -> Option<BeatAnalysis> {
    let frames = samples.len() / 2;
    if frames < (sample_rate as usize) / 2 {
        return None;
    }

    let mono: Vec<f32> = (0..frames)
        .map(|i| (samples[i * 2] + samples[i * 2 + 1]) * 0.5)
        .collect();

    const WIN: usize = 1024;
    let bins = WIN / 2 + 1;
    let hop = ONSET_HOP;

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WIN);
    let window: Vec<f32> = (0..WIN)
        .map(|i| {
            let x = 2.0 * std::f32::consts::PI * i as f32 / WIN as f32;
            0.5 * (1.0 - x.cos())
        })
        .collect();

    let num_frames = (mono.len().saturating_sub(WIN)) / hop + 1;
    if num_frames < 8 {
        return None;
    }

    let mut prev_mag = vec![0.0f32; bins];
    let mut flux = vec![0.0f32; num_frames];
    let mut buf = vec![Complex::<f32>::default(); WIN];

    for f in 0..num_frames {
        let start = f * hop;
        for i in 0..WIN {
            buf[i] = Complex::new(mono[start + i] * window[i], 0.0);
        }
        fft.process(&mut buf);
        let mut sum = 0.0f32;
        for k in 0..bins {
            let mag = buf[k].norm();
            let diff = mag - prev_mag[k];
            if diff > 0.0 {
                sum += diff;
            }
            prev_mag[k] = mag;
        }
        flux[f] = sum;
    }

    let mean = flux.iter().sum::<f32>() / flux.len() as f32;
    for v in flux.iter_mut() {
        *v = (*v - mean).max(0.0);
    }

    let fps = sample_rate as f32 / hop as f32;

    // Tempo via autocorrelation.
    let min_bpm = 60.0f32;
    let max_bpm = 200.0f32;
    let min_lag = (fps * 60.0 / max_bpm).round() as usize;
    let max_lag = (fps * 60.0 / min_bpm).round() as usize;
    let n = flux.len();

    let mut best_lag = 0usize;
    let mut best_score = 0.0f32;
    for lag in min_lag..=max_lag.min(n / 2) {
        let mut acc = 0.0f32;
        let count = n - lag;
        for i in 0..count {
            acc += flux[i] * flux[i + lag];
        }
        let score = acc / count as f32;
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }

    if best_lag == 0 {
        return None;
    }

    // Parabolic refine on the autocorrelation peak.
    let acov = |lag: i32| -> f32 {
        if lag < 1 || (lag as usize) + 1 >= n {
            return 0.0;
        }
        let l = lag as usize;
        let mut acc = 0.0f32;
        let count = n - l;
        for i in 0..count {
            acc += flux[i] * flux[i + l];
        }
        acc / count as f32
    };
    let y0 = acov(best_lag as i32 - 1);
    let y1 = acov(best_lag as i32);
    let y2 = acov(best_lag as i32 + 1);
    let denom = y0 - 2.0 * y1 + y2;
    let refined_lag = if denom.abs() > 1e-9 {
        best_lag as f32 + 0.5 * (y0 - y2) / denom
    } else {
        best_lag as f32
    };
    let tempo = (60.0 * fps / refined_lag).clamp(20.0, 400.0);

    // Phase alignment: try every integer offset within one period.
    let period = best_lag.max(1);
    let mut best_offset = 0usize;
    let mut best_phase = f32::NEG_INFINITY;
    for offset in 0..period {
        let mut score = 0.0f32;
        let mut count = 0;
        let mut i = offset;
        while i < n {
            score += flux[i];
            count += 1;
            i += period;
        }
        if count > 0 {
            score /= count as f32;
        }
        if score > best_phase {
            best_phase = score;
            best_offset = offset;
        }
    }
    let first_beat_secs = best_offset as f32 * hop as f32 / sample_rate as f32;

    Some(BeatAnalysis {
        onset_env: flux,
        onset_fps: fps,
        total_secs: frames as f32 / sample_rate as f32,
        tempo,
        first_beat_secs,
        confidence: best_score,
    })
}

/// Generate beat positions from a downbeat offset and tempo.
pub fn fit_beats(offset_secs: f32, tempo: f32, total_secs: f32) -> Vec<f32> {
    if tempo <= 0.0 || total_secs <= 0.0 {
        return Vec::new();
    }
    let period = 60.0 / tempo;
    let mut out = Vec::new();
    let mut t = offset_secs;
    while t < total_secs {
        out.push(t);
        t += period;
    }
    out
}

/// Snap a time value to the nearest strong onset within a search window.
pub fn snap_to_onset(analysis: &BeatAnalysis, time_secs: f32, window_secs: f32) -> f32 {
    if analysis.onset_env.is_empty() || analysis.onset_fps <= 0.0 {
        return time_secs;
    }
    let center = (time_secs * analysis.onset_fps) as i32;
    let half = (window_secs * analysis.onset_fps) as i32;
    let start = (center - half).max(0) as usize;
    let end = ((center + half) as usize).min(analysis.onset_env.len());
    if start >= end {
        return time_secs;
    }
    let mut best = start;
    let mut best_val = -1.0f32;
    for i in start..end {
        if analysis.onset_env[i] > best_val {
            best_val = analysis.onset_env[i];
            best = i;
        }
    }
    best as f32 / analysis.onset_fps
}

/// Wrapper that returns a BeatMap using detected values.
pub fn detect_beats(samples: &[f32], sample_rate: u32) -> Option<BeatMap> {
    let a = analyze(samples, sample_rate)?;
    let positions = fit_beats(a.first_beat_secs, a.tempo, a.total_secs);
    Some(BeatMap {
        tempo: a.tempo,
        beat_positions_secs: positions,
        confidence: a.confidence,
    })
}

/// Synthesize a click track. `period_secs` is the click spacing, `offset_secs`
/// is the downbeat position. Every `bar_beats`-th click is accented (higher
/// pitch, louder).
pub fn synth_clicks(
    sample_rate: u32,
    duration_secs: f32,
    period_secs: f32,
    offset_secs: f32,
    bar_beats: u16,
) -> Vec<f32> {
    let total_frames = (duration_secs * sample_rate as f32).round() as usize;
    let mut out = vec![0.0f32; total_frames * 2];
    if period_secs <= 0.0 || total_frames == 0 {
        return out;
    }
    let bar = bar_beats.max(1) as i32;
    let click_len = (sample_rate as f32 * 0.025) as usize;
    let two_pi = std::f32::consts::TAU;

    let mut beat_idx: i32 = 0;
    let mut t = offset_secs;
    while t < duration_secs {
        let start_frame = (t * sample_rate as f32).round() as i64;
        let is_down = beat_idx.rem_euclid(bar) == 0;
        let freq = if is_down { 1600.0 } else { 1000.0 };
        let amp = if is_down { 0.55 } else { 0.30 };
        for i in 0..click_len {
            let idx = start_frame + i as i64;
            if idx < 0 || idx as usize >= total_frames {
                break;
            }
            let t_in = i as f32 / sample_rate as f32;
            let env = (-t_in * 90.0).exp();
            let s = (two_pi * freq * t_in).sin() * env * amp;
            let fi = idx as usize;
            out[fi * 2] += s;
            out[fi * 2 + 1] += s;
        }
        t += period_secs;
        beat_idx += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthesize_click_track(bpm: f32, seconds: f32, rate: u32) -> Vec<f32> {
        let frames = (seconds * rate as f32) as usize;
        let mut out = vec![0.0f32; frames * 2];
        let period = 60.0 / bpm * rate as f32;
        let mut next = 0.0f32;
        let two_pi = std::f32::consts::TAU;
        for i in 0..frames {
            let phase = i as f32 - next;
            let t = if phase < 0.0 { 0.0 } else { phase / rate as f32 };
            let env = (-t * 60.0).exp();
            let s = (two_pi * 1000.0 * i as f32 / rate as f32).sin() * env * 0.6;
            out[i * 2] = s;
            out[i * 2 + 1] = s;
            if phase >= 0.0 && t > 0.02 {
                next += period;
            }
        }
        out
    }

    #[test]
    fn detects_simple_tempo() {
        let rate = 44100;
        let samples = synthesize_click_track(120.0, 6.0, rate);
        let a = analyze(&samples, rate).expect("detected");
        assert!(
            (a.tempo - 120.0).abs() < 3.0,
            "expected ~120 BPM, got {:.2}",
            a.tempo
        );
    }

    #[test]
    fn fit_beats_is_periodic() {
        let beats = fit_beats(0.1, 120.0, 4.0);
        assert!(beats.len() >= 4);
        for w in beats.windows(2) {
            let p = w[1] - w[0];
            assert!((p - 0.5).abs() < 1e-5);
        }
    }

    #[test]
    fn click_synth_has_downbeat_accent() {
        let rate = 44100;
        let clicks = synth_clicks(rate, 2.0, 0.5, 0.0, 4);
        // Find peak near t=0 (downbeat).
        let mut peak_0 = 0.0f32;
        for i in 0..(rate as usize / 10) {
            peak_0 = peak_0.max(clicks[i * 2].abs());
        }
        // Find peak near t=0.5 (upbeat).
        let offset = (rate as f32 * 0.5) as usize;
        let mut peak_1 = 0.0f32;
        for i in offset..(offset + rate as usize / 10) {
            peak_1 = peak_1.max(clicks[i * 2].abs());
        }
        assert!(
            peak_0 > peak_1 * 1.2,
            "downbeat {:.3} should be louder than upbeat {:.3}",
            peak_0,
            peak_1
        );
    }

    #[test]
    fn snap_finds_nearby_onset() {
        let rate = 44100;
        let samples = synthesize_click_track(120.0, 4.0, rate);
        let a = analyze(&samples, rate).expect("detected");
        // Snap a time that's close to but not on a beat.
        let snapped = snap_to_onset(&a, a.first_beat_secs + 0.02, 0.15);
        assert!(
            (snapped - a.first_beat_secs).abs() < 0.05,
            "snap drifted: {:.4} vs {:.4}",
            snapped,
            a.first_beat_secs
        );
    }
}
