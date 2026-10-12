//! Native PipeWire backend for lock-free, pre-rendered high-resolution clip streaming.
use pipewire as pw;
use pw::thread_loop::ThreadLoop;
use pw::stream::{Stream, StreamFlags, StreamListener};
use pw::context::Context;
use pw::properties::properties;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::pod::Pod;
use std::sync::{Arc, Mutex};

pub const SAMPLE_RATE: u32 = 192_000;
pub const CHANNELS: u32 = 2;
const QUANTUM: usize = 4096;

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

struct ClipVoice {
    start_beats: f32,
    length_beats: f32,
    fade_in_beats: f32,
    fade_out_beats: f32,
    was_active: bool,
    stretched_cache: Vec<f32>,
}

impl ClipVoice {
    fn new(spec: ClipSpec) -> Self {
        Self {
            start_beats: spec.start_beats,
            length_beats: spec.length_beats,
            fade_in_beats: spec.fade_in_beats,
            fade_out_beats: spec.fade_out_beats,
            was_active: false,
            stretched_cache: Vec::new(),
        }
    }
    fn reset(&mut self) {
        self.was_active = false;
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
}

impl TrackStream {
    fn reset(&mut self) {
        for c in &mut self.clips { c.reset(); }
    }
    fn seek_to_project_beat(&mut self, _project_beat: f32, _bpm: f32) {
        for clip in &mut self.clips {
            clip.was_active = false;
        }
    }
    fn produce(&mut self, mix: &mut [f32], n_frames: usize, project_pos_secs: f64, bpm: f32) {
        if self.muted || n_frames == 0 || self.clips.is_empty() { return; }

        let beat_secs = 60.0 / bpm.max(1.0) as f64;
        let dt = 1.0 / SAMPLE_RATE as f64;
        let gain = self.gain;

        for clip in self.clips.iter_mut() {
            let block_start = clip.start_beats;
            let block_end = clip.start_beats + clip.length_beats;
            let t0 = (project_pos_secs / beat_secs) as f32;
            let t1 = ((project_pos_secs + n_frames as f64 * dt) / beat_secs) as f32;

            if t1 < block_start || t0 >= block_end {
                clip.was_active = false;
                continue;
            }
            clip.was_active = true;

            let mut af = n_frames;
            let mut at = 0usize;
            for i in 0..n_frames {
                let t = project_pos_secs + i as f64 * dt;
                let b = (t / beat_secs) as f32;
                if b >= block_start && b < block_end {
                    if af == n_frames { af = i; }
                    at = i + 1;
                }
            }
            if af >= at { continue; }
            let active_len = at - af;

            let clip_len = clip.length_beats;
            let f_in = clip.fade_in_beats;
            let f_out = clip.fade_out_beats;
            let cache_frames = clip.stretched_cache.len() / CHANNELS as usize;
            if cache_frames == 0 { continue; }

            for i in 0..active_len {
                let oi = af + i;
                let t = project_pos_secs + oi as f64 * dt;
                let b = (t / beat_secs) as f32;
                
                let beats_into = b - block_start;
                let wrapped_beats = if clip_len > 0.0 { beats_into % clip_len } else { beats_into };
                let target_sec = (wrapped_beats as f64) * 60.0 / bpm as f64;
                
                let cache_idx = ((target_sec * SAMPLE_RATE as f64) as usize).min(cache_frames.saturating_sub(1));

                let into = b - block_start;
                let from_end = clip_len - into;
                let mut env = 1.0f32;
                if f_in > 0.0 && into < f_in { env = env.min((into / f_in).max(0.0)); }
                if f_out > 0.0 && from_end < f_out { env = env.min((from_end / f_out).max(0.0)); }
                let env = env.clamp(0.0, 1.0);

                mix[oi * 2] += clip.stretched_cache[cache_idx * 2] * gain * env;
                mix[oi * 2 + 1] += clip.stretched_cache[cache_idx * 2 + 1] * gain * env;
            }
        }
    }
}

struct Shared {
    tracks: Vec<TrackStream>,
    project_bpm: f32,
    playing: bool,
    looping: bool,
    loop_start_frac: f64,
    loop_end_frac: f64,
    project_pos_secs: f64,
    project_length_secs: f64,
    mix: Vec<f32>,
    latency_comp_secs: f64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            project_bpm: 120.0,
            playing: false,
            looping: false,
            loop_start_frac: 0.0,
            loop_end_frac: 1.0,
            project_pos_secs: 0.0,
            project_length_secs: 0.0,
            mix: vec![0.0; QUANTUM * 2],
            latency_comp_secs: 0.0,
        }
    }
}

