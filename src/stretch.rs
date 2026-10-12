//! Phase-vocoder time-stretcher.
//!
//! Preserves pitch while changing duration. Correct for tonal and sustained
//! material. Transients are smeared compared to WSOLA; we will add a hybrid
//! or swap in Signalsmith Stretch for production use.

use rustfft::{num_complex::Complex, FftPlanner};
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug)]
pub struct StretchParams {
    pub fft_size: usize,
}

impl Default for StretchParams {
    fn default() -> Self {
        Self { fft_size: 2048 }
    }
}

/// Time-stretch interleaved stereo input.
///
/// `ratio` = output_length / input_length. Ratio > 1 makes it longer.
pub fn stretch_stereo(input: &[f32], ratio: f64, params: &StretchParams) -> Vec<f32> {
    if input.is_empty() || ratio <= 0.0 {
        return Vec::new();
    }
    let fft_size = params.fft_size;
    let hop_out = fft_size / 4;
    let hop_in = ((hop_out as f64) / ratio).round().max(1.0) as usize;

    let frames = input.len() / 2;
    let mut l = Vec::with_capacity(frames);
    let mut r = Vec::with_capacity(frames);
    for i in 0..frames {
        l.push(input[i * 2]);
        r.push(input[i * 2 + 1]);
    }

    let out_l = stretch_mono(&l, hop_in, hop_out, fft_size);
    let out_r = stretch_mono(&r, hop_in, hop_out, fft_size);

    let n = out_l.len().min(out_r.len());
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        out.push(out_l[i]);
        out.push(out_r[i]);
    }

    // Pad or trim to the exact target length so callers get what they asked for.
    let target_frames = ((frames as f64) * ratio).round() as usize;
    let cur_frames = out.len() / 2;
    if cur_frames < target_frames {
        out.resize(target_frames * 2, 0.0);
    } else if cur_frames > target_frames {
        out.truncate(target_frames * 2);
    }

    out
}

fn stretch_mono(input: &[f32], hop_in: usize, hop_out: usize, fft_size: usize) -> Vec<f32> {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);
    let ifft = planner.plan_fft_inverse(fft_size);

    // Hann window (periodic form). Applied on analysis and again on
    // synthesis, giving an effective Hann^2 weighting, which satisfies
    // constant-overlap-add at hop = N/4.
    let window: Vec<f32> = (0..fft_size)
        .map(|i| {
            let x = 2.0 * PI * i as f32 / fft_size as f32;
            0.5 * (1.0 - x.cos())
        })
        .collect();

    if input.len() < fft_size {
        return input.to_vec();
    }

    let num_frames = (input.len() - fft_size) / hop_in + 1;
    let out_len = (num_frames - 1) * hop_out + fft_size;
    let mut out = vec![0.0f32; out_len];

    let mut last_phase = vec![0.0f32; fft_size];
    let mut accum_phase = vec![0.0f32; fft_size];

    // Expected phase advance over hop_in samples for each FFT bin.
    let omega: Vec<f32> = (0..fft_size)
        .map(|k| 2.0 * PI * k as f32 / fft_size as f32)
        .collect();
    let expected: Vec<f32> = omega.iter().map(|w| w * hop_in as f32).collect();

    let mut buf = vec![Complex::<f32>::default(); fft_size];

    for frame in 0..num_frames {
        let in_start = frame * hop_in;

        for i in 0..fft_size {
            buf[i] = Complex::new(input[in_start + i] * window[i], 0.0);
        }
        fft.process(&mut buf);

        for k in 0..fft_size {
            let mag = buf[k].norm();
            let phase = buf[k].arg();

            if frame == 0 {
                last_phase[k] = phase;
                accum_phase[k] = phase;
                buf[k] = Complex::from_polar(mag, phase);
                continue;
            }

            let delta = phase - last_phase[k];
            let deviation = wrap_to_pi(delta - expected[k]);
            let true_advance = expected[k] + deviation;
            let out_advance = true_advance * (hop_out as f32) / (hop_in as f32);

            accum_phase[k] = wrap_to_pi(accum_phase[k] + out_advance);
            last_phase[k] = phase;

            buf[k] = Complex::from_polar(mag, accum_phase[k]);
        }

        ifft.process(&mut buf);

        let out_start = frame * hop_out;
        let scale = 1.0 / fft_size as f32;
        for i in 0..fft_size {
            out[out_start + i] += buf[i].re * scale * window[i];
        }
    }

    out
}

fn wrap_to_pi(mut x: f32) -> f32 {
    while x > PI {
        x -= 2.0 * PI;
    }
    while x < -PI {
        x += 2.0 * PI;
    }
    x
}
