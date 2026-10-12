#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridQuantize {
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
    None,
}

impl GridQuantize {
    pub fn divisor(self) -> f32 {
        match self {
            GridQuantize::Quarter => 4.0,
            GridQuantize::Eighth => 8.0,
            GridQuantize::Sixteenth => 16.0,
            GridQuantize::ThirtySecond => 32.0,
            GridQuantize::None => 1.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            GridQuantize::Quarter => "1/4 Note",
            GridQuantize::Eighth => "1/8 Note",
            GridQuantize::Sixteenth => "1/16 Note",
            GridQuantize::ThirtySecond => "1/32 Note",
            GridQuantize::None => "Off",
        }
    }
}
use crate::acid::AcidInfo;
use crate::audio::{decode_wav, resample_linear, DecodedAudio};
use crate::beatmap::{analyze, fit_beats, snap_to_onset, BeatAnalysis};
use crate::mix::Mixer;
use crate::player_pw::{ClipSpec, Player, TrackSource};
use crate::stretch_ss::{SignalsmithEngine, StretchOptions, TransientMode};
use crate::wav::{write_acid_wav_to_file, AcidWavSpec, BitDepth, RiffFile};
use crate::waveform::WaveformCache;
use eframe::egui;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct Clip {
    pub start_beats: f32,
    pub length_beats: f32,
    pub source_offset_frames: usize,
    pub fade_in_beats: f32,
    pub fade_out_beats: f32,
}

pub struct Track {
    pub path: PathBuf,
    pub name: String,
    pub decoded: DecodedAudio,
    pub acid: Option<AcidInfo>,
    pub color: egui::Color32,
    pub gain: f32,
    pub muted: bool,
    pub solo: bool,
    pub detected_tempo: Option<f32>,
    pub beat_positions: Vec<f32>,
    pub pitch_semitones: f32,
    pub playback: Arc<Vec<f32>>,
    pub waveform: Arc<WaveformCache>,
    pub downbeat_secs: f32,
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn native_tempo(&self) -> Option<f32> {
        self.detected_tempo.or_else(|| self.acid.as_ref().map(|a| a.tempo))
    }
    pub fn beats(&self) -> Option<f32> {
        self.acid.as_ref().map(|a| a.beats)
    }
    pub fn stretched_frames(&self, project_bpm: f32) -> usize {
        match self.native_tempo() {
            Some(t) if t > 0.0 => {
                let stretch = (t / project_bpm) as f64;
                ((self.decoded.num_frames() as f64) * stretch).round() as usize
            }
            _ => self.decoded.num_frames(),
        }
    }
    pub fn stretched_seconds(&self, project_bpm: f32) -> f32 {
        self.stretched_frames(project_bpm) as f32 / self.decoded.sample_rate as f32
    }
}

pub struct WizardState {
    pub track_idx: usize,
    pub analysis: BeatAnalysis,
    pub tempo: f32,
    pub first_beat_secs: f32,
    pub bar_beats: u16,
    pub metronome: bool,
    pub original_tempo: Option<f32>,
    pub original_beat_positions: Vec<f32>,
    pub last_change: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StretchQuality {
    PhaseVocoder,
    Signalsmith,
}

impl StretchQuality {
    pub fn label(self) -> &'static str {
        match self {
            StretchQuality::PhaseVocoder => "phase vocoder",
            StretchQuality::Signalsmith => "signalsmith",
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct RenderSettings {
    pub export_rate: u32, // 0 = device rate
    pub export_bit_depth: BitDepth,
    pub quality: StretchQuality,
    pub transient: TransientMode,
    pub formant_semitones: f32,
    pub preserve_formants: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            export_rate: 0,
            export_bit_depth: BitDepth::Int16,
            quality: StretchQuality::Signalsmith,
            transient: TransientMode::Balanced,
            formant_semitones: 0.0,
            preserve_formants: false,
        }
    }
}

#[allow(dead_code)]
pub struct AcidApp {
    project_bpm: f32,
    looping: bool,
    loop_start_beats: f32,
    loop_end_beats: f32,
    auto_loop_region: bool,
    tracks: Vec<Track>,
    player: Option<Player>,
    player_error: Option<String>,
    new_path: String,
    export_path: String,
    status: String,
    pixels_per_second: f32,
    fit_to_window: bool,
    next_color: usize,
    picker_rx: Option<Receiver<Result<Vec<PathBuf>, String>>>,
    beatmap_rx: Option<Receiver<(usize, Option<BeatAnalysis>)>>,
    beatmap_in_progress: Option<usize>,
    wizard: Option<WizardState>,
    render_settings: RenderSettings,
    latency_comp_ms: f32,
    show_settings: bool,
    pick_downbeat_for: Option<usize>,
    selected_track: Option<usize>,
    selected_clip: Option<usize>,
    clip_drag: Option<(usize, usize, f32)>,
    loop_drag: Option<&'static str>,
    grid_quantize: GridQuantize,
}

const PALETTE: &[egui::Color32] = &[
    egui::Color32::from_rgb(96, 148, 220),
    egui::Color32::from_rgb(220, 148, 96),
    egui::Color32::from_rgb(148, 220, 96),
    egui::Color32::from_rgb(220, 96, 148),
    egui::Color32::from_rgb(148, 96, 220),
    egui::Color32::from_rgb(96, 220, 148),
    egui::Color32::from_rgb(220, 220, 96),
];

impl AcidApp {
    pub fn new() -> Self {
        let (player, player_error) = match Player::new() {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e)),
        };

        let mut app = Self {
            project_bpm: 120.0,
            looping: false,
            loop_start_beats: 0.0,
            loop_end_beats: 0.0,
            auto_loop_region: true,
            tracks: Vec::new(),
            player,
            player_error,
            new_path: String::new(),
            export_path: "/tmp/acid-export.wav".to_string(),
            status: "loaded demo loops. press play.".to_string(),
            pixels_per_second: 80.0,
            fit_to_window: false,
            next_color: 0,
            picker_rx: None,
            beatmap_rx: None,
            beatmap_in_progress: None,
            wizard: None,
            render_settings: RenderSettings::default(),
            latency_comp_ms: 0.0,
            show_settings: false,
            pick_downbeat_for: None,
            selected_track: None,
            selected_clip: None,
            clip_drag: None,
            loop_drag: None,
            grid_quantize: GridQuantize::Quarter,
        };

        let mut loaded_any = false;
        for p in ["/tmp/loop_a.wav", "/tmp/loop_b.wav"] {
            let path = PathBuf::from(p);
            if path.exists() && app.load_track_inner(&path).is_ok() {
                loaded_any = true;
            }
        }
        if !loaded_any {
            app.load_demo_tracks();
        }
        if let Some(p) = &app.player {
            p.set_latency_comp_secs((app.latency_comp_ms / 1000.0) as f64);
        }