pub struct Player {
    shared: Arc<Mutex<Shared>>,
    _mainloop: ThreadLoop,
    _stream: Stream,
    _listener: StreamListener<()>,
}

impl Player {
    pub fn device_sample_rate(&self) -> u32 { SAMPLE_RATE }
    pub fn output_channels(&self) -> usize { CHANNELS as usize }

    pub fn set_tracks(&self, sources: Vec<TrackSource>) {
        let mut s = self.shared.lock().unwrap();
        let bpm = s.project_bpm;
        
        s.tracks = sources.into_iter().map(|src| {
            let clips: Vec<ClipVoice> = src.clips.iter().map(|spec| {
                let mut voice = ClipVoice::new(*spec);
                let native = if src.native_tempo > 0.0 { src.native_tempo } else { bpm };
                let s_ratio = bpm.max(1.0) as f64 / native as f64;
                
                let mut engine = signalsmith_stretch::Stretch::preset_default(CHANNELS, SAMPLE_RATE);
                engine.set_transpose_factor_semitones(src.pitch_semitones, None);
                
                let src_frames = src.samples.len() / CHANNELS as usize;
                let out_len = (((src_frames as f64) * s_ratio).round() as usize).max(4);
                let mut out_buf = vec![0.0f32; out_len * 2];
                
                if src_frames > 0 {
                    engine.process(&*src.samples, &mut out_buf);
                }
                voice.stretched_cache = out_buf;
                voice
            }).collect();
            
            TrackStream {
                source: src.samples.clone(),
                native_tempo: src.native_tempo,
                pitch_semitones: src.pitch_semitones,
                gain: src.gain,
                muted: src.muted,
                solo: src.solo,
                clips,
            }
        }).collect();

        let mut max_end = 0.0f32;
        for t in &s.tracks {
            for c in &t.clips { max_end = max_end.max(c.start_beats + c.length_beats); }
        }
        s.project_length_secs = (max_end as f64) * 60.0 / bpm.max(1.0) as f64;
    }
}

impl Player {
    pub fn new() -> Result<Self, String> {
        pw::init();
        let mainloop = unsafe { ThreadLoop::new(Some("acid-loop"), None) }.map_err(|e| e.to_string())?;
        let context = Context::new(&mainloop).map_err(|e| e.to_string())?;
        let core = context.connect(None).map_err(|e| e.to_string())?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let shared_cb = shared.clone();

        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::NODE_NAME => "acid-ui",
            *pw::keys::AUDIO_CHANNELS => "2",
        };
        let stream = Stream::new(&core, "acid-ui", props).map_err(|e| e.to_string())?;

        let mut audio_info = AudioInfoRaw::new();
        audio_info.set_format(AudioFormat::F32LE);
        audio_info.set_rate(SAMPLE_RATE);
        audio_info.set_channels(CHANNELS);

