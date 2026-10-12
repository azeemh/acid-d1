//! Streaming multitrack player with clip-based voices.
//!
//! Each track owns a list of clips. Every clip is an independent voice with
//! its own Signalsmith engine and its own read position, so a clip boundary
//! is a clean crossfade point rather than a phase discontinuity.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use signalsmith_stretch::Stretch;
use std::sync::{Arc, Mutex};

const MIX_FRAMES: usize = 2048;
const SCRATCH_FRAMES: usize = 16384;

#[derive(Clone, Copy, Debug)]
pub struct ClipSpec {
    pub start_beats: f32,
    pub length_beats: f32,
    pub source_offset_frames: usize,
    pub fade_in_beats: f32,
    pub fade_out_beats: f32,
}

pub struct TrackSource {
    pub samples: Arc<Vec<f32>>,
    pub native_tempo: f32,
    pub pitch_semitones: f32,
    pub gain: f32,
    pub muted: bool,
    pub solo: bool,
    pub clips: Vec<ClipSpec>,
}

pub struct TrackAudio {
    pub samples: Vec<f32>,
    pub gain: f32,
    pub muted: bool,
    pub solo: bool,
}

struct ClipVoice {
    start_beats: f32,
    length_beats: f32,
    source_offset_frames: usize,
    fade_in_beats: f32,
    fade_out_beats: f32,
    engine: Option<Stretch>,
    read_pos: usize,
    was_active: bool,
    scratch_in: Vec<f32>,
    scratch_out: Vec<f32>,
}

impl ClipVoice {
    fn new(spec: ClipSpec) -> Self {
        Self {
            start_beats: spec.start_beats,
            length_beats: spec.length_beats,
            source_offset_frames: spec.source_offset_frames,
            fade_in_beats: spec.fade_in_beats,
            fade_out_beats: spec.fade_out_beats,
            engine: None,
            read_pos: 0,
            was_active: false,
            scratch_in: vec![0.0; SCRATCH_FRAMES * 2],
            scratch_out: vec![0.0; SCRATCH_FRAMES * 2],
        }
    }

    fn reset(&mut self) {
        self.read_pos = 0;
        self.was_active = false;
        if let Some(e) = &mut self.engine {
            e.reset();
        }
    }