        if !app.tracks.is_empty() {
            let beats = app.project_beats();
            app.loop_start_beats = 0.0;
            app.loop_end_beats = beats;
            app.push_state_to_player();
        }

        app
    }

    fn project_seconds(&self) -> f32 {
        self.tracks
            .iter()
            .map(|t| t.stretched_seconds(self.project_bpm))
            .fold(0.0f32, f32::max)
    }
    fn project_beats(&self) -> f32 {
        self.project_seconds() * self.project_bpm / 60.0
    }
    fn beats_to_seconds(&self, beats: f32) -> f32 {
        beats * 60.0 / self.project_bpm
    }
    fn loop_region_fractions(&self) -> (f64, f64) {
        let total = self.project_beats().max(0.0001) as f64;
        let s = (self.loop_start_beats as f64 / total).clamp(0.0, 1.0);
        let e = (self.loop_end_beats as f64 / total).clamp(0.0, 1.0);
        (s.min(e), e.max(s))
    }

    fn default_clip_beats(
        decoded: &DecodedAudio,
        native_tempo: Option<f32>,
        project_bpm: f32,
    ) -> f32 {
        let tempo = match native_tempo {
            Some(t) if t > 0.0 => t,
            _ => project_bpm.max(1.0),
        };
        let secs = decoded.duration_secs() as f32;
        (secs * tempo / 60.0).max(1.0)
    }

    fn device_rate(&self) -> u32 {
        self.player
            .as_ref()
            .map(|p| p.device_sample_rate())
            .unwrap_or(44100)
    }

    fn make_playback(decoded: &DecodedAudio, device_rate: u32) -> Arc<Vec<f32>> {
        if decoded.sample_rate == device_rate {
            Arc::new(decoded.samples.clone())
        } else {
            Arc::new(resample_linear(&decoded.samples, decoded.sample_rate, device_rate))
        }
    }

    fn load_track_inner(&mut self, path: &PathBuf) -> Result<(), String> {
        if self.tracks.iter().any(|t| t.path == *path) {
            return Err(format!("already loaded: {}", path.display()));
        }
        let bytes = fs::read(path).map_err(|e| format!("read: {}", e))?;
        let decoded = decode_wav(&bytes).map_err(|e| format!("decode: {}", e))?;
        let acid = RiffFile::parse(&bytes)
            .ok()
            .and_then(|riff| AcidInfo::from_riff(&riff).ok());
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let color = PALETTE[self.next_color % PALETTE.len()];
        self.next_color += 1;
        let playback = Self::make_playback(&decoded, self.device_rate());
        let waveform = Arc::new(WaveformCache::build(&decoded.samples, 256));
        let native = acid.as_ref().map(|a| a.tempo);
        let length_beats =
            Self::default_clip_beats(&decoded, native, self.project_bpm);
        self.tracks.push(Track {
            path: path.clone(),
            name,
            decoded,
            acid,
            color,
            gain: 1.0,
            muted: false,
            solo: false,
            detected_tempo: None,
            beat_positions: Vec::new(),
            pitch_semitones: 0.0,
            playback,
            waveform,
            downbeat_secs: 0.0,
            clips: vec![Clip {
                start_beats: 0.0,
                length_beats,
                source_offset_frames: 0,
                fade_in_beats: 0.0,
                fade_out_beats: 0.0,
            }],
        });
        Ok(())
    }

    fn load_track(&mut self, path: &PathBuf) -> Result<(), String> {
        let r = self.load_track_inner(path);
        if r.is_ok() {
            self.push_state_to_player();
        }
        r
    }

    fn load_tracks_batch(&mut self, paths: &[PathBuf]) -> (usize, Option<String>) {
        let mut loaded = 0usize;
        let mut last_err: Option<String> = None;
        for p in paths {
            match self.load_track_inner(p) {
                Ok(()) => loaded += 1,
                Err(e) => last_err = Some(e),
            }
        }
        if loaded > 0 {
            self.push_state_to_player();
        }
        (loaded, last_err)
    }

    fn load_demo_tracks(&mut self) {
        use crate::gen::build_click_loop;
        let demos: &[(&str, f32, f32, f32)] = &[
            ("demo A (128 BPM, 440 Hz)", 128.0, 8.0, 440.0),
            ("demo B (90 BPM, 660 Hz)", 90.0, 8.0, 660.0),
        ];
        for (name, bpm, beats, hz) in demos {
            let bytes = build_click_loop(*beats, *bpm, *hz, 44100, 2);
            let decoded = match decode_wav(&bytes) {
                Ok(d) => d,
                Err(_) => continue,
            };
            let acid = RiffFile::parse(&bytes)
                .ok()
                .and_then(|r| AcidInfo::from_riff(&r).ok());
            let color = PALETTE[self.next_color % PALETTE.len()];
            self.next_color += 1;
            let playback = Self::make_playback(&decoded, self.device_rate());
            let waveform = Arc::new(WaveformCache::build(&decoded.samples, 256));
            let native = acid.as_ref().map(|a| a.tempo);
            let length_beats =
                Self::default_clip_beats(&decoded, native, self.project_bpm);
            self.tracks.push(Track {
                path: PathBuf::from(format!("<in-memory:{}>", name)),
                name: name.to_string(),
                decoded,
                acid,
                color,
                gain: 1.0,
                muted: false,
                solo: false,
                detected_tempo: None,
                beat_positions: Vec::new(),
                pitch_semitones: 0.0,
                playback,
                waveform,
                downbeat_secs: 0.0,
                clips: vec![Clip {
                    start_beats: 0.0,
                    length_beats,
                    source_offset_frames: 0,
                    fade_in_beats: 0.0,
                    fade_out_beats: 0.0,
                }],
            });
        }
        let total_beats = self.project_beats();
        self.loop_start_beats = 0.0;
        self.loop_end_beats = total_beats;
    }

    fn push_state_to_player(&mut self) {
        let player = match &self.player {
            Some(p) => p,
            None => return,
        };
        let mut sources: Vec<TrackSource> = Vec::new();
        if let Some(w) = &self.wizard {
            if let Some(t) = self.tracks.get(w.track_idx) {
                sources.push(TrackSource {
                    samples: t.playback.clone(),
                    native_tempo: w.tempo,
                    pitch_semitones: t.pitch_semitones,
                    gain: t.gain,
                    muted: false,
                    solo: false,
                    clips: t
                        .clips
                        .iter()
                        .map(|c| ClipSpec {
                            start_beats: c.start_beats,
                            length_beats: c.length_beats,
                            source_offset_frames: c.source_offset_frames,
                            fade_in_beats: c.fade_in_beats,
                            fade_out_beats: c.fade_out_beats,
                        })
                        .collect(),
                });
            }
        } else {
            for t in &self.tracks {
                sources.push(TrackSource {
                    samples: t.playback.clone(),
                    native_tempo: t.native_tempo().unwrap_or(0.0),
                    pitch_semitones: t.pitch_semitones,
                    gain: t.gain,
                    muted: t.muted,
                    solo: t.solo,
                    clips: t
                        .clips
                        .iter()
                        .map(|c| ClipSpec {
                            start_beats: c.start_beats,
                            length_beats: c.length_beats,
                            source_offset_frames: c.source_offset_frames,
                            fade_in_beats: c.fade_in_beats,
                            fade_out_beats: c.fade_out_beats,
                        })
                        .collect(),
                });
            }
        }
        player.set_tracks(sources);
        player.set_tempo_multicore(self.project_bpm);
        player.set_looping(self.looping && self.wizard.is_none());
        let (s, e) = self.loop_region_fractions();
        player.set_loop_region_frac(s, e);
    }

    fn push_loop_to_player(&self) {
        if let Some(p) = &self.player {
            p.set_looping(self.looping && self.wizard.is_none());
            let (s, e) = self.loop_region_fractions();
            p.set_loop_region_frac(s, e);
        }
    }

    fn start_playback(&mut self) {
        if let Some(p) = &self.player {
            p.set_tempo_multicore(self.project_bpm);
            self.push_loop_to_player();
            p.play();
            self.status = "playing".to_string();
        }
    }

    fn stop_playback(&mut self) {
        if let Some(p) = &self.player {
            p.stop();
        }
        self.status = "stopped".to_string();
    }

    fn poll_picker(&mut self) {
        let rx = match self.picker_rx.take() {
            Some(rx) => rx,
            None => return,
        };
        match rx.try_recv() {
            Ok(Ok(paths)) => {
                if paths.is_empty() {
                    self.status = "picker cancelled".to_string();
                } else {
                    let (loaded, err) = self.load_tracks_batch(&paths);
                    self.status = match err {
                        Some(e) => format!("loaded {} ({})", loaded, e),
                        None => format!("loaded {} file(s)", loaded),
                    };
                }
            }
            Ok(Err(e)) => self.status = e,
            Err(mpsc::TryRecvError::Empty) => self.picker_rx = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    fn launch_file_picker(&mut self) {
        if self.picker_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.picker_rx = Some(rx);
        self.status = "waiting for file picker...".to_string();
        std::thread::spawn(move || {
            let _ = tx.send(pick_files_blocking());
        });
    }

    fn launch_beatmap_wizard(&mut self, track_idx: usize) {
        if self.beatmap_rx.is_some() {
            self.status = "analysis already running".to_string();
            return;
        }
        if self.wizard.is_some() {
            self.status = "wizard already open".to_string();
            return;
        }
        let t = match self.tracks.get(track_idx) {
            Some(t) => t,
            None => return,
        };
        let samples = t.decoded.samples.clone();
        let rate = t.decoded.sample_rate;
        self.status = format!("analyzing {}...", t.name);
        self.beatmap_in_progress = Some(track_idx);
        let (tx, rx) = mpsc::channel();
        self.beatmap_rx = Some(rx);
        std::thread::spawn(move || {
            let result = analyze(&samples, rate);
            let _ = tx.send((track_idx, result));
        });
    }

    fn poll_beatmap(&mut self) {
        let rx = match self.beatmap_rx.take() {
            Some(rx) => rx,
            None => return,
        };
        match rx.try_recv() {
            Ok((idx, Some(a))) => {
                self.beatmap_in_progress = None;
                self.open_wizard(idx, a);
            }
            Ok((_, None)) => {
                self.beatmap_in_progress = None;
                self.status = "beatmap failed".to_string();
            }
            Err(mpsc::TryRecvError::Empty) => self.beatmap_rx = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => self.beatmap_in_progress = None,
        }
    }

    fn open_wizard(&mut self, idx: usize, analysis: BeatAnalysis) {
        let (orig_tempo, orig_positions, name) = match self.tracks.get(idx) {
            Some(t) => (t.detected_tempo, t.beat_positions.clone(), t.name.clone()),
            None => return,
        };
        self.wizard = Some(WizardState {
            track_idx: idx,
            tempo: analysis.tempo,
            first_beat_secs: analysis.first_beat_secs,
            bar_beats: 4,
            metronome: true,
            original_tempo: orig_tempo,
            original_beat_positions: orig_positions,
            last_change: None,
            analysis,
        });
        self.status = format!("wizard open: {}", name);
        self.apply_wizard_to_track();
        self.push_state_to_player();
    }

    fn apply_wizard_to_track(&mut self) {
        let (idx, tempo, first_beat, total) = match &self.wizard {
            Some(w) => (w.track_idx, w.tempo, w.first_beat_secs, w.analysis.total_secs),
            None => return,
        };
        let positions = fit_beats(first_beat, tempo, total);
        if let Some(t) = self.tracks.get_mut(idx) {
            t.detected_tempo = Some(tempo);
            t.beat_positions = positions;
        }
    }

    fn accept_wizard(&mut self) {
        if let Some(w) = self.wizard.take() {
            if let Some(t) = self.tracks.get_mut(w.track_idx) {
                t.downbeat_secs = w.first_beat_secs;
            }
        }
        self.status = "wizard applied".to_string();
        self.push_state_to_player();
    }

    fn cancel_wizard(&mut self) {
        if let Some(w) = self.wizard.take() {
            if let Some(t) = self.tracks.get_mut(w.track_idx) {
                t.detected_tempo = w.original_tempo;
                t.beat_positions = w.original_beat_positions;
            }
            self.status = "wizard cancelled".to_string();
        }
        self.push_state_to_player();
    }

    fn render_mixdown_to_file(&self, path: &Path) -> Result<String, String> {
        if self.tracks.is_empty() {
            return Err("no tracks".to_string());
        }
        let any_solo = self.tracks.iter().any(|t| t.solo);
        let target_rate = if self.render_settings.export_rate == 0 {
            self.device_rate()
        } else {
            self.render_settings.export_rate
        };

        let mut mixer: Option<Mixer> = None;
        for t in &self.tracks {
            if t.muted {
                continue;
            }
            if any_solo && !t.solo {
                continue;
            }
            let ratio = match t.native_tempo() {
                Some(nt) if nt > 0.0 => (nt / self.project_bpm) as f64,
                _ => 1.0,
            };
            let needs_stretch =
                (ratio - 1.0).abs() > 1e-6 || t.pitch_semitones.abs() > 1e-3;
            let stretched = if !needs_stretch {
                t.decoded.samples.clone()
            } else {
                let mut eng = SignalsmithEngine::new(2, t.decoded.sample_rate);
                let opts = StretchOptions {
                    pitch_semitones: t.pitch_semitones,
                    formant_semitones: self.render_settings.formant_semitones,
                    preserve_formants: self.render_settings.preserve_formants,
                    transient: self.render_settings.transient,
                };
                eng.stretch(&t.decoded.samples, ratio, &opts)
            };
            let resampled = if t.decoded.sample_rate != target_rate {
                resample_linear(&stretched, t.decoded.sample_rate, target_rate)
            } else {
                stretched
            };
            match &mut mixer {
                None => {
                    let frames = resampled.len() / 2;
                    let mut m = Mixer::new(target_rate, frames);
                    m.add_at(&resampled, 0, t.gain);
                    mixer = Some(m);
                }
                Some(m) => {
                    m.add_at(&resampled, 0, t.gain);
                }
            }
        }
        let mut mixer = mixer.ok_or("all tracks muted or not selected")?;
        mixer.normalize_to(0.95);
        let secs = mixer.len_frames() as f32 / mixer.sample_rate as f32;
        let beats = secs * self.project_bpm / 60.0;
        let spec = AcidWavSpec {
            sample_rate: mixer.sample_rate,
            channels: 2,
            tempo: self.project_bpm,
            beats,
            meter_num: 4,
            meter_den: 4,
            root_note: 60.0,
            bit_depth: self.render_settings.export_bit_depth,
        };
        write_acid_wav_to_file(path, &mixer.samples, &spec)
            .map_err(|e| format!("write: {}", e))?;
        Ok(format!(
            "{:.2}s, {:.2} beats at {:.1} BPM, {} Hz {}",
            secs,
            beats,
            self.project_bpm,
            target_rate,
            self.render_settings.export_bit_depth.label()
        ))
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = dropped.into_iter().filter_map(|f| f.path).collect();
        if paths.is_empty() {
            return;
        }
        let (loaded, last_err) = self.load_tracks_batch(&paths);
        self.status = if let Some(err) = last_err {
            format!("loaded {} ({})", loaded, err)
        } else {
            format!("loaded {} file(s)", loaded)
        };
    }
}

impl eframe::App for AcidApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_dropped_files(ctx);
        self.poll_picker();
        self.poll_beatmap();

        let needs_repaint = self
            .player
            .as_ref()
            .map(|p| p.is_playing())
            .unwrap_or(false)
            || self.picker_rx.is_some()
            || self.beatmap_rx.is_some()
            || self.wizard.is_some();
        if needs_repaint {
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("transport").show(ctx, |ui| {
            // Row 1
            ui.horizontal(|ui| {
                ui.heading("acid-ui");
                ui.separator();
                let playing = self
                    .player
                    .as_ref()
                    .map(|p| p.is_playing())
                    .unwrap_or(false);
                if playing {
                    if ui.button("stop").clicked() {
                        self.stop_playback();
                    }
                } else if ui.button("play").clicked() {
                    self.start_playback();
                }
                ui.separator();
                ui.label("tempo:");
                let prev = self.project_bpm;
                ui.add(
                    egui::Slider::new(&mut self.project_bpm, 40.0..=220.0)
                        .suffix(" BPM")
                        .fixed_decimals(1),
                );
                if (self.project_bpm - prev).abs() > 1e-6 {
                    if self.auto_loop_region {
                        self.loop_start_beats = 0.0;
                        self.loop_end_beats = self.project_beats();
                    }
                    self.push_loop_to_player();
                    if let Some(p) = &self.player {
                        p.set_tempo_multicore(self.project_bpm);
                    }
                }
                ui.separator();
                ui.label("zoom:");
                if ui
                    .add(
                        egui::Slider::new(&mut self.pixels_per_second, 2.0..=2000.0)
                            .logarithmic(true)
                            .show_value(false),
                    )
                    .changed()
                {
                    self.fit_to_window = false;
                }
                let fit_resp = ui.selectable_label(self.fit_to_window, "fit");
                if fit_resp.clicked() {
                    self.fit_to_window = !self.fit_to_window;
                }
                if ui.small_button("z-").clicked() {
                    self.fit_to_window = false;
                    self.pixels_per_second = (self.pixels_per_second / 1.5).max(2.0);
                }
                if ui.small_button("z+").clicked() {
                    self.fit_to_window = false;
                    self.pixels_per_second = (self.pixels_per_second * 1.5).min(2000.0);
                }
                if ui.small_button("settings").clicked() {
                    self.show_settings = !self.show_settings;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(err) = &self.player_error {
                        ui.colored_label(egui::Color32::RED, err);
                    } else if let Some(p) = &self.player {
                        let export_rate = if self.render_settings.export_rate == 0 {
                            p.device_sample_rate()
                        } else {
                            self.render_settings.export_rate
                        };
                        ui.label(format!(
                            "{} Hz / {} ch  ·  export {} Hz {}",
                            p.device_sample_rate(),
                            p.output_channels(),
                            export_rate,
                            self.render_settings.export_bit_depth.label()
                        ));
                    }
                });
            });

            // Row 2: loop
            ui.horizontal(|ui| {
                let mut looping = self.looping;
                if ui.checkbox(&mut looping, "loop").changed() {
                    self.looping = looping;
                    self.push_loop_to_player();
                }
                ui.separator();
                ui.separator();
                ui.label("grid snap:");
                for option in [GridQuantize::Quarter, GridQuantize::Eighth, GridQuantize::Sixteenth, GridQuantize::ThirtySecond, GridQuantize::None] {
                    if ui.selectable_label(self.grid_quantize == option, option.label()).clicked() {
                        self.grid_quantize = option;
                    }
                }
                ui.separator();
                ui.label("region:");
                let total_beats = self.project_beats().max(0.25);
                let mut start = self.loop_start_beats;
                let mut end = self.loop_end_beats;
                let sr = ui.add(
                    egui::DragValue::new(&mut start)
                        .speed(0.25)
                        .range(0.0..=total_beats)
                        .suffix("b"),
                );
                ui.label("->");
                let er = ui.add(
                    egui::DragValue::new(&mut end)
                        .speed(0.25)
                        .range(0.0..=total_beats)
                        .suffix("b"),
                );
                if sr.changed() || er.changed() {
                    let s_snap = (start * 4.0).round() / 4.0;
                    let e_snap = (end * 4.0).round() / 4.0;
                    self.loop_start_beats = s_snap.min(e_snap);
                    self.loop_end_beats =
                        (e_snap.max(s_snap + 0.25)).min(total_beats);
                    self.auto_loop_region = false;
                    self.push_loop_to_player();
                }
                ui.separator();
                for (label, beats) in
                    [("1 bar", 4.0_f32), ("2 bars", 8.0), ("4 bars", 16.0)]
                {
                    if ui.small_button(label).clicked() {
                        self.auto_loop_region = false;
                        // Extend from current loop_start, not from zero.
                        let start = self.loop_start_beats;
                        self.loop_end_beats = (start + beats).min(total_beats);
                        self.looping = true;
                        self.push_loop_to_player();
                    }
                }
                if ui.small_button("whole").clicked() {
                    self.auto_loop_region = true;
                    self.loop_start_beats = 0.0;
                    self.loop_end_beats = total_beats;
                    self.push_loop_to_player();
                }
                ui.separator();
                let rb = self.loop_end_beats - self.loop_start_beats;
                let rs = self.beats_to_seconds(rb);
                ui.label(format!("({:.2}b = {:.2}s)", rb, rs));
            });

            // Row 3: files
            ui.horizontal(|ui| {
                ui.label("path:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_path)
                        .desired_width(240.0)
                        .hint_text("/path/to/loop.wav"),
                );
                let busy = self.picker_rx.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new("browse..."))
                    .clicked()
                {
                    self.launch_file_picker();
                }
                if ui.button("+ add").clicked() {
                    let p = PathBuf::from(self.new_path.trim());
                    if p.exists() {
                        match self.load_track(&p) {
                            Ok(()) => {
                                self.status = format!("loaded {}", p.display());
                                self.new_path.clear();
                            }
                            Err(e) => self.status = format!("error: {}", e),
                        }
                    } else {
                        self.status = format!("not found: {}", p.display());
                    }
                }
                if ui.button("demos").clicked() {
                    self.load_demo_tracks();
                    self.push_state_to_player();
                    self.status = "loaded demos".to_string();
                }
                if ui.button("clear").clicked() {
                    self.tracks.clear();
                    self.push_state_to_player();
                    self.status = "cleared".to_string();
                }
                ui.separator();
                ui.label("export:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.export_path)
                        .desired_width(200.0),
                );
                if ui.button("save").clicked() {
                    let p = PathBuf::from(self.export_path.trim());
                    match self.render_mixdown_to_file(&p) {
                        Ok(info) => {
                            self.status = format!("wrote {} ({})", p.display(), info)
                        }
                        Err(e) => self.status = format!("export error: {}", e),
                    }
                }
                ui.separator();
                ui.label(&self.status);
            });
        });

        // Track list
        egui::SidePanel::left("tracks")
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.heading("tracks");
                ui.small("drag audio files onto the window to add them");
                ui.separator();

                let mut remove_idx: Option<usize> = None;
                let mut beatmap_idx: Option<usize> = None;
                let mut pick_downbeat_idx: Option<usize> = None;
                let mut solo_changed = false;

                for (i, t) in self.tracks.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(12.0, 12.0),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(rect, 2.0, t.color);
                        ui.label(&t.name);
                    });
                    ui.horizontal(|ui| {
                        let mut muted = t.muted;
                        if ui.checkbox(&mut muted, "M").changed() {
                            t.muted = muted;
                            if let Some(p) = &self.player {
                                p.update_track_mute(i, muted);
                            }
                        }
                        let mut solo = t.solo;
                        if ui.checkbox(&mut solo, "S").changed() {
                            t.solo = solo;
                            if let Some(p) = &self.player {
                                p.update_track_solo(i, solo);
                            }
                            solo_changed = true;
                        }
                        let mut gain = t.gain;
                        if ui
                            .add(
                                egui::DragValue::new(&mut gain)
                                    .speed(0.01)
                                    .range(0.0..=2.0)
                                    .prefix("g "),
                            )
                            .changed()
                        {
                            t.gain = gain;
                            if let Some(p) = &self.player {
                                p.update_track_gain(i, gain);
                            }
                        }
                        let mut pitch = t.pitch_semitones;
                        if ui
                            .add(
                                egui::DragValue::new(&mut pitch)
                                    .speed(0.05)
                                    .range(-24.0..=24.0)
                                    .suffix(" st"),
                            )
                            .changed()
                        {
                            t.pitch_semitones = pitch;
                        }
                        let picking = self.pick_downbeat_for == Some(i);
                        if ui
                            .add_enabled(
                                !picking,
                                egui::Button::new("set beat").small(),
                            )
                            .clicked()
                        {
                            pick_downbeat_idx = Some(i);
                        }
                        let busy = self.beatmap_in_progress == Some(i);
                        if ui
                            .add_enabled(!busy, egui::Button::new("beatmap").small())
                            .clicked()
                        {
                            beatmap_idx = Some(i);
                        }
                        if ui.small_button("x").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                    let detected = t.detected_tempo.is_some();
                    let detail = match (t.native_tempo(), t.beats()) {
                        (Some(bpm), Some(beats)) => format!(
                            "{:.2} BPM{}, {} beats, {:.2}s",
                            bpm,
                            if detected { " (detected)" } else { "" },
                            beats,
                            t.decoded.duration_secs()
                        ),
                        (Some(bpm), None) => format!(
                            "{:.2} BPM{}, {:.2}s",
                            bpm,
                            if detected { " (detected)" } else { "" },
                            t.decoded.duration_secs()
                        ),
                        _ => format!(
                            "{:.2}s (no tempo, try beatmap)",
                            t.decoded.duration_secs()
                        ),
                    };
                    ui.small(detail);
                    ui.separator();
                }

                if solo_changed {
                    self.push_state_to_player();
                }
                if let Some(i) = remove_idx {
                    self.tracks.remove(i);
                    self.push_state_to_player();
                }
                if let Some(i) = beatmap_idx {
                    self.launch_beatmap_wizard(i);
                }
                if let Some(i) = pick_downbeat_idx {
                    self.pick_downbeat_for = Some(i);
                    let name = self
                        .tracks
                        .get(i)
                        .map(|t| t.name.clone())
                        .unwrap_or_default();
                    self.status =
                        format!("click on the timeline to set downbeat of {}", name);
                }
                if self.tracks.is_empty() {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        "no tracks. drop a file, add a path, or click demos.",
                    );
                }
            });

        // Timeline
        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_size();
            let (rect, response) =
                ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, egui::Color32::from_gray(28));

            if self.tracks.is_empty() {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "drop audio files here",
                    egui::FontId::proportional(16.0),
                    egui::Color32::GRAY,
                );
                return;
            }

            let label_w = 150.0;
            let lane_h = 54.0;
            let lane_gap = 8.0;
            let header_h = 24.0;
            let total_secs = self.project_seconds().max(0.001);
            let usable_w = (rect.width() - label_w).max(50.0);
            let px_per_sec = if self.fit_to_window {
                usable_w / total_secs
            } else {
                self.pixels_per_second
            };
            let beat_secs = 60.0 / self.project_bpm;
            let bar_secs = beat_secs * 4.0;
            let max_secs = self
                .tracks
                .iter()
                .map(|t| t.stretched_seconds(self.project_bpm))
                .fold(0.0f32, f32::max);
            let show_beats = beat_secs * px_per_sec >= 8.0;
            let show_bars = bar_secs * px_per_sec >= 4.0;
            let show_bar_labels = bar_secs * px_per_sec >= 40.0;
            let show_sec_labels = px_per_sec < 8.0;

            // ---- Loop handle drag ----
            {
                let ls_x = rect.left()
                    + label_w
                    + self.beats_to_seconds(self.loop_start_beats) * px_per_sec;
                let le_x = rect.left()
                    + label_w
                    + self.beats_to_seconds(self.loop_end_beats) * px_per_sec;
                const GRAB: f32 = 8.0;
                if let Some(ptr) = response.interact_pointer_pos() {
                    if response.drag_started() && self.loop_drag.is_none() {
                        let ds = (ptr.x - ls_x).abs();
                        let de = (ptr.x - le_x).abs();
                        if ds < GRAB && ds <= de {
                            self.loop_drag = Some("start");
                        } else if de < GRAB {
                            self.loop_drag = Some("end");
                        }
                    }
                    if response.dragged() && self.loop_drag.is_some() {
                        let secs = (ptr.x - rect.left() - label_w) / px_per_sec;
                        let raw = secs * self.project_bpm / 60.0;
                        let snapped = (raw * 4.0).round() / 4.0;
                        let total_beats = self.project_beats().max(0.25);
                        match self.loop_drag {
                            Some("start") => {
                                self.loop_start_beats = snapped.clamp(
                                    0.0,
                                    (self.loop_end_beats - 0.25).max(0.0),
                                );
                                self.auto_loop_region = false;
                            }
                            Some("end") => {
                                self.loop_end_beats = snapped.clamp(
                                    self.loop_start_beats + 0.25,
                                    total_beats,
                                );
                                self.auto_loop_region = false;
                            }
                            _ => {}
                        }
                        self.push_loop_to_player();
                    }
                    if response.drag_stopped() {
                        self.loop_drag = None;
                    }
                }
            }

            if self.loop_drag.is_none()
                && (response.clicked() || response.dragged())
                && response
                    .interact_pointer_pos()
                    .map(|p| p.x >= rect.left() + label_w)
                    .unwrap_or(false)
            {
                if let Some(pos) = response.interact_pointer_pos() {
                    let proj_secs = (pos.x - rect.left() - label_w) / px_per_sec;
                    
                    if let Some(idx) = self.pick_downbeat_for {
                        // Handled by downbeat shortcut picker
                        if let Some(t) = self.tracks.get_mut(idx) {
                            let stretch = match t.native_tempo() { Some(nt) if nt > 0.0 => nt / self.project_bpm, _ => 1.0 };
                            t.downbeat_secs = (proj_secs / stretch).clamp(0.0, t.decoded.duration_secs() as f32);
                        }
                        self.pick_downbeat_for = None;
                    } else {
                        // ACID Pro Loop Brush Painting logic:
                        // Calculate which track lane the mouse is currently hovering over
                        let click_y = pos.y - (rect.top() + header_h);
                        let lane_index = (click_y / (lane_h + lane_gap)) as usize;
                        
                        if lane_index < self.tracks.len() {
                            let click_beat = proj_secs * self.project_bpm / 60.0;
                            let div = self.grid_quantize.divisor();
                            let snapped_beat = if self.grid_quantize == GridQuantize::None { click_beat } else { (click_beat * div).floor() / div };
                            
                            let t = &mut self.tracks[lane_index];
                            self.selected_track = Some(lane_index);
                            
                            // Check if a clip already occupies this beat area, otherwise paint a 4-beat (1 bar) block
                            let duplicate = t.clips.iter().any(|c| snapped_beat >= c.start_beats && snapped_beat < c.start_beats + c.length_beats);
                            if !duplicate {
                                let native_t = t.native_tempo();
                                let default_len = Self::default_clip_beats(&t.decoded, native_t, self.project_bpm);
                                t.clips.push(Clip {
                                    start_beats: snapped_beat,
                                    length_beats: default_len.min(4.0), // Paint 1 bar blocks at a time
                                    source_offset_frames: 0,
                                    fade_in_beats: 0.0,
                                    fade_out_beats: 0.0,
                                });
                                self.status = format!("Painted loop block on track {} at beat {}", t.name, snapped_beat);
                                self.push_state_to_player();
                            }
                        } else {
                            // Clicked blank lower canvas space, update regular playhead placement position fraction
                            let frac = (proj_secs / total_secs) as f64;
                            if let Some(p) = &self.player {
                                p.set_position_frac(frac.clamp(0.0, 1.0));
                            }
                        }
                    }
                }
            }

            let n_bars = (max_secs / bar_secs).ceil() as i32 + 1;
            for b in 0..=n_bars {
                let x = rect.left() + label_w + (b as f32 * bar_secs) * px_per_sec;
                if x > rect.right() {
                    break;
                }
                if show_bars {
                    painter.line_segment(
                        [
                            egui::pos2(x, rect.top() + header_h),
                            egui::pos2(x, rect.bottom()),
                        ],
                        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(70)),
                    );
                }
                if show_beats {
                    for k in 1..4 {
                        let bx = x + (k as f32 * beat_secs) * px_per_sec;
                        if bx > rect.right() {
                            break;
                        }
                        painter.line_segment(
                            [
                                egui::pos2(bx, rect.top() + header_h),
                                egui::pos2(bx, rect.bottom()),
                            ],
                            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(45)),
                        );
                    }
                }
            }

            let loop_x0 = rect.left()
                + label_w
                + self.beats_to_seconds(self.loop_start_beats) * px_per_sec;
            let loop_x1 = rect.left()
                + label_w
                + self.beats_to_seconds(self.loop_end_beats) * px_per_sec;
            if self.loop_end_beats > self.loop_start_beats {
                let loop_rect = egui::Rect::from_min_max(
                    egui::pos2(loop_x0.max(rect.left() + label_w), rect.top() + header_h),
                    egui::pos2(loop_x1.min(rect.right()), rect.bottom()),
                );
                if loop_rect.width() > 0.0 {
                    painter.rect_filled(
                        loop_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(120, 200, 120, 22),
                    );
                    let active = self.loop_drag;
                    for (x, which) in
                        [(loop_rect.left(), "start"), (loop_rect.right(), "end")]
                    {
                        let hover = response
                            .hover_pos()
                            .map(|p| (p.x - x).abs() < 8.0)
                            .unwrap_or(false);
                        let highlight = active == Some(which) || hover;
                        let color = if highlight {
                            egui::Color32::from_rgb(180, 255, 180)
                        } else {
                            egui::Color32::from_rgb(120, 200, 120)
                        };
                        let width = if highlight { 3.0_f32 } else { 2.0_f32 };
                        painter.line_segment(
                            [
                                egui::pos2(x, loop_rect.top()),
                                egui::pos2(x, loop_rect.bottom()),
                            ],
                            egui::Stroke::new(width, color),
                        );
                        for y in [loop_rect.top(), loop_rect.bottom()] {
                            painter.rect_filled(
                                egui::Rect::from_center_size(
                                    egui::pos2(x, y),
                                    egui::vec2(8.0, 8.0),
                                ),
                                1.0,
                                color,
                            );
                        }
                    }
                }
            }

            let any_solo = self.tracks.iter().any(|t| t.solo);
            let mut y = rect.top() + header_h;
            for t in &self.tracks {
                let lane_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.left(), y),
                    egui::vec2(rect.width(), lane_h),
                );
                painter.rect_filled(lane_rect, 2.0, egui::Color32::from_gray(36));
                painter.text(
                    egui::pos2(rect.left() + 8.0, y + lane_h * 0.5),
                    egui::Align2::LEFT_CENTER,
                    &t.name,
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_gray(220),
                );

                let dim = t.muted || (any_solo && !t.solo);
                let fill = if dim { egui::Color32::from_gray(80) } else { t.color };

                // Draw each clip segment with inner waveforms and markers mapped to its own scope
                for clip in &t.clips {
                    let clip_start_sec = clip.start_beats * 60.0 / self.project_bpm;
                    let clip_dur_sec = clip.length_beats * 60.0 / self.project_bpm;
                    let block_x = rect.left() + label_w + clip_start_sec * px_per_sec;
                    let block_w = clip_dur_sec * px_per_sec;
                    
                    let block_rect = egui::Rect::from_min_size(
                        egui::pos2(block_x, y + 4.0),
                        egui::vec2(block_w.max(4.0), lane_h - 8.0),
                    );
                    
                    painter.rect_filled(block_rect, 3.0, fill.gamma_multiply(0.55));
                    painter.rect_stroke(block_rect, 3.0, egui::Stroke::new(1.0_f32, fill));

                    // Waveforms rendered inside this clip block
                    if t.waveform.num_buckets() > 0 && block_w > 4.0 {
                        let cy = block_rect.center().y;
                        let half_h = block_rect.height() * 0.5 - 3.0;
                        let src_frames = t.decoded.num_frames();
                        let bucket_size = t.waveform.samples_per_bucket as f32;
                        let n_buckets = t.waveform.num_buckets();
                        let x_start = block_rect.left().ceil() as i32;
                        let x_end = block_rect.right().floor() as i32;
                        for px in x_start..=x_end {
                            let frac = (px as f32 - block_rect.left()) / block_w;
                            let src_frame = frac * src_frames as f32;
                            let bucket = (src_frame / bucket_size) as usize;
                            if bucket >= n_buckets { continue; }
                            let mn = t.waveform.mins[bucket];
                            let mx = t.waveform.maxes[bucket];
                            painter.line_segment(
                                [egui::pos2(px as f32, cy - mx * half_h), egui::pos2(px as f32, cy - mn * half_h)],
                                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 130)),
                            );
                        }
                    }

                    // Beat ticks relative to this clip
                    if !t.beat_positions.is_empty() {
                        let stretch = match t.native_tempo() { Some(nt) if nt > 0.0 => (nt / self.project_bpm) as f32, _ => 1.0 };
                        for bp in &t.beat_positions {
                            let bx = block_x + bp * stretch * px_per_sec;
                            if bx > block_rect.right() { break; }
                            if bx >= block_rect.left() {
                                painter.line_segment(
                                    [egui::pos2(bx, block_rect.top() + 2.0), egui::pos2(bx, block_rect.bottom() - 2.0)],
                                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 90)),
                                );
                            }
                        }
                    }
                }



                y += lane_h + lane_gap;
            }

            if let Some(p) = &self.player {
                let frac = p.position_frac();
                let t = (frac as f32) * total_secs;
                let x = rect.left() + label_w + t * px_per_sec;
                if x <= rect.right() {
                    painter.line_segment(
                        [
                            egui::pos2(x, rect.top() + header_h),
                            egui::pos2(x, rect.bottom()),
                        ],
                        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(255, 80, 80)),
                    );
                }
            }

            if show_bar_labels {
                for b in 0..=n_bars {
                    let x =
                        rect.left() + label_w + (b as f32 * bar_secs) * px_per_sec;
                    if x > rect.right() {
                        break;
                    }
                    let secs = b as f32 * bar_secs;
                    let bar = b + 1;
                    painter.text(
                        egui::pos2(x + 3.0, rect.top() + 2.0),
                        egui::Align2::LEFT_TOP,
                        format!("bar {} ({:.2}s)", bar, secs),
                        egui::FontId::monospace(10.0),
                        egui::Color32::from_gray(150),
                    );
                }
            }
            if show_sec_labels || !show_bar_labels {
                let step = if px_per_sec < 2.0 {
                    30.0
                } else if px_per_sec < 6.0 {
                    10.0
                } else {
                    5.0
                };
                let n = (max_secs / step).ceil() as i32 + 1;
                for i in 0..=n {
                    let secs = i as f32 * step;
                    let x = rect.left() + label_w + secs * px_per_sec;
                    if x > rect.right() {
                        break;
                    }
                    let mm = (secs / 60.0).floor() as i32;
                    let ss = secs - mm as f32 * 60.0;
                    painter.text(
                        egui::pos2(x + 3.0, rect.top() + 2.0),
                        egui::Align2::LEFT_TOP,
                        format!("{}:{:05.2}", mm, ss),
                        egui::FontId::monospace(10.0),
                        egui::Color32::from_gray(150),
                    );
                }
            }

            painter.line_segment(
                [
                    egui::pos2(rect.left() + label_w, rect.top() + header_h),
                    egui::pos2(rect.left() + label_w, rect.bottom()),
                ],
                egui::Stroke::new(1.0_f32, egui::Color32::from_gray(70)),
            );
        });

        // Settings window
        if self.show_settings {
            let mut open = true;
            egui::Window::new("Settings")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Playhead latency compensation:");
                    ui.horizontal(|ui| {
                        let mut ms = self.latency_comp_ms;
                        if ui
                            .add(
                                egui::Slider::new(&mut ms, -2000.0..=3000.0)
                                    .suffix(" ms")
                                    .fixed_decimals(0),
                            )
                            .changed()
                        {
                            self.latency_comp_ms = ms;
                            if let Some(p) = &self.player {
                                p.set_latency_comp_secs((ms / 1000.0) as f64);
                            }
                        }
                        if ui.small_button("reset").clicked() {
                            self.latency_comp_ms = 0.0;
                            if let Some(p) = &self.player {
                                p.set_latency_comp_secs(0.0);
                            }
                        }
                    });
                    ui.small("Negative = playhead moves forward. Positive = playhead moves back.");
                    ui.separator();
                    ui.label("Export sample rate:");
                    ui.horizontal(|ui| {
                        let rates: &[(u32, &str)] = &[
                            (0, "Device"),
                            (44100, "44.1k"),
                            (48000, "48k"),
                            (96000, "96k"),
                            (192000, "192k"),
                        ];
                        for (r, label) in rates {
                            if ui
                                .selectable_label(self.render_settings.export_rate == *r, *label)
                                .clicked()
                            {
                                self.render_settings.export_rate = *r;
                            }
                        }
                    });
                    ui.separator();
                    ui.label("Export bit depth:");
                    ui.horizontal(|ui| {
                        for d in [BitDepth::Int16, BitDepth::Int24, BitDepth::Float32] {
                            if ui
                                .selectable_label(
                                    self.render_settings.export_bit_depth == d,
                                    d.label(),
                                )
                                .clicked()
                            {
                                self.render_settings.export_bit_depth = d;
                            }
                        }
                    });
                    ui.separator();
                    ui.label("Transient handling (Signalsmith):");
                    ui.horizontal(|ui| {
                        for mode in [
                            TransientMode::Smooth,
                            TransientMode::Balanced,
                            TransientMode::Sharp,
                        ] {
                            if ui
                                .selectable_label(
                                    self.render_settings.transient == mode,
                                    mode.label(),
                                )
                                .clicked()
                            {
                                self.render_settings.transient = mode;
                            }
                        }
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        let mut pf = self.render_settings.preserve_formants;
                        if ui.checkbox(&mut pf, "preserve formants on pitch shift").changed() {
                            self.render_settings.preserve_formants = pf;
                        }
                    });
                });
            if !open {
                self.show_settings = false;
            }
        }

        // Beatmapper wizard
        if self.wizard.is_some() {
            let track_name = self
                .wizard
                .as_ref()
                .and_then(|w| self.tracks.get(w.track_idx))
                .map(|t| t.name.clone())
                .unwrap_or_else(|| "?".to_string());
            let playing = self
                .player
                .as_ref()
                .map(|p| p.is_playing())
                .unwrap_or(false);
            let mut close_ok = false;
            let mut close_cancel = false;
            let mut do_stop = false;
            let mut do_start = false;
            let mut changed = false;

            egui::Window::new("Beatmapper Wizard")
                .collapsible(false)
                .resizable(false)
                .default_width(540.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    let w = match self.wizard.as_mut() {
                        Some(w) => w,
                        None => return,
                    };
                    ui.label(format!("Track: {}", track_name));
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("Tempo:");
                        if ui
                            .add(
                                egui::DragValue::new(&mut w.tempo)
                                    .speed(0.05)
                                    .range(40.0..=240.0)
                                    .suffix(" BPM")
                                    .fixed_decimals(2),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        if ui.small_button("half").clicked() {
                            w.tempo = (w.tempo / 2.0).max(40.0);
                            changed = true;
                        }
                        if ui.small_button("double").clicked() {
                            w.tempo = (w.tempo * 2.0).min(240.0);
                            changed = true;
                        }
                        ui.label(format!("(conf {:.3})", w.analysis.confidence));
                    });

                    ui.horizontal(|ui| {
                        ui.label("First beat:");
                        if ui
                            .add(
                                egui::DragValue::new(&mut w.first_beat_secs)
                                    .speed(0.005)
                                    .range(0.0..=w.analysis.total_secs)
                                    .suffix(" s")
                                    .fixed_decimals(4),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        if ui.small_button("snap").clicked() {
                            w.first_beat_secs =
                                snap_to_onset(&w.analysis, w.first_beat_secs, 0.15);
                            changed = true;
                        }
                        if ui.small_button("prev").clicked() {
                            w.first_beat_secs =
                                (w.first_beat_secs - 60.0 / w.tempo).max(0.0);
                            changed = true;
                        }
                        if ui.small_button("next").clicked() {
                            w.first_beat_secs += 60.0 / w.tempo;
                            changed = true;
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Bar length:");
                        for n in 2u16..=8 {
                            let sel = w.bar_beats == n;
                            if ui.selectable_label(sel, format!("{}", n)).clicked() {
                                w.bar_beats = n;
                                changed = true;
                            }
                        }
                        ui.label("beats");
                    });

                    ui.separator();

                    ui.horizontal(|ui| {
                        let mut metro = w.metronome;
                        if ui.checkbox(&mut metro, "metronome").changed() {
                            w.metronome = metro;
                            changed = true;
                        }
                        if playing {
                            if ui.button("stop").clicked() {
                                do_stop = true;
                            }
                        } else if ui.button("preview").clicked() {
                            do_start = true;
                        }
                        let period = 60.0 / w.tempo;
                        let total = w.analysis.total_secs;
                        let beats = ((total - w.first_beat_secs) / period)
                            .floor()
                            .max(0.0);
                        let bars = beats / w.bar_beats as f32;
                        ui.label(format!("= {:.2} beats, {:.2} bars", beats, bars));
                    });

                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("apply").clicked() {
                            close_ok = true;
                        }
                        if ui.button("cancel").clicked() {
                            close_cancel = true;
                        }
                    });
                });

            if changed {
                if let Some(w) = self.wizard.as_mut() {
                    w.last_change = Some(Instant::now());
                }
                self.apply_wizard_to_track();
                self.push_state_to_player();
            }
            if close_ok {
                self.accept_wizard();
            } else if close_cancel {
                self.cancel_wizard();
            }
            if do_stop {
                self.stop_playback();
            }
            if do_start {
                self.start_playback();
            }
        }
    }
}

fn pick_files_blocking() -> Result<Vec<PathBuf>, String> {
    use std::process::Command;
    let attempts: &[(&str, &[&str])] = &[
        (
            "zenity",
            &[
                "--file-selection",
                "--multiple",
                "--separator=\\n",
                "--title=Add tracks",
                "--file-filter=Audio | *.wav *.mp3 *.flac *.ogg *.m4a *.aac *.opus *.wma",
                "--file-filter=All | *",
            ],
        ),
        (
            "qarma",
            &[
                "--file-selection",
                "--multiple",
                "--separator=\\n",
                "--title=Add tracks",
            ],
        ),
        (
            "kdialog",
            &[
                "--title",
                "Add tracks",
                "--getopenfilename",
                ".",
                "--multiple",
                "--separate-output",
            ],
        ),
    ];
    for (cmd, args) in attempts {
        match Command::new(cmd).args(*args).output() {
            Ok(out) => {
                if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    let paths: Vec<PathBuf> = text
                        .lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .map(PathBuf::from)
                        .collect();
                    return Ok(paths);
                }
                return Ok(Vec::new());
            }
            Err(_) => continue,
        }
    }
    Err("no file picker found (install zenity, qarma, or kdialog)".to_string())
}