        let mut buffer = Vec::with_capacity(1024);
        let mut b = spa::pod::builder::Builder::new(&mut buffer);
        let _ = spa::pod::builder::builder_add!(
            &mut b,
            Object(spa::sys::SPA_TYPE_OBJECT_Format, spa::sys::SPA_PARAM_EnumFormat) {
                spa::sys::SPA_FORMAT_mediaType => Id(spa::utils::Id(spa::sys::SPA_MEDIA_TYPE_audio)),
                spa::sys::SPA_FORMAT_mediaSubtype => Id(spa::utils::Id(spa::sys::SPA_MEDIA_SUBTYPE_raw)),
                spa::sys::SPA_FORMAT_AUDIO_format => Id(spa::utils::Id(spa::sys::SPA_AUDIO_FORMAT_F32_LE)),
                spa::sys::SPA_FORMAT_AUDIO_rate => Int(SAMPLE_RATE as i32),
                spa::sys::SPA_FORMAT_AUDIO_channels => Int(CHANNELS as i32),
            }
        );
        let pod = buffer.clone();

        let listener = stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _ud| { process_cb(stream, &shared_cb); })
            .register()
            .map_err(|e| e.to_string())?;

        let connect_res = {
            let _guard = mainloop.lock();
            let mut params = [Pod::from_bytes(&pod).unwrap()];
            stream.connect(
                spa::utils::Direction::Output,
                None,
                StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
                &mut params,
            )
        };
        connect_res.map_err(|e| e.to_string())?;

        {
            let _guard = mainloop.lock();
            mainloop.start();
            let _ = stream.set_active(true);
        }

        Ok(Self { shared, _mainloop: mainloop, _stream: stream, _listener: listener })
    }

    pub fn set_tempo(&self, bpm: f32) {
        let mut s = self.shared.lock().unwrap();
        s.project_bpm = bpm;
        let old_tracks = std::mem::take(&mut s.tracks);
        s.tracks = old_tracks.into_iter().map(|mut t| {
            let src_samples = t.source.clone();
            let pitch = t.pitch_semitones;
            let native_t = t.native_tempo;
            for c in &mut t.clips {
                let native = if native_t > 0.0 { native_t } else { bpm };
                let s_ratio = bpm.max(1.0) as f64 / native as f64;
                let mut engine = signalsmith_stretch::Stretch::preset_default(CHANNELS, SAMPLE_RATE);
                engine.set_transpose_factor_semitones(pitch, None);
                let src_frames = src_samples.len() / CHANNELS as usize;
                let out_len = (((src_frames as f64) * s_ratio).round() as usize).max(4);
                let mut out_buf = vec![0.0f32; out_len * 2];
                if src_frames > 0 { engine.process(&*src_samples, &mut out_buf); }
                c.stretched_cache = out_buf;
            }
            t
        }).collect();
        let mut max_end = 0.0f32;
        for t in &s.tracks {
            for c in &t.clips { max_end = max_end.max(c.start_beats + c.length_beats); }
        }
        s.project_length_secs = (max_end as f64) * 60.0 / s.project_bpm.max(1.0) as f64;
    }

    pub fn update_track_gain(&self, idx: usize, gain: f32) {
        if let Some(t) = self.shared.lock().unwrap().tracks.get_mut(idx) { t.gain = gain; }
    }
    pub fn update_track_mute(&self, idx: usize, muted: bool) {
        if let Some(t) = self.shared.lock().unwrap().tracks.get_mut(idx) { t.muted = muted; }
    }
    pub fn update_track_solo(&self, idx: usize, solo: bool) {
        if let Some(t) = self.shared.lock().unwrap().tracks.get_mut(idx) { t.solo = solo; }
    }
    pub fn play(&self) {
        let mut s = self.shared.lock().unwrap();
        if s.project_pos_secs >= s.project_length_secs {
            s.project_pos_secs = 0.0;
            for t in &mut s.tracks { t.reset(); }
        }
        s.playing = true;
    }
    pub fn stop(&self) {
        let mut s = self.shared.lock().unwrap();
        s.playing = false;
        s.project_pos_secs = 0.0;
        for t in &mut s.tracks { t.reset(); }
    }
    pub fn is_playing(&self) -> bool { self.shared.lock().unwrap().playing }
    pub fn position_frac(&self) -> f64 {
        let s = self.shared.lock().unwrap();
        if s.project_length_secs <= 0.0 { return 0.0; }
        let heard = (s.project_pos_secs - s.latency_comp_secs).max(0.0);
        (heard / s.project_length_secs).clamp(0.0, 1.0)
    }
    pub fn total_frames(&self) -> usize {
        let s = self.shared.lock().unwrap();
        (s.project_length_secs * SAMPLE_RATE as f64) as usize
    }
    pub fn set_position_frac(&self, frac: f64) {
        let mut s = self.shared.lock().unwrap();
        s.project_pos_secs = frac.clamp(0.0, 1.0) * s.project_length_secs;
        let bpm = s.project_bpm;
        let project_beat = (s.project_pos_secs * bpm as f64 / 60.0) as f32;
        for t in &mut s.tracks { t.seek_to_project_beat(project_beat, bpm); }
    }
    pub fn set_looping(&self, on: bool) { self.shared.lock().unwrap().looping = on; }
    pub fn set_loop_region_frac(&self, start: f64, end: f64) {
        let mut s = self.shared.lock().unwrap();
        s.loop_start_frac = start.clamp(0.0, 1.0);
        s.loop_end_frac = end.clamp(0.0, 1.0);
    }
    pub fn latency_comp_secs(&self) -> f64 { self.shared.lock().unwrap().latency_comp_secs }
    pub fn set_latency_comp_secs(&self, secs: f64) { self.shared.lock().unwrap().latency_comp_secs = secs.clamp(-2.0, 4.0); }
}