    fn envelope_at(&self, project_beat: f32) -> f32 {
        let into = project_beat - self.start_beats;
        let from_end = self.length_beats - into;
        let mut e = 1.0f32;
        if self.fade_in_beats > 0.0 && into < self.fade_in_beats {
            e = e.min((into / self.fade_in_beats).max(0.0));
        }
        if self.fade_out_beats > 0.0 && from_end < self.fade_out_beats {
            e = e.min((from_end / self.fade_out_beats).max(0.0));
        }
        e.clamp(0.0, 1.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn produce(
        &mut self,
        mix: &mut [f32],
        n_frames: usize,
        project_pos_secs: f64,
        beat_secs: f64,
        dt: f64,
        s_ratio: f64,
        source: &[f32],
        src_frames: usize,
        gain: f32,
        pitch: f32,
        device_rate: u32,
    ) {
        let block_start = self.start_beats;
        let block_end = self.start_beats + self.length_beats;
        let t0 = (project_pos_secs / beat_secs) as f32;
        let t1 = ((project_pos_secs + n_frames as f64 * dt) / beat_secs) as f32;

        if t1 < block_start || t0 >= block_end {
            self.was_active = false;
            return;
        }

        let inside_at_start = t0 >= block_start && t0 < block_end;
        if inside_at_start && !self.was_active {
            self.read_pos = 0;
            if let Some(e) = &mut self.engine {
                e.reset();
            }
        }
        self.was_active = inside_at_start;

        let mut af = n_frames;
        let mut at = 0usize;
        for i in 0..n_frames {
            let t = project_pos_secs + i as f64 * dt;
            let b = (t / beat_secs) as f32;
            if b >= block_start && b < block_end {
                if af == n_frames {
                    af = i;
                }
                at = i + 1;
            }
        }
        if af >= at {
            return;
        }
        let active_len = at - af;

        let needs_stretch = (s_ratio - 1.0).abs() > 1e-4 || pitch.abs() > 1e-3;

        if !needs_stretch {
            for i in 0..active_len {
                let src_idx = self.source_offset_frames + self.read_pos;
                if src_idx >= src_frames {
                    break;
                }
                let oi = af + i;
                let t = project_pos_secs + oi as f64 * dt;
                let b = (t / beat_secs) as f32;
                let env = self.envelope_at(b);
                mix[oi * 2] += source[src_idx * 2] * gain * env;
                mix[oi * 2 + 1] += source[src_idx * 2 + 1] * gain * env;
                self.read_pos += 1;
            }
            return;
        }

        if self.engine.is_none() {
            let mut e = Stretch::preset_default(2, device_rate);
            e.set_transpose_factor_semitones(pitch, None);
            self.engine = Some(e);
        }

        let n_out = active_len.min(SCRATCH_FRAMES);
        let n_in_needed = (((n_out as f64) / s_ratio).ceil() as usize).max(2);
        let remaining =
            src_frames.saturating_sub(self.source_offset_frames + self.read_pos);
        let n_in = n_in_needed.min(remaining).min(SCRATCH_FRAMES);

        let mut scratch_in = std::mem::take(&mut self.scratch_in);
        let mut scratch_out = std::mem::take(&mut self.scratch_out);

        for i in 0..n_in {
            let src_idx = self.source_offset_frames + self.read_pos + i;
            scratch_in[i * 2] = source[src_idx * 2];
            scratch_in[i * 2 + 1] = source[src_idx * 2 + 1];
        }
        self.read_pos += n_in;

        if let Some(engine) = self.engine.as_mut() {
            engine.process(
                &scratch_in[..n_in * 2],
                &mut scratch_out[..n_out * 2],
            );
        }

        for i in 0..n_out {
            let oi = af + i;
            let t = project_pos_secs + oi as f64 * dt;
            let b = (t / beat_secs) as f32;
            let env = self.envelope_at(b);
            mix[oi * 2] += scratch_out[i * 2] * gain * env;
            mix[oi * 2 + 1] += scratch_out[i * 2 + 1] * gain * env;
        }

        self.scratch_in = scratch_in;
        self.scratch_out = scratch_out;
    }
}

struct TrackStream {
    source: Arc<Vec<f32>>,
    native_tempo: f32,
    pitch_semitones: f32,
    gain: f32,
    muted: bool,
    solo: bool,
    clips: Vec<ClipVoice>,
    device_rate: u32,
}

impl TrackStream {
    fn new(src: TrackSource, device_rate: u32) -> Self {
        Self {
            source: src.samples,
            native_tempo: src.native_tempo,
            pitch_semitones: src.pitch_semitones,
            gain: src.gain,
            muted: src.muted,
            solo: src.solo,
            clips: src.clips.into_iter().map(ClipVoice::new).collect(),
            device_rate,
        }
    }

    fn reset(&mut self) {
        for c in &mut self.clips {
            c.reset();
        }
    }

    /// Move every clip's read position so that playback from `project_beat`
    /// lands on the correct source frame. Clips whose block does not contain
    /// the beat are reset to their start.
    fn seek_to_project_beat(&mut self, project_beat: f32, bpm: f32) {
        let device_rate = self.device_rate as f32;
        let native = if self.native_tempo > 0.0 {
            self.native_tempo
        } else {
            bpm
        };
        let frames_per_beat = 60.0 * device_rate / native.max(1.0);
        for clip in &mut self.clips {
            let end = clip.start_beats + clip.length_beats;
            if project_beat >= clip.start_beats && project_beat < end {
                let beats_into = project_beat - clip.start_beats;
                let frames_into = (beats_into * frames_per_beat).round() as usize;
                clip.read_pos = frames_into;
                clip.was_active = true;
                if let Some(e) = &mut clip.engine {
                    e.reset();
                }
            } else {
                clip.read_pos = 0;
                clip.was_active = false;
                if let Some(e) = &mut clip.engine {
                    e.reset();
                }
            }
        }
    }

    fn produce(
        &mut self,
        mix: &mut [f32],
        n_frames: usize,
        project_pos_secs: f64,
        bpm: f32,
    ) {
        if self.muted || n_frames == 0 {
            return;
        }
        let src_frames = self.source.len() / 2;
        if src_frames == 0 || self.clips.is_empty() {
            return;
        }

        let native = if self.native_tempo > 0.0 {
            self.native_tempo
        } else {
            bpm
        };
        let beat_secs = 60.0 / bpm.max(1.0) as f64;
        let dt = 1.0 / self.device_rate as f64;
        let s_ratio = native as f64 / bpm.max(1.0) as f64;
        let pitch = self.pitch_semitones;
        let gain = self.gain;
        let device_rate = self.device_rate;
        let source = Arc::clone(&self.source);

        for clip in self.clips.iter_mut() {
            clip.produce(
                mix,
                n_frames,
                project_pos_secs,
                beat_secs,
                dt,
                s_ratio,
                &source,
                src_frames,
                gain,
                pitch,
                device_rate,
            );
        }
    }
}

struct Shared {
    tracks: Vec<TrackStream>,
    project_bpm: f32,
    device_rate: u32,
    playing: bool,
    looping: bool,
    loop_start_frac: f64,
    loop_end_frac: f64,
    project_pos_secs: f64,
    project_length_secs: f64,
    /// Delay between when we submit audio and when it reaches the speakers,
    /// in seconds. Playhead display subtracts this.
    latency_comp_secs: f64,
    mix: Vec<f32>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            project_bpm: 120.0,
            device_rate: 48000,
            playing: false,
            looping: false,
            loop_start_frac: 0.0,
            loop_end_frac: 1.0,
            project_pos_secs: 0.0,
            project_length_secs: 0.0,
            latency_comp_secs: 0.0,
            mix: vec![0.0; MIX_FRAMES * 2],
        }
    }
}

