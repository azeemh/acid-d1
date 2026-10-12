use crate::acid::{build_acid_chunk_body, AcidInfo};
use crate::audio::decode_wav;
use crate::gen::{build_acid_wav, build_click_loop, WavSpec};
use crate::mix::Mixer;
use crate::stretch::{stretch_stereo, StretchParams};
use crate::wav::{write_wav_to_vec, PcmFormat, RiffFile};

fn sample_info() -> AcidInfo {
    AcidInfo {
        flags: 1,
        is_loop: true,
        root_note: 60.0,
        beats: 8.0,
        meter_den: 4,
        meter_num: 4,
        tempo: 120.0,
    }
}

#[test]
fn riff_parser_finds_acid_chunk() {
    let info = sample_info();
    let wav = build_acid_wav(&WavSpec::default(), &info);
    let riff = RiffFile::parse(&wav).expect("parse riff");
    assert!(riff.find(b"fmt ").is_some());
    assert!(riff.find(b"acid").is_some());
    assert!(riff.find(b"data").is_some());
}

#[test]
fn acid_round_trip() {
    let info = sample_info();
    let wav = build_acid_wav(&WavSpec::default(), &info);
    let riff = RiffFile::parse(&wav).expect("parse riff");
    let parsed = AcidInfo::from_riff(&riff).expect("parse acid");
    assert_eq!(parsed.flags, info.flags);
    assert_eq!(parsed.is_loop, info.is_loop);
    assert!((parsed.root_note - info.root_note).abs() < 1e-6);
    assert!((parsed.beats - info.beats).abs() < 1e-6);
    assert_eq!(parsed.meter_den, info.meter_den);
    assert_eq!(parsed.meter_num, info.meter_num);
    assert!((parsed.tempo - info.tempo).abs() < 1e-6);
}

#[test]
fn root_note_maps_to_middle_c() {
    let info = sample_info();
    assert_eq!(info.root_midi(), 60);
    assert_eq!(info.note_name(), "C4");
    let hz = info.root_hz();
    assert!((hz - 261.6256).abs() < 0.01, "got {} Hz", hz);
}

#[test]
fn fmt_chunk_parses() {
    let info = sample_info();
    let spec = WavSpec::default();
    let wav = build_acid_wav(&spec, &info);
    let riff = RiffFile::parse(&wav).expect("parse riff");
    let fmt_chunk = riff.find(b"fmt ").expect("fmt chunk");
    let fmt = PcmFormat::parse(&fmt_chunk.data).expect("parse fmt");
    assert_eq!(fmt.channels, spec.channels);
    assert_eq!(fmt.sample_rate, spec.sample_rate);
    assert_eq!(fmt.bits_per_sample, spec.bits_per_sample);
}

#[test]
fn missing_acid_chunk_errors_cleanly() {
    let body: &[u8] = b"WAVE";
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    let riff = RiffFile::parse(&out).expect("parse empty riff");
    assert!(AcidInfo::from_riff(&riff).is_err());
}

#[test]
fn build_chunk_body_is_24_bytes() {
    let info = sample_info();
    let body = build_acid_chunk_body(&info);
    assert_eq!(body.len(), 24);
}

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

