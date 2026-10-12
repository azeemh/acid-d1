use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use std::fmt;
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::conv::IntoSample;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::sample::Sample;

#[derive(Debug)]
pub enum AudioError {
    MissingFmt,
    MissingData,
    BadFmt,
    UnsupportedBits(u16),
    DeviceError(String),
    NoDevice,
    DecodeFailed(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::MissingFmt => write!(f, "no fmt chunk"),
            AudioError::MissingData => write!(f, "no data chunk"),
            AudioError::BadFmt => write!(f, "malformed audio file"),
            AudioError::UnsupportedBits(b) => write!(f, "unsupported bit depth: {}", b),
            AudioError::DeviceError(s) => write!(f, "audio device error: {}", s),
            AudioError::NoDevice => write!(f, "no default output device"),
            AudioError::DecodeFailed(s) => write!(f, "decode failed: {}", s),
        }
    }
}

impl std::error::Error for AudioError {}

pub struct DecodedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl DecodedAudio {
    pub fn num_frames(&self) -> usize {
        self.samples.len() / 2
    }

    pub fn duration_secs(&self) -> f64 {
        self.num_frames() as f64 / self.sample_rate as f64
    }
}

/// Decode any audio format symphonia supports (WAV, MP3, FLAC, OGG, M4A/AAC,
/// Opus, WMA, etc.). Output is always interleaved stereo f32 at the source
/// sample rate.
pub fn decode_audio(data: &[u8]) -> Result<DecodedAudio, AudioError> {
    let cursor = Cursor::new(data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| AudioError::DecodeFailed(e.to_string()))?;

    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or(AudioError::BadFmt)?;

    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| AudioError::DecodeFailed(e.to_string()))?;

    let mut sample_rate: u32 = 0;
    let mut src_channels: usize = 0;
    let mut interleaved: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(_)) => break,
            Err(e) => return Err(AudioError::DecodeFailed(e.to_string())),
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                if sample_rate == 0 {
                    sample_rate = spec.rate;
                    src_channels = spec.channels.count();
                }
                copy_samples(&decoded, &mut interleaved);
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(_)) => continue,
            Err(e) => return Err(AudioError::DecodeFailed(e.to_string())),
        }
    }

    if sample_rate == 0 || src_channels == 0 || interleaved.is_empty() {
        return Err(AudioError::DecodeFailed(
            "no audio samples produced".to_string(),
        ));
    }

    // Normalize to stereo.
    let stereo = match src_channels {
        1 => {
            let mut v = Vec::with_capacity(interleaved.len() * 2);
            for s in interleaved {
                v.push(s);
                v.push(s);
            }
            v
        }
        2 => interleaved,
        n => {
            let frames = interleaved.len() / n;
            let mut v = Vec::with_capacity(frames * 2);
            for f in 0..frames {
                v.push(interleaved[f * n]);
                v.push(interleaved[f * n + 1]);
            }
            v
        }
    };

    Ok(DecodedAudio {
        samples: stereo,
        sample_rate,
    })
}

/// Backwards-compatible alias used by the rest of the codebase.
pub fn decode_wav(data: &[u8]) -> Result<DecodedAudio, AudioError> {
    decode_audio(data)
}

fn copy_samples(buf_ref: &AudioBufferRef, out: &mut Vec<f32>) {
    let frames = buf_ref.frames();
    let channels = buf_ref.spec().channels.count();
    match buf_ref {
        AudioBufferRef::U8(b) => push_buf(b, frames, channels, out),
        AudioBufferRef::U16(b) => push_buf(b, frames, channels, out),
        AudioBufferRef::U24(b) => push_buf(b, frames, channels, out),
        AudioBufferRef::U32(b) => push_buf(b, frames, channels, out),
        AudioBufferRef::S8(b) => push_buf(b, frames, channels, out),
        AudioBufferRef::S16(b) => push_buf(b, frames, channels, out),
        AudioBufferRef::S24(b) => push_buf(b, frames, channels, out),
        AudioBufferRef::S32(b) => push_buf(b, frames, channels, out),
        AudioBufferRef::F32(b) => push_buf(b, frames, channels, out),
        AudioBufferRef::F64(b) => push_buf(b, frames, channels, out),
    }
}

fn push_buf<T>(buf: &AudioBuffer<T>, frames: usize, channels: usize, out: &mut Vec<f32>)
where
    T: Sample + IntoSample<f32>,
{
    for f in 0..frames {
        for c in 0..channels {
            out.push(buf.chan(c)[f].into_sample());
        }
    }
}