pub struct Player {
    shared: Arc<Mutex<Shared>>,
    device_sample_rate: u32,
    output_channels: usize,
    _stream: cpal::Stream,
}

impl Player {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string())?;

        // Match PipeWire's clock rate when possible. If the default config
        // says 48000 but PipeWire is running at 192000, cpal's "default"
        // hides a hidden resample that costs latency. We open at 192000
        // instead and let PipeWire pass it through untouched.
        let mut chosen_rate = 192_000u32;
        let mut chosen_channels = 2u16;
        let mut chosen_format = cpal::SampleFormat::F32;
        let mut picked_explicit = false;

        if let Ok(configs) = device.supported_output_configs() {
            // First pass: prefer F32.
            // Second pass: accept I16 or U16.
            // Ignore U8 and anything else the engine cannot convert.
            for want_f32 in [true, false] {
                for c in device.supported_output_configs().unwrap() {
                    if c.channels() < 2 {
                        continue;
                    }
                    if c.min_sample_rate().0 > 192_000
                        || c.max_sample_rate().0 < 192_000
                    {
                        continue;
                    }
                    let fmt = c.sample_format();
                    let ok = match fmt {
                        cpal::SampleFormat::F32 => want_f32,
                        cpal::SampleFormat::I16 | cpal::SampleFormat::U16 => !want_f32,
                        _ => false,
                    };
                    if !ok {
                        continue;
                    }
                    chosen_rate = 192_000;
                    chosen_channels = 2;
                    chosen_format = fmt;
                    picked_explicit = true;
                    break;
                }
                if picked_explicit {
                    break;
                }
            }
            let _ = configs;
        }

        let (device_sample_rate, output_channels, sample_format, stream_config) =
            if picked_explicit {
                (
                    chosen_rate,
                    chosen_channels as usize,
                    chosen_format,
                    cpal::StreamConfig {
                        channels: chosen_channels,
                        sample_rate: cpal::SampleRate(chosen_rate),
                        buffer_size: cpal::BufferSize::Default,
                    },
                )
            } else {
                let config =
                    device.default_output_config().map_err(|e| e.to_string())?;
                let rate = config.sample_rate().0;
                let ch = config.channels() as usize;
                let fmt = config.sample_format();
                let sc: cpal::StreamConfig = config.into();
                (rate, ch, fmt, sc)
            };

        eprintln!(
            "acid-ui: audio stream {} Hz, {} ch, {:?}",
            device_sample_rate, output_channels, sample_format
        );

        let shared = Arc::new(Mutex::new(Shared {
            device_rate: device_sample_rate,
            ..Shared::default()
        }));
        let err_fn = |e| eprintln!("stream error: {}", e);

        let stream = match sample_format {
            SampleFormat::F32 => {
                let s = shared.clone();
                let ch = output_channels;
                device
                    .build_output_stream(
                        &stream_config,
                        move |buf: &mut [f32], _| fill_f32(&s, buf, ch),
                        err_fn,
                        None,
                    )
                    .map_err(|e| e.to_string())?
            }
            SampleFormat::I16 => {
                let s = shared.clone();
                let ch = output_channels;
                device
                    .build_output_stream(
                        &stream_config,
                        move |buf: &mut [i16], _| fill_i16(&s, buf, ch),
                        err_fn,
                        None,
                    )
                    .map_err(|e| e.to_string())?
            }
            SampleFormat::U16 => {
                let s = shared.clone();
                let ch = output_channels;
                device
                    .build_output_stream(
                        &stream_config,
                        move |buf: &mut [u16], _| fill_u16(&s, buf, ch),
                        err_fn,
                        None,
                    )
                    .map_err(|e| e.to_string())?
            }
            other => return Err(format!("unsupported sample format: {:?}", other)),
        };

        stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            device_sample_rate,
            output_channels,
            _stream: stream,
        })
    }

    pub fn device_sample_rate(&self) -> u32 {
        self.device_sample_rate
    }
    pub fn output_channels(&self) -> usize {
        self.output_channels
    }

    fn recompute_length(s: &mut Shared) {
        let mut max_end = 0.0f32;
        for t in &s.tracks {
            for c in &t.clips {
                let end = c.start_beats + c.length_beats;
                if end > max_end {
                    max_end = end;
                }
            }
        }
        s.project_length_secs =
            (max_end as f64) * 60.0 / s.project_bpm.max(1.0) as f64;
    }

    pub fn set_tracks(&self, sources: Vec<TrackSource>) {
        let mut s = self.shared.lock().unwrap();
        let device_rate = s.device_rate;
        let mut old = std::mem::take(&mut s.tracks);
        let mut new = Vec::with_capacity(sources.len());
        for src in sources {
            let matched = old
                .iter()
                .position(|t| Arc::ptr_eq(&t.source, &src.samples));
            match matched {
                Some(i) => {
                    let mut t = old.remove(i);
                    if (t.pitch_semitones - src.pitch_semitones).abs() > 1e-4 {
                        t.pitch_semitones = src.pitch_semitones;
                        for c in &mut t.clips {
                            if let Some(e) = &mut c.engine {
                                e.set_transpose_factor_semitones(
                                    src.pitch_semitones,
                                    None,
                                );
                            }
                        }
                    }
                    let clips_changed = t.clips.len() != src.clips.len()
                        || t.clips.iter().zip(src.clips.iter()).any(|(a, b)| {
                            (a.start_beats - b.start_beats).abs() > 1e-4
                                || (a.length_beats - b.length_beats).abs() > 1e-4
                                || a.source_offset_frames != b.source_offset_frames
                        });
                    if clips_changed {
                        t.clips =
                            src.clips.iter().copied().map(ClipVoice::new).collect();
                    }
                    t.native_tempo = src.native_tempo;
                    t.gain = src.gain;
                    t.muted = src.muted;
                    t.solo = src.solo;
                    new.push(t);
                }
                None => new.push(TrackStream::new(src, device_rate)),
            }
        }
        s.tracks = new;
        Self::recompute_length(&mut s);
    }

    pub fn set_tempo(&self, bpm: f32) {
        let mut s = self.shared.lock().unwrap();
        s.project_bpm = bpm;
        Self::recompute_length(&mut s);
    }

    pub fn update_track_gain(&self, idx: usize, gain: f32) {
        let mut s = self.shared.lock().unwrap();
        if let Some(t) = s.tracks.get_mut(idx) {
            t.gain = gain;
        }
    }
    pub fn update_track_mute(&self, idx: usize, muted: bool) {
        let mut s = self.shared.lock().unwrap();
        if let Some(t) = s.tracks.get_mut(idx) {
            t.muted = muted;
        }
    }
    pub fn update_track_solo(&self, idx: usize, solo: bool) {
        let mut s = self.shared.lock().unwrap();
        if let Some(t) = s.tracks.get_mut(idx) {
            t.solo = solo;
        }
    }

    pub fn play(&self) {
        let mut s = self.shared.lock().unwrap();
        if s.project_pos_secs >= s.project_length_secs {
            s.project_pos_secs = 0.0;
            for t in &mut s.tracks {
                t.reset();
            }
        }
        s.playing = true;
    }
    pub fn stop(&self) {
        let mut s = self.shared.lock().unwrap();
        s.playing = false;
        s.project_pos_secs = 0.0;
        for t in &mut s.tracks {
            t.reset();
        }
    }
    pub fn is_playing(&self) -> bool {
        self.shared.lock().unwrap().playing
    }
    pub fn position_frac(&self) -> f64 {
        let s = self.shared.lock().unwrap();
        if s.project_length_secs <= 0.0 {
            return 0.0;
        }
        let heard_secs =
            (s.project_pos_secs - s.latency_comp_secs).max(0.0);
        (heard_secs / s.project_length_secs).clamp(0.0, 1.0)
    }

    pub fn latency_comp_secs(&self) -> f64 {
        self.shared.lock().unwrap().latency_comp_secs
    }

    pub fn set_latency_comp_secs(&self, secs: f64) {
        let mut s = self.shared.lock().unwrap();
        s.latency_comp_secs = secs.clamp(-2.0, 4.0);
    }
    pub fn total_frames(&self) -> usize {
        let s = self.shared.lock().unwrap();
        (s.project_length_secs * s.device_rate as f64) as usize
    }
    pub fn set_position_frac(&self, frac: f64) {
        let mut s = self.shared.lock().unwrap();
        let frac = frac.clamp(0.0, 1.0);
        s.project_pos_secs = frac * s.project_length_secs;
        let bpm = s.project_bpm;
        let project_beat = (s.project_pos_secs * bpm as f64 / 60.0) as f32;
        for t in &mut s.tracks {
            t.seek_to_project_beat(project_beat, bpm);
        }
    }
    pub fn set_looping(&self, on: bool) {
        self.shared.lock().unwrap().looping = on;
    }
    pub fn set_loop_region_frac(&self, start: f64, end: f64) {
        let mut s = self.shared.lock().unwrap();
        s.loop_start_frac = start.clamp(0.0, 1.0);
        s.loop_end_frac = end.clamp(0.0, 1.0);
    }
}