fn process_cb(stream: &pw::stream::StreamRef, shared: &Arc<Mutex<Shared>>) {
    let mut s = match shared.try_lock() { Ok(g) => g, Err(_) => return, };
    let mut buffer = match stream.dequeue_buffer() { Some(b) => b, None => return, };
    let datas = buffer.datas_mut();
    if datas.is_empty() { return; }
    let data = &mut datas[0];
    let out: &mut [f32] = match data.data() {
        Some(d) => unsafe { std::slice::from_raw_parts_mut(d.as_mut_ptr() as *mut f32, d.len() / 4) },
        None => return,
    };
    let n_frames = out.len() / CHANNELS as usize;
    if n_frames == 0 { return; }
    let n = n_frames.min(QUANTUM);
    for v in s.mix[..n * 2].iter_mut() { *v = 0.0; }
    if s.playing && !s.tracks.is_empty() {
        let bpm = s.project_bpm;
        let looping = s.looping;
        let loop_start_secs = s.loop_start_frac * s.project_length_secs;
        let loop_end_secs = s.loop_end_frac * s.project_length_secs;
        let use_loop = looping && loop_end_secs > loop_start_secs;
        let mut pos = s.project_pos_secs;
        let any_solo = s.tracks.iter().any(|t| t.solo);
        let mut tracks = std::mem::take(&mut s.tracks);
        let mut written = 0usize;
        while written < n {
            if use_loop && pos >= loop_end_secs {
                pos = loop_start_secs;
                let project_beat = (pos * bpm as f64 / 60.0) as f32;
                for t in tracks.iter_mut() { t.seek_to_project_beat(project_beat, bpm); }
            }
            let remaining = n - written;
            let chunk = if use_loop {
                let secs_to_end = (loop_end_secs - pos).max(0.0);
                let frames_to_end = (secs_to_end * SAMPLE_RATE as f64).ceil() as usize;
                frames_to_end.max(1).min(remaining)
            } else { remaining };
            let slice = &mut s.mix[written * 2..(written + chunk) * 2];
            for t in tracks.iter_mut() {
                if t.muted || (any_solo && !t.solo) { continue; }
                t.produce(slice, chunk, pos, bpm);
            }
            pos += chunk as f64 / SAMPLE_RATE as f64;
            written += chunk;
        }
        s.tracks = tracks;
        s.project_pos_secs = pos;
        if !use_loop && s.project_pos_secs >= s.project_length_secs { s.playing = false; }
    }
    for i in 0..n * 2 {
        let v = s.mix[i];
        let ax = v.abs();
        out[i] = if ax <= 0.90f32 { v } else { (0.90f32 + 0.10f32 * ((ax - 0.90f32) / 0.10f32).tanh()).copysign(v) };
    }
    for i in n * 2..out.len() { out[i] = 0.0; }
    let raw_chunk = data.chunk_mut().as_raw() as *const spa::sys::spa_chunk as *mut spa::sys::spa_chunk;
    unsafe {
        (*raw_chunk).offset = 0;
        (*raw_chunk).size = (n * CHANNELS as usize * 4) as u32;
        (*raw_chunk).stride = (CHANNELS as usize * 4) as i32;
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        { let _guard = self._mainloop.lock(); let _ = self._stream.disconnect(); }
        self._mainloop.stop();
    }
}