fn dominant_freq_mono(samples: &[f32], rate: u32) -> f32 {
    use rustfft::num_complex::Complex;
    use rustfft::FftPlanner;

    let frames = samples.len() / 2;
    let mut n = 1usize;
    while n * 2 <= frames.min(16384) {
        n *= 2;
    }
    let start = (frames - n) / 2;
    let mut buf: Vec<Complex<f32>> = (0..n)
        .map(|i| Complex::new(samples[(start + i) * 2], 0.0))
        .collect();

    for i in 0..n {
        let x = std::f32::consts::PI * i as f32 / (n - 1) as f32;
        let w = 0.5 * (1.0 - x.cos());
        buf[i] *= w;
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    fft.process(&mut buf);

    let half = n / 2;
    let mut best_k = 0usize;
    let mut best_mag = 0.0f32;
    for k in 2..half {
        let m = buf[k].norm();
        if m > best_mag {
            best_mag = m;
            best_k = k;
        }
    }
    best_k as f32 * rate as f32 / n as f32
}

#[test]
fn stretch_preserves_length_ratio() {
    let rate = 44100;
    let input = make_sine(rate as usize, rate, 440.0);
    let params = StretchParams::default();
    for ratio in [0.5f64, 0.75, 1.5, 2.0] {
        let out = stretch_stereo(&input, ratio, &params);
        let expected = (rate as f64 * ratio).round() as usize;
        let got = out.len() / 2;
        let err = ((got as f64) - (expected as f64)).abs() / expected as f64;
        assert!(err < 0.01, "ratio={}: err {:.3}", ratio, err);
    }
}

#[test]
fn stretch_preserves_pitch_fft() {
    let rate = 44100;
    let input = make_sine(rate as usize, rate, 440.0);
    let params = StretchParams::default();
    for ratio in [0.5f64, 0.75, 1.5, 2.0] {
        let out = stretch_stereo(&input, ratio, &params);
        let f = dominant_freq_mono(&out, rate);
        assert!(
            (f - 440.0).abs() < 15.0,
            "ratio={}: expected ~440 Hz, got {:.1} Hz",
            ratio,
            f
        );
    }
}

#[test]
fn wav_writer_round_trips() {
    let rate = 44100;
    let input = make_sine(rate as usize / 4, rate, 440.0);
    let bytes = write_wav_to_vec(&input, rate, 2);

    let riff = RiffFile::parse(&bytes).expect("parse riff");
    assert!(riff.find(b"fmt ").is_some());
    assert!(riff.find(b"data").is_some());

    let decoded = decode_wav(&bytes).expect("decode");
    assert_eq!(decoded.sample_rate, rate);
    assert_eq!(decoded.samples.len(), input.len());

    let mut max_err = 0.0f32;
    for (a, b) in input.iter().zip(decoded.samples.iter()) {
        let e = (a - b).abs();
        if e > max_err {
            max_err = e;
        }
    }
    assert!(max_err < 1e-3, "max round-trip error {}", max_err);
}

#[test]
fn mixer_adds_at_offset() {
    let mut m = Mixer::new(44100, 0);
    let block = vec![0.5f32; 4];
    m.add_at(&block, 1, 1.0);
    assert_eq!(m.len_frames(), 3);
    assert_eq!(m.samples[0], 0.0);
    assert_eq!(m.samples[2], 0.5);
}

#[test]
fn mixer_normalize() {
    let mut m = Mixer::new(44100, 0);
    let block = vec![0.25f32; 4];
    m.add_at(&block, 0, 1.0);
    m.normalize_to(1.0);
    assert!((m.peak() - 1.0).abs() < 1e-6);
}

#[test]
fn two_loops_stretch_to_common_project_length() {
    let project_bpm = 120.0f32;
    let params = StretchParams::default();

    let a_bytes = build_click_loop(8.0, 128.0, 440.0, 44100, 2);
    let a = decode_wav(&a_bytes).expect("decode a");

    let b_bytes = build_click_loop(8.0, 90.0, 660.0, 44100, 2);
    let b = decode_wav(&b_bytes).expect("decode b");

    let a_stretch = (128.0f32 / project_bpm) as f64;
    let b_stretch = (90.0f32 / project_bpm) as f64;

    let a_out = stretch_stereo(&a.samples, a_stretch, &params);
    let b_out = stretch_stereo(&b.samples, b_stretch, &params);

    let expected = (8.0 * 60.0 / project_bpm * 44100.0).round() as usize;

    let a_frames = a_out.len() / 2;
    let b_frames = b_out.len() / 2;

    let a_err = ((a_frames as f64) - (expected as f64)).abs() / expected as f64;
    let b_err = ((b_frames as f64) - (expected as f64)).abs() / expected as f64;

    assert!(a_err < 0.005, "a: {} frames, expected {}", a_frames, expected);
    assert!(b_err < 0.005, "b: {} frames, expected {}", b_frames, expected);

    let diff = (a_frames as i64 - b_frames as i64).abs();
    assert!(diff < 200, "a and b differ by {} frames", diff);
}