/// Soft-knee limiter.
///
/// Below THRESHOLD: unity, completely transparent.
/// Above THRESHOLD: an asymptotic tanh rolloff that approaches 1.0.
///
/// THRESHOLD is set low (0.5 = -6 dBFS) so the transition into compression
/// is gradual and the top of the signal is shaped by a smooth curve rather
/// than being clipped. tanh is C1 at the knee because tanh'(0) = 1, so there
/// is no derivative discontinuity and no audible "kink".
#[inline]
fn limit(x: f32) -> f32 {
    const THRESHOLD: f32 = 0.5;
    const RANGE: f32 = 1.0 - THRESHOLD;
    let ax = x.abs();
    if ax <= THRESHOLD {
        x
    } else {
        let over = (ax - THRESHOLD) / RANGE;
        let compressed = THRESHOLD + RANGE * over.tanh();
        compressed.copysign(x)
    }
}

fn fill_f32(shared: &Arc<Mutex<Shared>>, buf: &mut [f32], out_ch: usize) {
    let mut s = shared.lock().unwrap();
    let n_frames = buf.len() / out_ch;
    if out_ch == 0 || n_frames == 0 {
        for x in buf.iter_mut() {
            *x = 0.0;
        }
        return;
    }
    if !s.playing || s.tracks.is_empty() {
        for x in buf.iter_mut() {
            *x = 0.0;
        }
        return;
    }

    let mut tracks = std::mem::take(&mut s.tracks);
    let mut mix = std::mem::take(&mut s.mix);
    let bpm = s.project_bpm;
    let n = n_frames.min(MIX_FRAMES);
    let looping = s.looping;
    let loop_start_secs = s.loop_start_frac * s.project_length_secs;
    let loop_end_secs = s.loop_end_frac * s.project_length_secs;
    let use_loop = looping && loop_end_secs > loop_start_secs;
    let device_rate_f = s.device_rate as f64;

    // Clear the mix region we will use.
    for v in mix[..n * 2].iter_mut() {
        *v = 0.0;
    }

    let any_solo = tracks.iter().any(|t| t.solo);

    let mut pos = s.project_pos_secs;
    let mut written = 0usize;

    // Segment loop: process up to the loop end, then wrap and continue.
    while written < n {
        // Wrap if the playhead is already past the loop end.
        if use_loop && pos >= loop_end_secs {
            pos = loop_start_secs;
            let project_beat = (pos * bpm as f64 / 60.0) as f32;
            for t in tracks.iter_mut() {
                t.seek_to_project_beat(project_beat, bpm);
            }
        }

        // Frames we can produce before hitting the loop end.
        let remaining_out = n - written;
        let chunk = if use_loop {
            let secs_to_end = (loop_end_secs - pos).max(0.0);
            let frames_to_end = (secs_to_end * device_rate_f).ceil() as usize;
            frames_to_end.max(1).min(remaining_out)
        } else {
            remaining_out
        };

        let slice = &mut mix[written * 2..(written + chunk) * 2];
        for t in tracks.iter_mut() {
            if t.muted {
                continue;
            }
            if any_solo && !t.solo {
                continue;
            }
            t.produce(slice, chunk, pos, bpm);
        }

        pos += chunk as f64 / device_rate_f;
        written += chunk;
    }

    // Write the mix out to the output buffer.
    for f in 0..n {
        let l = limit(mix[f * 2]);
        let r = limit(mix[f * 2 + 1]);
        let base = f * out_ch;
        if out_ch == 1 {
            buf[base] = (l + r) * 0.5;
        } else {
            buf[base] = l;
            buf[base + 1] = r;
            for c in 2..out_ch {
                buf[base + c] = 0.0;
            }
        }
    }
    for f in n..n_frames {
        let base = f * out_ch;
        for c in 0..out_ch {
            buf[base + c] = 0.0;
        }
    }

    // Persist final position.
    s.project_pos_secs = pos;
    if !use_loop && s.project_pos_secs >= s.project_length_secs {
        s.playing = false;
    }

    s.tracks = tracks;
    s.mix = mix;
}

fn fill_i16(shared: &Arc<Mutex<Shared>>, buf: &mut [i16], out_ch: usize) {
    let mut tmp = vec![0.0f32; buf.len()];
    fill_f32(shared, &mut tmp, out_ch);
    for (o, v) in buf.iter_mut().zip(tmp.iter()) {
        *o = (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
    }
}
fn fill_u16(shared: &Arc<Mutex<Shared>>, buf: &mut [u16], out_ch: usize) {
    let mut tmp = vec![0.0f32; buf.len()];
    fill_f32(shared, &mut tmp, out_ch);
    for (o, v) in buf.iter_mut().zip(tmp.iter()) {
        let x = v.clamp(-1.0, 1.0) * 0.5 + 0.5;
        *o = (x * u16::MAX as f32) as u16;
    }
}