use rayon::prelude::*;

impl Player {
    pub fn set_tempo_multicore(&self, bpm: f32) {
        let shared_clone = self.shared.clone();
        let mut snapshot_bpm = 120.0;
        
        #[derive(Clone)]
        struct WorkTrack {
            source_samples: Arc<Vec<f32>>,
            native_tempo: f32,
            pitch_semitones: f32,
            clips: Vec<ClipSpec>,
        }
        
        let mut work_tracks: Vec<WorkTrack> = Vec::new();
        
        if let Ok(mut s) = self.shared.lock() {
            s.project_bpm = bpm;
            snapshot_bpm = bpm;
            
            work_tracks = s.tracks.iter().map(|t| WorkTrack {
                source_samples: t.source.clone(),
                native_tempo: t.native_tempo,
                pitch_semitones: t.pitch_semitones,
                clips: t.clips.iter().map(|c| ClipSpec {
                    start_beats: c.start_beats,
                    length_beats: c.length_beats,
                    source_offset_frames: 0,
                    fade_in_beats: c.fade_in_beats,
                    fade_out_beats: c.fade_out_beats,
                }).collect(),
            }).collect();
        }

        if work_tracks.is_empty() { return; }

        std::thread::spawn(move || {
            let rendered_buffers: Vec<Vec<Vec<f32>>> = work_tracks.par_iter().map(|track| {
                let native = if track.native_tempo > 0.0 { track.native_tempo } else { snapshot_bpm };
                let s_ratio = native as f64 / snapshot_bpm.max(1.0) as f64;
                
                track.clips.iter().map(|_spec| {
                    let mut engine = signalsmith_stretch::Stretch::preset_default(CHANNELS, SAMPLE_RATE);
                    engine.set_transpose_factor_semitones(track.pitch_semitones, None);
                    
                    let src_frames = track.source_samples.len() / CHANNELS as usize;
                    let out_len = (((src_frames as f64) * s_ratio).round() as usize).max(4);
                    let mut out_buf = vec![0.0f32; out_len * 2];
                    
                    if src_frames > 0 {
                        engine.process(&*track.source_samples, &mut out_buf);
                    }
                    out_buf
                }).collect()
            }).collect();

            if let Ok(mut s) = shared_clone.lock() {
                if (s.project_bpm - snapshot_bpm).abs() < 1e-3 {
                    for (track_idx, track_clip_buffers) in rendered_buffers.into_iter().enumerate() {
                        if let Some(t) = s.tracks.get_mut(track_idx) {
                            for (clip_idx, buffer) in track_clip_buffers.into_iter().enumerate() {
                                if let Some(c) = t.clips.get_mut(clip_idx) {
                                    c.stretched_cache = buffer;
                                }
                            }
                        }
                    }
                    let mut max_end = 0.0f32;
                    for t in &s.tracks {
                        for c in &t.clips { max_end = max_end.max(c.start_beats + c.length_beats); }
                    }
                    s.project_length_secs = (max_end as f64) * 60.0 / snapshot_bpm as f64;
                }
            }
        });
    }
}