pub fn resample_linear(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == dst_rate || input.is_empty() {
        return input.to_vec();
    }

    let in_frames = input.len() / 2;
    if in_frames < 2 {
        return input.to_vec();
    }
    let ratio = dst_rate as f64 / src_rate as f64;
    let out_frames = ((in_frames as f64) * ratio).round() as usize;
    if out_frames == 0 {
        return Vec::new();
    }
    let step = src_rate as f64 / dst_rate as f64;

    // Catmull-Rom cubic interpolation. 4 taps, no transcendentals.
    // For sample rates within a few percent of each other (the common case
    // for 44.1<->48 kHz) this is indistinguishable from a long sinc kernel.
    fn catmull_rom(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
        let a = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3;
        let b = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c = -0.5 * y0 + 0.5 * y2;
        let d = y1;
        ((a * t + b) * t + c) * t + d
    }

    let last = in_frames - 1;
    let mut out = vec![0.0f32; out_frames * 2];

    for i in 0..out_frames {
        let pos = i as f64 * step;
        let i1 = pos.floor() as isize;
        let t = (pos - i1 as f64) as f32;

        // Four taps around the sample position, clamped to bounds.
        let idx0 = (i1 - 1).clamp(0, last as isize) as usize;
        let idx1 = i1.clamp(0, last as isize) as usize;
        let idx2 = (i1 + 1).clamp(0, last as isize) as usize;
        let idx3 = (i1 + 2).clamp(0, last as isize) as usize;

        let l = catmull_rom(
            input[idx0 * 2],
            input[idx1 * 2],
            input[idx2 * 2],
            input[idx3 * 2],
            t,
        );
        let r = catmull_rom(
            input[idx0 * 2 + 1],
            input[idx1 * 2 + 1],
            input[idx2 * 2 + 1],
            input[idx3 * 2 + 1],
            t,
        );
        out[i * 2] = l;
        out[i * 2 + 1] = r;
    }

    out
}


pub fn remap_channels(stereo: &[f32], out_channels: usize) -> Vec<f32> {
    if out_channels == 2 {
        return stereo.to_vec();
    }
    let frames = stereo.len() / 2;
    let mut out = vec![0.0f32; frames * out_channels];
    for f in 0..frames {
        let l = stereo[f * 2];
        let r = stereo[f * 2 + 1];
        if out_channels == 1 {
            out[f] = (l + r) * 0.5;
        } else {
            out[f * out_channels] = l;
            out[f * out_channels + 1] = r;
        }
    }
    out
}

pub fn play_stereo(samples: &[f32], sample_rate: u32) -> Result<(), AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(AudioError::NoDevice)?;
    let config = device
        .default_output_config()
        .map_err(|e| AudioError::DeviceError(e.to_string()))?;

    let device_rate = config.sample_rate().0;
    let device_ch = config.channels() as usize;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

    let resampled = resample_linear(samples, sample_rate, device_rate);
    let output = remap_channels(&resampled, device_ch);
    let total = output.len();

    let shared = Arc::new(output);
    let position = Arc::new(AtomicUsize::new(0));

    let err_fn = |e| eprintln!("stream error: {}", e);

    let stream = match sample_format {
        SampleFormat::F32 => {
            let s = shared.clone();
            let p = position.clone();
            device
                .build_output_stream(
                    &stream_config,
                    move |buf: &mut [f32], _| fill_f32(&s, &p, buf),
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::DeviceError(e.to_string()))?
        }
        SampleFormat::I16 => {
            let s = shared.clone();
            let p = position.clone();
            device
                .build_output_stream(
                    &stream_config,
                    move |buf: &mut [i16], _| fill_i16(&s, &p, buf),
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::DeviceError(e.to_string()))?
        }
        SampleFormat::U16 => {
            let s = shared.clone();
            let p = position.clone();
            device
                .build_output_stream(
                    &stream_config,
                    move |buf: &mut [u16], _| fill_u16(&s, &p, buf),
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::DeviceError(e.to_string()))?
        }
        other => {
            return Err(AudioError::DeviceError(format!(
                "unsupported sample format: {:?}",
                other
            )))
        }
    };

    stream
        .play()
        .map_err(|e| AudioError::DeviceError(e.to_string()))?;

    loop {
        if position.load(Ordering::Relaxed) >= total {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(200));
    drop(stream);
    Ok(())
}

fn fill_f32(shared: &[f32], position: &AtomicUsize, buf: &mut [f32]) {
    let pos = position.load(Ordering::Relaxed);
    for (i, out) in buf.iter_mut().enumerate() {
        let idx = pos + i;
        *out = if idx < shared.len() { shared[idx] } else { 0.0 };
    }
    position.store(pos + buf.len(), Ordering::Relaxed);
}

fn fill_i16(shared: &[f32], position: &AtomicUsize, buf: &mut [i16]) {
    let pos = position.load(Ordering::Relaxed);
    for (i, out) in buf.iter_mut().enumerate() {
        let idx = pos + i;
        let s = if idx < shared.len() { shared[idx] } else { 0.0 };
        *out = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
    }
    position.store(pos + buf.len(), Ordering::Relaxed);
}

fn fill_u16(shared: &[f32], position: &AtomicUsize, buf: &mut [u16]) {
    let pos = position.load(Ordering::Relaxed);
    for (i, out) in buf.iter_mut().enumerate() {
        let idx = pos + i;
        let s = if idx < shared.len() { shared[idx] } else { 0.0 };
        let v = s.clamp(-1.0, 1.0) * 0.5 + 0.5;
        *out = (v * u16::MAX as f32) as u16;
    }
    position.store(pos + buf.len(), Ordering::Relaxed);
}
