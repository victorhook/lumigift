//! Show studio: audio loading + analysis, the show → light pipeline, toolbar,
//! stage preview + cue inspector, transport (docs/UI_DESIGN.md §3.2 Studio, §4.4).
//! The timeline itself lives in `timeline.rs`.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use egui::{Align, Align2, Color32, CornerRadius, Key, KeyboardShortcut, Layout, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use lumigift_audio::engine::{EngineCursor, RHYTHM_THRESHOLD};
use lumigift_audio::{AnalysisCache, BeatSync, Cue, FeatureProvider, Job, LoadedTrack, OneShot, OneShotKind, Player, Show, ShowEngine, ShowSettings, TrackRef};
use lumigift_fx::{EFFECT_NAMES, PALETTES, PALETTE_NAMES};
use lumigift_proto::MusicFrame;

use crate::app::{App, Persist, fmt_mmss, fmt_timecode};
use crate::icons;
use crate::pages::timeline;
use crate::stage::{StageGift, StageOptions};
use crate::theme::{self, *};
use crate::toasts::{ToastKind, Toasts};
use crate::widgets::{self, EmptyAction, EmptyState};

pub const SNAPS: [&str; 8] = ["Off", "1/16 beat", "1/8 beat", "1/4 beat", "1/2 beat", "1 beat", "1 bar", "Section"];

#[derive(Clone, Copy, PartialEq)]
pub enum Drag {
    None,
    MoveCues { grab_t: f64, primary: usize },
    CueEdge { idx: usize },
    MoveEvent { idx: usize, grab_t: f64 },
    Scrub,
    LoopEdge(u8),
    LoopMove { grab_t: f64 },
    LoopCreate { start: f64 },
    Pan { start_scroll: f64, start_x: f32 },
    Rubber { x0: f32, y0: f32 },
    Minimap { grab_t: f64 },
}

pub struct Studio {
    pub player: Option<Player>,
    pub player_err: Option<String>,
    pub job: Option<Job<LoadedTrack>>,
    pub job_path: Option<PathBuf>,
    pub job_started: Option<Instant>,
    pub load_error: Option<(String, String)>,
    pub track: Option<LoadedTrack>,
    pub engine: Option<ShowEngine>,
    /// Event / envelope cursor of the local preview (the engine's own one
    /// serves the radio stream).
    preview_cursor: EngineCursor,
    pending_show: Option<(PathBuf, Show)>,
    pub show_path: Option<PathBuf>,
    pub dirty: bool,
    undo: Vec<Show>,
    redo: Vec<Show>,
    // view
    pub pps: f32,
    pub target_pps: f32,
    pub scroll_t: f64,
    pub follow: bool,
    pub prev_view: Option<(f32, f64)>,
    pub snap: usize,
    pub snap_on: bool,
    pub sel: BTreeSet<usize>,
    pub sel_events: BTreeSet<usize>,
    pub drag: Drag,
    pub drag_dt: f64,
    pub snap_line: Option<(f64, String)>,
    pub loop_on: bool,
    pub loop_region: Option<(f64, f64)>,
    play_start: f64,
    pub generated_at: Option<Instant>,
    pub generated: HashSet<u64>,
    pub edited: HashSet<u64>,
    pub inserted_at: Option<(usize, Instant)>,
    pub gen_style: usize,
    pub gen_palettes: [bool; 8],
    pub gen_keep: bool,
    pub editing_time: Option<String>,
    pub last_frame: Option<MusicFrame>,
    clipboard: Vec<Cue>,
    pub fit_pending: bool,
    pub upper_tab: usize,
    pub lane_w: f32,
    pub zoom_anchor: Option<(f64, f32)>,
    pub v_scroll: f32,
    pub hover_t: Option<f64>,
}

/// Where the show frames go besides the local preview (see [`Studio::tick`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioPath {
    /// Not LIVE: only the local preview.
    None,
    /// LIVE to the simulator's virtual gifts.
    Simulator,
    /// LIVE to real gifts over the radio bridge.
    Hardware,
}

/// Worst-case delay from `link.set_show` to a simulated gift having the frame:
/// one 10 ms show tick plus ~1 ms of simulated USB and air time.
pub const SIM_PIPELINE: f64 = 0.011;

/// The frames of one display frame (see [`Studio::tick`]).
#[derive(Clone, Copy, Debug)]
pub struct TickFrames {
    /// For the local preview, timed for the next vsync.
    pub preview: MusicFrame,
    /// For the radio (LIVE), timed for when the gifts show it.
    pub radio: Option<MusicFrame>,
}

pub fn key_t(t: f64) -> u64 {
    (t * 1000.0).round().max(0.0) as u64
}

impl Studio {
    pub fn new(persist: &Persist) -> Self {
        let (player, player_err) = match Player::with_device(persist.audio_device.as_deref()) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e.to_string())),
        };
        Self {
            player,
            player_err,
            job: None,
            job_path: None,
            job_started: None,
            load_error: None,
            track: None,
            engine: None,
            preview_cursor: EngineCursor::new(),
            pending_show: None,
            show_path: None,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            pps: 40.0,
            target_pps: 40.0,
            scroll_t: 0.0,
            follow: true,
            prev_view: None,
            snap: 5,
            snap_on: true,
            sel: BTreeSet::new(),
            sel_events: BTreeSet::new(),
            drag: Drag::None,
            drag_dt: 0.0,
            snap_line: None,
            loop_on: false,
            loop_region: None,
            play_start: 0.0,
            generated_at: None,
            generated: HashSet::new(),
            edited: HashSet::new(),
            inserted_at: None,
            gen_style: 1,
            gen_palettes: [true; 8],
            gen_keep: true,
            editing_time: None,
            last_frame: None,
            clipboard: Vec::new(),
            fit_pending: false,
            upper_tab: 0,
            lane_w: 1000.0,
            zoom_anchor: None,
            v_scroll: 0.0,
            hover_t: None,
        }
    }

    // -- loading ------------------------------------------------------------

    pub fn open_audio(&mut self, path: PathBuf) {
        self.job = Some(lumigift_audio::load_track(path.clone(), AnalysisCache::default_location()));
        self.job_path = Some(path);
        self.job_started = Some(Instant::now());
        self.load_error = None;
    }

    pub fn pick_audio(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Open audio")
            .add_filter("Audio", &["mp3", "flac", "wav", "ogg", "m4a", "aac", "opus"])
            .pick_file()
        {
            self.open_audio(p);
        }
    }

    pub fn open_show(&mut self, path: PathBuf, toasts: &mut Toasts) {
        match Show::load(&path) {
            Ok(show) => {
                let audio = PathBuf::from(&show.track.path);
                if self.track.as_ref().is_some_and(|t| t.audio.content_hash == show.track.content_hash && !show.track.content_hash.is_empty()) {
                    self.install_show(show, Some(path));
                } else if audio.exists() {
                    self.pending_show = Some((path, show));
                    self.open_audio(audio);
                } else {
                    toasts.error("Couldn't find the show's audio", format!("{} is missing. Open the audio file to relink it.", audio.display()));
                    self.pending_show = Some((path, show));
                }
            }
            Err(e) => toasts.error("Couldn't open the show", e.to_string()),
        }
    }

    pub fn pick_show(&mut self, toasts: &mut Toasts) {
        if let Some(p) = rfd::FileDialog::new().set_title("Open show").add_filter("Lumigift show", &["lgshow"]).pick_file() {
            self.open_show(p, toasts);
        }
    }

    fn install_show(&mut self, show: Show, path: Option<PathBuf>) {
        if let Some(e) = &mut self.engine {
            e.set_show(show);
            e.reset();
        }
        self.show_path = path;
        self.reset_history();
    }

    /// A freshly loaded show: clean, no undo history, nothing selected.
    fn reset_history(&mut self) {
        self.dirty = false;
        self.undo.clear();
        self.redo.clear();
        self.clear_selection();
    }

    pub fn clear_selection(&mut self) {
        self.sel.clear();
        self.sel_events.clear();
    }

    pub fn save(&mut self, save_as: bool, toasts: &mut Toasts, persist: &mut Persist) {
        let Some(e) = &self.engine else {
            toasts.info("Nothing to save yet: open an audio file first");
            return;
        };
        let path = if save_as || self.show_path.is_none() {
            let default = self.title_stem() + ".lgshow";
            match rfd::FileDialog::new().set_title("Save show").set_file_name(default).add_filter("Lumigift show", &["lgshow"]).save_file() {
                Some(p) => p,
                None => return,
            }
        } else {
            self.show_path.clone().unwrap()
        };
        match e.show().save(&path) {
            Ok(()) => {
                self.dirty = false;
                self.show_path = Some(path.clone());
                persist.last_show = Some(path.clone());
                persist.recent_shows.retain(|p| p != &path);
                persist.recent_shows.insert(0, path);
                persist.recent_shows.truncate(8);
                toasts.success("Show saved");
            }
            Err(e) => toasts.error("Couldn't save the show", e.to_string()),
        }
    }

    pub fn set_device(&mut self, id: Option<String>, toasts: &mut Toasts) {
        match &self.player {
            Some(p) => {
                if let Err(e) = p.set_device(id.as_deref()) {
                    toasts.error("Couldn't switch audio output", e.to_string());
                }
            }
            None => match Player::with_device(id.as_deref()) {
                Ok(p) => {
                    if let Some(t) = &self.track {
                        p.load(t.audio.clone());
                    }
                    self.player = Some(p);
                    self.player_err = None;
                }
                Err(e) => toasts.error("No audio output", e.to_string()),
            },
        }
    }

    pub fn post_frame(&mut self, persist: &mut Persist) {
        let Some(job) = &mut self.job else { return };
        let Some(res) = job.try_take() else { return };
        self.job = None;
        match res {
            Ok(track) => {
                if let Some(p) = &self.player {
                    p.stop();
                    p.load(track.audio.clone());
                }
                let mut show = match self.pending_show.take() {
                    Some((path, show)) => {
                        self.show_path = Some(path.clone());
                        persist.last_show = Some(path);
                        show
                    }
                    None => {
                        self.show_path = None;
                        let mut s = Show::auto_generate(&track.analysis);
                        self.generated_at = Some(Instant::now());
                        self.generated = s.cues.iter().map(|c| key_t(c.time)).collect();
                        s.track = TrackRef::default();
                        s
                    }
                };
                show.track = TrackRef {
                    path: track.path.display().to_string(),
                    content_hash: track.audio.content_hash.clone(),
                    title: track.audio.metadata.title.clone().unwrap_or_default(),
                };
                let provider: Arc<dyn FeatureProvider> = track.analysis.clone();
                self.engine = Some(ShowEngine::new(provider, show));
                self.preview_cursor.reset();
                persist.last_audio = Some(track.path.clone());
                self.track = Some(track);
                self.reset_history();
                self.scroll_t = 0.0;
                self.loop_region = None;
                self.fit_pending = true;
            }
            Err(lumigift_audio::Error::Cancelled) => {}
            Err(e) => {
                let raw = e.to_string();
                let plain = if raw.to_lowercase().contains("unsupported") {
                    "This audio format isn't supported.".to_string()
                } else if raw.to_lowercase().contains("i/o") || raw.to_lowercase().contains("no such file") {
                    "The file couldn't be read.".to_string()
                } else {
                    "The decoder couldn't read this file.".to_string()
                };
                self.load_error = Some((plain, raw));
            }
        }
    }

    // -- queries ------------------------------------------------------------

    pub fn has_track(&self) -> bool {
        self.track.is_some() && self.engine.is_some()
    }
    pub fn is_loading(&self) -> bool {
        self.job.is_some()
    }
    pub fn is_playing(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.is_playing())
    }
    pub fn position(&self) -> f64 {
        self.player.as_ref().map(|p| p.position()).unwrap_or(0.0)
    }
    pub fn duration(&self) -> f64 {
        self.track.as_ref().map(|t| t.analysis.duration).unwrap_or(0.0)
    }
    pub fn position_duration(&self) -> (f64, f64) {
        (self.position(), self.duration())
    }
    pub fn output_name(&self) -> String {
        match &self.player {
            Some(p) => {
                let n = p.device_name();
                if n.is_empty() { "Default output".into() } else { n }
            }
            None => "No audio output".into(),
        }
    }
    pub fn title(&self) -> String {
        let Some(t) = &self.track else { return String::new() };
        t.audio.metadata.title.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| {
            t.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        })
    }
    fn title_stem(&self) -> String {
        self.track
            .as_ref()
            .and_then(|t| t.path.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "show".into())
    }
    pub fn next_section_in(&self, pos: f64) -> Option<f64> {
        let t = self.track.as_ref()?;
        t.analysis.sections.iter().find(|s| s.start > pos + 0.05).map(|s| s.start - pos)
    }
    pub fn show(&self) -> Option<&Show> {
        self.engine.as_ref().map(|e| e.show())
    }

    // -- transport ----------------------------------------------------------

    pub fn toggle_play(&mut self) {
        let Some(p) = &self.player else { return };
        if !self.has_track() {
            return;
        }
        if !p.is_playing() {
            self.play_start = p.position();
            if p.position() >= self.duration() - 0.05 {
                p.seek(0.0);
            }
        }
        p.toggle();
        if let Some(e) = &mut self.engine {
            e.reset();
        }
        self.preview_cursor.reset();
    }
    pub fn seek(&mut self, t: f64) {
        if let Some(p) = &self.player {
            p.seek(t.clamp(0.0, self.duration().max(0.0)));
        }
        if let Some(e) = &mut self.engine {
            e.reset();
        }
        self.preview_cursor.reset();
    }
    pub fn seek_bar(&mut self, bar: u32) {
        let Some(t) = &self.track else { return };
        if let Some(b) = t.analysis.beats.iter().find(|b| b.bar + 1 >= bar && b.beat_in_bar == 0) {
            let tt = b.time;
            self.seek(tt);
        }
    }
    pub fn return_to_start(&mut self) {
        let t = self.play_start;
        self.seek(t);
    }

    /// Advance the show clock for this display frame.
    ///
    /// Each frame is computed for the moment it becomes visible, so the
    /// lights land on the sound in every mode (`Player::position` is what
    /// is heard right now):
    ///
    /// * **Preview** (the Studio stage, rendered locally by
    ///   `lumigift_fx::Renderer` in this GUI frame): visible at the next
    ///   vsync, so `pos + display_dt`. No radio look-ahead: adding it made
    ///   the preview run ahead of the audio by the radio latency (42 ms by
    ///   default), which reads as "eager" lights.
    /// * **Simulator LIVE** (the stage mirrors the virtual gifts): the frame
    ///   is broadcast within one 10 ms scheduler tick and ~1 ms of simulated
    ///   USB + air, rendered instantly by the sim gifts, then read back by
    ///   the next GUI frame and shown one vsync later:
    ///   `pos + display_dt + max(display_dt, SIM_PIPELINE)`. The calibrated
    ///   hardware latency does not apply.
    /// * **Hardware LIVE**: `pos + latency_ms` (the calibrated radio +
    ///   render latency of the real gifts). The stage keeps showing the
    ///   local preview at `pos + display_dt`.
    ///
    /// The radio frame drives the engine's own event cursor (every BEAT /
    /// ONSET is sent once); the preview runs on a second cursor so both
    /// time lines get every event.
    pub fn tick(&mut self, persist: &Persist, slots: u8, display_dt: f64, radio: RadioPath) -> Option<TickFrames> {
        let player = self.player.as_ref()?;
        let engine = self.engine.as_mut()?;
        let mut pos = player.position();
        if self.loop_on && player.is_playing()
            && let Some((a, b)) = self.loop_region
                && b > a + 0.05 && pos >= b {
                    player.seek(a);
                    engine.reset();
                    self.preview_cursor.reset();
                    pos = a;
                }
        let display_dt = display_dt.clamp(0.0, 0.05);
        let playing = player.is_playing();
        let preview_t = pos + display_dt;
        let preview = if playing { engine.peek_frame_at(&mut self.preview_cursor, preview_t, slots) } else { engine.peek_frame(preview_t, slots) };
        let radio_t = match radio {
            RadioPath::None => None,
            RadioPath::Simulator => Some(pos + display_dt + display_dt.max(SIM_PIPELINE)),
            RadioPath::Hardware => Some(pos + persist.latency_ms as f64 / 1000.0),
        };
        let radio = radio_t.map(|t| if playing { engine.frame_at(t, slots) } else { engine.peek_frame(t, slots) });
        self.last_frame = Some(preview);
        Some(TickFrames { preview, radio })
    }

    // -- editing ------------------------------------------------------------

    pub fn checkpoint(&mut self) {
        if let Some(e) = &self.engine {
            self.undo.push(e.show().clone());
            if self.undo.len() > 500 {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.dirty = true;
        }
    }
    pub fn undo(&mut self) {
        self.step_history(true);
    }
    pub fn redo(&mut self) {
        self.step_history(false);
    }
    /// Pop a show off the undo (or redo) stack, pushing the current one onto the other.
    fn step_history(&mut self, undo: bool) {
        let (from, to) = if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        if let (Some(e), Some(s)) = (&mut self.engine, from.pop()) {
            to.push(e.show().clone());
            e.set_show(s);
            self.clear_selection();
            self.dirty = true;
        }
    }
    pub fn show_mut(&mut self) -> Option<&mut Show> {
        self.engine.as_mut().map(|e| e.show_mut())
    }

    pub fn insert_cue_at(&mut self, t: f64) {
        if !self.has_track() {
            return;
        }
        self.checkpoint();
        let show = self.show_mut().unwrap();
        let i = show.split_at(t);
        let tt = show.cues[i].time;
        self.sel = [i].into();
        self.sel_events.clear();
        self.edited.insert(key_t(tt));
        self.inserted_at = Some((i, Instant::now()));
    }
    pub fn insert_cue_at_playhead(&mut self) {
        let t = self.snap_time_simple(self.position());
        self.insert_cue_at(t);
    }
    pub fn add_marker_at(&mut self, t: f64) {
        if !self.has_track() {
            return;
        }
        self.checkpoint();
        let show = self.show_mut().unwrap();
        let i = show.insert_event(OneShot { time: t, duration: 0.15, kind: OneShotKind::Flash { hue: 0, palette: None } });
        self.sel_events = [i].into();
        self.sel.clear();
    }
    pub fn delete_selection(&mut self) {
        if self.sel.is_empty() && self.sel_events.is_empty() {
            return;
        }
        self.checkpoint();
        let sel: Vec<usize> = self.sel.iter().rev().copied().collect();
        let evs: Vec<usize> = self.sel_events.iter().rev().copied().collect();
        let show = self.show_mut().unwrap();
        for i in sel {
            if show.cues.len() > 1 {
                show.remove_cue(i);
            }
        }
        for i in evs {
            show.remove_event(i);
        }
        self.clear_selection();
    }
    pub fn edit_selected(&mut self, f: impl Fn(&mut Cue)) {
        if self.sel.is_empty() {
            return;
        }
        self.checkpoint();
        let sel = self.sel.clone();
        let mut keys = Vec::new();
        if let Some(show) = self.show_mut() {
            for i in sel {
                if let Some(c) = show.cues.get_mut(i) {
                    f(c);
                    keys.push(key_t(c.time));
                }
            }
        }
        self.edited.extend(keys);
    }
    pub fn duplicate(&mut self) {
        let Some(show) = self.show() else { return };
        if self.sel.is_empty() {
            return;
        }
        let cues: Vec<Cue> = self.sel.iter().filter_map(|i| show.cues.get(*i).cloned()).collect();
        let first = cues.first().map(|c| c.time).unwrap_or(0.0);
        let last_i = *self.sel.iter().last().unwrap();
        let end = show.cues.get(last_i + 1).map(|c| c.time).unwrap_or(self.duration());
        let off = end - first;
        self.checkpoint();
        let show = self.show_mut().unwrap();
        let mut new_sel = BTreeSet::new();
        for mut c in cues {
            c.time += off;
            new_sel.insert(show.insert_cue(c));
        }
        let times: Vec<f64> = new_sel.iter().map(|i| show.cues[*i].time).collect();
        self.select_times(&times);
    }
    pub fn copy(&mut self) {
        if let Some(show) = self.show() {
            self.clipboard = self.sel.iter().filter_map(|i| show.cues.get(*i).cloned()).collect();
        }
    }
    pub fn paste(&mut self) {
        if self.clipboard.is_empty() || !self.has_track() {
            return;
        }
        let at = self.position();
        let first = self.clipboard[0].time;
        let clip = self.clipboard.clone();
        self.checkpoint();
        let show = self.show_mut().unwrap();
        let mut times = Vec::new();
        for mut c in clip {
            c.time = c.time - first + at;
            times.push(c.time);
            show.insert_cue(c);
        }
        self.select_times(&times);
    }

    /// Select the cues starting at `times` (indices shift as cues are re-inserted).
    fn select_times(&mut self, times: &[f64]) {
        let Some(show) = self.show() else { return };
        self.sel = times.iter().filter_map(|t| show.cues.iter().position(|c| (c.time - t).abs() < 1e-6)).collect();
    }

    /// Move the selected cues by `d` seconds (one undo step); returns their new start times.
    pub fn move_selected_cues(&mut self, d: f64) -> Vec<f64> {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        self.checkpoint();
        let Some(show) = self.show_mut() else { return Vec::new() };
        let mut moved: Vec<Cue> = sel.iter().rev().filter_map(|i| show.remove_cue(*i)).collect();
        let mut times = Vec::new();
        for c in moved.iter_mut() {
            c.time = (c.time + d).max(0.0);
            times.push(c.time);
        }
        for c in moved {
            show.insert_cue(c);
        }
        self.select_times(&times);
        times
    }

    pub fn auto_generate_now(&mut self, toasts: &mut Toasts) {
        let Some(track) = &self.track else {
            toasts.info("Open an audio file first");
            return;
        };
        let mut s = Show::auto_generate(&track.analysis);
        // Style: scale brightness; palettes: remap onto the chosen set.
        let scale = [0.65, 1.0, 1.0][self.gen_style];
        let chosen: Vec<u8> = (0..8u8).filter(|i| self.gen_palettes[*i as usize]).collect();
        for (k, c) in s.cues.iter_mut().enumerate() {
            c.brightness = ((c.brightness as f32) * scale).min(255.0) as u8;
            if self.gen_style == 2 {
                c.brightness = c.brightness.max(220);
                c.hue_speed *= 1.5;
            }
            if self.gen_style == 0 && matches!(c.effect, 4) {
                c.effect = 1;
            }
            if !chosen.is_empty() && !chosen.contains(&c.palette) {
                c.palette = chosen[k % chosen.len()];
            }
        }
        if self.gen_style == 0 {
            s.events.retain(|e| !matches!(e.kind, OneShotKind::Strobe { .. }));
        }
        self.checkpoint();
        let keep = self.gen_keep;
        let edited = self.edited.clone();
        let Some(engine) = &mut self.engine else { return };
        let old = engine.show().clone();
        let mut new_show = old.clone();
        new_show.cues = s.cues;
        new_show.events = s.events;
        if keep {
            for c in old.cues.iter().filter(|c| edited.contains(&key_t(c.time))) {
                new_show.insert_cue(c.clone());
            }
        }
        self.generated = new_show.cues.iter().map(|c| key_t(c.time)).filter(|k| !edited.contains(k)).collect();
        engine.set_show(new_show);
        self.generated_at = Some(Instant::now());
        self.clear_selection();
        toasts.push(ToastKind::Success, "Show generated", Some("Ctrl+Z to undo".into()));
    }

    // -- snapping -----------------------------------------------------------

    /// Grid point nearest to `t` for the current snap unit (no candidates/radius).
    pub fn grid_snap(&self, t: f64) -> Option<f64> {
        let a = &self.track.as_ref()?.analysis;
        if !self.snap_on || self.snap == 0 || a.beats.len() < 2 {
            return None;
        }
        match self.snap {
            6 => a.nearest_downbeat(t),
            7 => a.sections.iter().map(|s| s.start).min_by(|x, y| (x - t).abs().total_cmp(&(y - t).abs())),
            unit => {
                let div = [0, 16, 8, 4, 2, 1][unit] as f64;
                let i = a.beat_index_at(t).unwrap_or(0);
                let b0 = a.beats[i].time;
                let b1 = a.beats.get(i + 1).map(|b| b.time).unwrap_or(b0 + 60.0 / a.bpm.max(1.0) as f64);
                let (b0, b1) = if t < b0 { (b0 - (b1 - b0), b0) } else { (b0, b1) };
                let step = (b1 - b0) / div;
                Some(b0 + ((t - b0) / step).round() * step)
            }
        }
    }
    pub fn snap_time_simple(&self, t: f64) -> f64 {
        self.grid_snap(t).unwrap_or(t)
    }
    pub fn snap_unit_secs(&self) -> f64 {
        let Some(t) = &self.track else { return 0.1 };
        let beat = 60.0 / t.analysis.bpm.max(1.0) as f64;
        match self.snap {
            1 => beat / 16.0,
            2 => beat / 8.0,
            3 => beat / 4.0,
            4 => beat / 2.0,
            6 | 7 => beat * 4.0,
            0 => 0.05,
            _ => beat,
        }
    }
}

pub fn handle_dropped_files(app: &mut App, ctx: &egui::Context) {
    let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
    for p in dropped {
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "lgshow" => {
                app.studio.open_show(p, &mut app.toasts);
                app.set_page(crate::app::Page::Studio);
            }
            "lgfw" | "bin" => {
                app.firmware.load(p);
                app.set_page(crate::app::Page::Firmware);
            }
            _ => {
                app.studio.open_audio(p);
                app.set_page(crate::app::Page::Studio);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

fn shortcuts(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = Modifiers::COMMAND;
    let cs = Modifiers::COMMAND | Modifiers::SHIFT;
    let sc = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
    if sc(cs, Key::O) {
        app.studio.pick_audio();
    } else if sc(c, Key::O) {
        app.studio.pick_show(&mut app.toasts);
    }
    if sc(cs, Key::S) {
        app.studio.save(true, &mut app.toasts, &mut app.persist);
    } else if sc(c, Key::S) {
        app.studio.save(false, &mut app.toasts, &mut app.persist);
    }
    if sc(cs, Key::Z) {
        app.studio.redo();
    } else if sc(c, Key::Z) {
        app.studio.undo();
    }
    if sc(c, Key::G) {
        app.studio.auto_generate_now(&mut app.toasts);
    }
    if sc(c, Key::D) {
        app.studio.duplicate();
    }
    if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
        app.studio.copy();
    }
    if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Cut))) {
        app.studio.copy();
        app.studio.delete_selection();
    }
    if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))) && !ctx.egui_wants_keyboard_input() {
        app.studio.paste();
    }
    if ctx.egui_wants_keyboard_input() || app.palette.is_open() {
        return;
    }
    let s = &mut app.studio;
    let n = Modifiers::NONE;
    let sh = Modifiers::SHIFT;
    if sc(n, Key::Space) {
        s.toggle_play();
    }
    if sc(n, Key::Home) {
        s.seek(0.0);
    }
    if sc(n, Key::End) {
        let d = s.duration();
        s.seek(d);
    }
    if sc(n, Key::L) {
        s.loop_on = !s.loop_on;
        if s.loop_on && s.loop_region.is_none() {
            let p = s.position();
            let bar = s.snap_unit_secs().max(0.5) * 4.0;
            s.loop_region = Some((p, p + bar * 2.0));
        }
    }
    if sc(n, Key::I) {
        let p = s.position();
        let e = s.loop_region.map(|r| r.1).unwrap_or(p + 8.0).max(p + 0.1);
        s.loop_region = Some((p, e));
        s.loop_on = true;
    }
    if sc(n, Key::O) {
        let p = s.position();
        let b = s.loop_region.map(|r| r.0).unwrap_or((p - 8.0).max(0.0)).min(p - 0.1).max(0.0);
        s.loop_region = Some((b, p));
        s.loop_on = true;
    }
    if sc(n, Key::Enter) {
        s.return_to_start();
    }
    let unit = s.snap_unit_secs();
    let bar = 4.0 * 60.0 / s.track.as_ref().map(|t| t.analysis.bpm.max(1.0) as f64).unwrap_or(120.0);
    for (m, k, d) in [(n, Key::ArrowLeft, -unit), (n, Key::ArrowRight, unit), (sh, Key::ArrowLeft, -bar), (sh, Key::ArrowRight, bar)] {
        if sc(m, k) {
            if s.sel.is_empty() {
                let p = s.position();
                s.seek(p + d);
            } else {
                s.move_selected_cues(d);
            }
        }
    }
    if sc(c, Key::ArrowLeft) || sc(c, Key::ArrowRight) {
        let fwd = ctx.input(|i| i.key_down(Key::ArrowRight));
        if let Some(t) = &s.track {
            let p = s.position();
            let target = if fwd {
                t.analysis.sections.iter().map(|x| x.start).find(|x| *x > p + 0.05)
            } else {
                t.analysis.sections.iter().map(|x| x.start).rev().find(|x| *x < p - 0.3)
            };
            if let Some(tt) = target {
                s.seek(tt);
            }
        }
    }
    if sc(n, Key::C) {
        s.insert_cue_at_playhead();
    }
    if sc(n, Key::M) {
        let t = s.snap_time_simple(s.position());
        s.add_marker_at(t);
    }
    if sc(n, Key::N) {
        s.snap_on = !s.snap_on;
    }
    if sc(n, Key::F) {
        s.follow = !s.follow;
    }
    if sc(n, Key::Delete) || sc(n, Key::Backspace) {
        s.delete_selection();
    }
    if sc(n, Key::OpenBracket) {
        s.edit_selected(|c| c.brightness = c.brightness.saturating_sub(26));
    }
    if sc(n, Key::CloseBracket) {
        s.edit_selected(|c| c.brightness = c.brightness.saturating_add(26));
    }
    let nums = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8];
    for (i, k) in nums.iter().enumerate() {
        if sc(sh, *k) {
            s.edit_selected(|c| c.palette = i as u8);
        } else if sc(n, *k) {
            s.edit_selected(|c| c.effect = i as u8);
        }
    }
    if sc(sh, Key::Z) {
        timeline::zoom_fit(s, None);
    } else if sc(n, Key::Z) {
        timeline::zoom_selection(s);
    }
    if sc(n, Key::X)
        && let Some((pps, sc_t)) = s.prev_view.take() {
            s.target_pps = pps;
            s.scroll_t = sc_t;
        }
    if sc(n, Key::Plus) || sc(n, Key::Equals) {
        let p = s.position();
        timeline::zoom_around(s, 1.4, p, None);
    }
    if sc(n, Key::Minus) {
        let p = s.position();
        timeline::zoom_around(s, 1.0 / 1.4, p, None);
    }
}

pub fn page(app: &mut App, ui: &mut Ui) {
    shortcuts(app, ui);
    toolbar(app, ui);

    if let Some(job) = &app.studio.job {
        analysing(app, ui, job.progress(), job.stage());
        return;
    }
    if let Some((plain, raw)) = app.studio.load_error.clone() {
        let mut details = false;
        let act = widgets::empty_state(
            ui,
            &EmptyState { headline: "Couldn't read this file", body: &plain, primary: Some(("Choose another file", Some(icons::FOLDER_OPEN))), secondary: Some("Details"), big: false },
            |ui| {
                if ui.data(|d| d.get_temp::<bool>(egui::Id::unique("err-details")).unwrap_or(false)) {
                    ui.add_space(8.0);
                    egui::Frame::new().fill(BG_SUNKEN).corner_radius(CornerRadius::same(R_SM)).inner_margin(10).show(ui, |ui| {
                        ui.add(egui::Label::new(widgets::rich(&raw, "mono", TEXT_MUTED)).wrap().selectable(true));
                    });
                }
            },
        );
        match act {
            EmptyAction::Primary => app.studio.pick_audio(),
            EmptyAction::Secondary => details = true,
            _ => {}
        }
        if details {
            ui.data_mut(|d| {
                let v = d.get_temp::<bool>(egui::Id::unique("err-details")).unwrap_or(false);
                d.insert_temp(egui::Id::unique("err-details"), !v)
            });
        }
        return;
    }
    if !app.studio.has_track() {
        no_audio(app, ui);
        return;
    }

    let compact = app.compact;
    let rect = ui.available_rect_before_wrap();
    let stage_h = if compact { 220.0 } else { app.persist.stage_split_h.clamp(160.0, (rect.height() - 300.0).max(160.0)) };
    // Upper split: stage preview | cue inspector.
    let upper = Rect::from_min_size(rect.min, vec2(rect.width(), stage_h));
    let insp_w = if compact { 0.0 } else { INSPECTOR_W };
    let stage_r = Rect::from_min_max(upper.min, pos2(upper.right() - insp_w, upper.bottom()));
    {
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(stage_r));
        if compact && app.studio.upper_tab == 1 {
            c.painter().rect_filled(stage_r, CornerRadius::ZERO, SURFACE_1);
            let mut ci = c.new_child(egui::UiBuilder::new().max_rect(stage_r.shrink(16.0)));
            egui::ScrollArea::vertical().show(&mut ci, |ui| cue_inspector(app, ui));
        } else {
            stage_preview(app, &mut c, stage_r);
        }
        if compact {
            let tr = Rect::from_min_size(stage_r.min + vec2(12.0, 12.0), vec2(150.0, 28.0));
            let mut tc = c.new_child(egui::UiBuilder::new().max_rect(tr));
            let mut tab = app.studio.upper_tab;
            if widgets::segmented(&mut tc, "upper-tab", &mut tab, &["Stage", "Cue"]) {
                app.studio.upper_tab = tab;
            }
        }
    }
    if !compact {
        let ir = Rect::from_min_max(pos2(stage_r.right(), upper.top()), upper.max);
        ui.painter().rect_filled(ir, CornerRadius::ZERO, SURFACE_1);
        ui.painter().vline(ir.left() + 0.5, ir.y_range(), Stroke::new(1.0, BORDER_SUBTLE));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(ir.shrink2(vec2(16.0, 14.0))));
        egui::ScrollArea::vertical().id_salt("cue-insp").auto_shrink(false).show(&mut c, |ui| cue_inspector(app, ui));
    }
    // Divider (draggable).
    let div = Rect::from_min_size(pos2(rect.left(), upper.bottom() - 3.0), vec2(rect.width(), 6.0));
    let dresp = ui.interact(div, ui.scope_id().with("split"), Sense::drag());
    if dresp.hovered() || dresp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if dresp.dragged() {
        app.persist.stage_split_h = (app.persist.stage_split_h + dresp.drag_delta().y).clamp(200.0, 600.0);
    }
    // Transport (44 px).
    let tr = Rect::from_min_size(pos2(rect.left(), upper.bottom()), vec2(rect.width(), 44.0));
    transport(app, ui, tr);
    // Timeline.
    let tl = Rect::from_min_max(pos2(rect.left(), tr.bottom()), rect.max);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(tl));
    timeline::show(app, &mut c, tl);
    ui.allocate_rect(rect, Sense::hover());
}

fn toolbar(app: &mut App, ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), PAGE_HEADER_H), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, SURFACE_1);
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, BORDER_SUBTLE));
    let inner = rect.shrink2(vec2(16.0, 0.0));
    let ctx = ui.ctx().clone();
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        // File chip.
        let title = if app.studio.has_track() { app.studio.title() } else { "Studio".into() };
        let font = theme::font("title");
        let tw = widgets::text_width(ui, &title, font.clone()).min(280.0);
        let (fr, fresp) = ui.allocate_exact_size(vec2(tw + 44.0, 32.0), Sense::click());
        let p = ui.painter();
        if fresp.hovered() {
            p.rect_filled(fr, CornerRadius::same(R_SM), HOVER);
        }
        widgets::paint_icon(p, pos2(fr.left() + 12.0, fr.center().y), icons::MUSIC_NOTES, 16.0, TEXT_MUTED);
        let t = widgets::truncate(ui, &title, &font, 280.0);
        let tr = p.text(pos2(fr.left() + 26.0, fr.center().y), Align2::LEFT_CENTER, t, font, TEXT);
        if app.studio.dirty {
            p.circle_filled(pos2(tr.right() + 7.0, fr.center().y), 3.0, TEXT_MUTED);
        }
        widgets::paint_icon(p, pos2(fr.right() - 9.0, fr.center().y), icons::CARET_DOWN, 12.0, TEXT_FAINT);
        egui::Popup::menu(&fresp).width(240.0).show(|ui| {
            if ui.button(format!("{}   Open audio…", icons::FILE_AUDIO)).clicked() {
                app.studio.pick_audio();
            }
            if ui.button(format!("{}   Open show…", icons::FOLDER_OPEN)).clicked() {
                app.studio.pick_show(&mut app.toasts);
            }
            ui.add_enabled_ui(app.studio.has_track(), |ui| {
                if ui.button(format!("{}   Save", icons::SAVE)).clicked() {
                    app.studio.save(false, &mut app.toasts, &mut app.persist);
                }
                if ui.button(format!("{}   Save as…", icons::SAVE)).clicked() {
                    app.studio.save(true, &mut app.toasts, &mut app.persist);
                }
            });
            if !app.persist.recent_shows.is_empty() {
                ui.separator();
                widgets::overline(ui, "Recent shows");
                for p in app.persist.recent_shows.clone() {
                    let n = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    if ui.button(n).clicked() {
                        app.studio.open_show(p, &mut app.toasts);
                    }
                }
            }
        });
        if let Some(t) = &app.studio.track {
            let a = &t.analysis;
            if a.rhythm.iter().any(|&r| r >= RHYTHM_THRESHOLD) {
                widgets::text(ui, format!("{:.1} BPM · 4/4 · {}", a.bpm, fmt_mmss(a.duration)), "value_small", TEXT_MUTED);
            } else {
                widgets::text(ui, format!("Free tempo · {}", fmt_mmss(a.duration)), "value_small", TEXT_MUTED)
                    .on_hover_text(format!("No steady pulse detected (rhythm confidence {:.0}%): beat sync stays off in Auto, and the beat grid (~{:.0} BPM) is only a guide for snapping.", a.rhythm_confidence * 100.0, a.bpm));
            }
        }
        sep(ui);
        // Auto-generate popover.
        let gb = widgets::secondary_button(ui, Some(icons::MAGIC_WAND), "Auto-generate", app.studio.has_track())
            .on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Generate a show from the analysis", Some("Ctrl+G")));
        egui::Popup::from_toggle_button_response(&gb)
            .frame(theme::popover())
            .width(300.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                widgets::text(ui, "Auto-generate", "title", TEXT);
                ui.add_space(10.0);
                widgets::text(ui, "Style", "label", TEXT_MUTED);
                let mut st = app.studio.gen_style;
                widgets::segmented(ui, "gen-style", &mut st, &["Calm", "Balanced", "Energetic"]);
                app.studio.gen_style = st;
                ui.add_space(10.0);
                widgets::text(ui, "Palettes", "label", TEXT_MUTED);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    for i in 0..8 {
                        let on = app.studio.gen_palettes[i];
                        let (r, resp) = ui.allocate_exact_size(vec2(84.0, 24.0), Sense::click());
                        let p = ui.painter();
                        p.rect_filled(r, CornerRadius::same(R_PILL), if on { ACCENT_SOFT } else { SURFACE_2 });
                        if on {
                            p.rect_stroke(r, CornerRadius::same(R_PILL), Stroke::new(1.0, theme::alpha(ACCENT, 0.6)), StrokeKind::Inside);
                        }
                        widgets::palette_strip(p, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(14.0, 8.0)), &PALETTES[i], 1.0);
                        p.text(pos2(r.left() + 28.0, r.center().y), Align2::LEFT_CENTER, PALETTE_NAMES[i], theme::font("small"), if on { ACCENT_TEXT } else { TEXT_MUTED });
                        if resp.clicked() {
                            app.studio.gen_palettes[i] = !on;
                        }
                    }
                });
                ui.add_space(10.0);
                ui.checkbox(&mut app.studio.gen_keep, "Keep my edited cues");
                ui.add_space(12.0);
                if widgets::primary_button(ui, Some(icons::MAGIC_WAND), "Generate", true).clicked() {
                    app.studio.auto_generate_now(&mut app.toasts);
                    ui.close();
                }
            });
        sep(ui);
        // Snap.
        widgets::text(ui, icons::METRONOME, "body", TEXT_MUTED);
        let has_beats = app.studio.track.as_ref().is_some_and(|t| t.analysis.beats.len() > 1);
        ui.add_enabled_ui(has_beats, |ui| {
            let label = if app.studio.snap_on { SNAPS[app.studio.snap] } else { "Off" };
            let r = egui::ComboBox::from_id_salt("snap")
                .selected_text(widgets::rich(format!("Snap {label}"), "button", TEXT_SECONDARY))
                .width(120.0)
                .show_ui(ui, |ui| {
                    for (i, s) in SNAPS.iter().enumerate() {
                        if ui.selectable_label(app.studio.snap == i && app.studio.snap_on, *s).clicked() {
                            app.studio.snap = i;
                            app.studio.snap_on = i != 0;
                        }
                    }
                });
            r.response.on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Snap (hold Alt to bypass)", Some("N")));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            live_pill(app, ui, &ctx);
            sep(ui);
            if !app.compact {
                // Latency slider.
                ui.add_sized(vec2(56.0, 20.0), egui::Label::new(widgets::rich(format!("{:.0} ms", app.persist.latency_ms), "value", TEXT_SECONDARY)));
                let s = ui.add_sized(vec2(120.0, 20.0), egui::Slider::new(&mut app.persist.latency_ms, -50.0..=250.0).show_value(false).step_by(1.0));
                if s.double_clicked() {
                    app.persist.latency_ms = app.persist.default_latency_ms;
                }
                s.on_hover_text("Radio + render latency of the real gifts when LIVE. Increase if the gifts light up late. The preview and the simulator compensate automatically. Double-click to reset.");
                widgets::text(ui, format!("{}  Latency", icons::TIMER), "small_medium", TEXT_MUTED);
            }
        });
    });
}

fn sep(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(1.0, 20.0), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, BORDER));
}

fn live_pill(app: &mut App, ui: &mut Ui, ctx: &egui::Context) {
    let enabled = app.backend.is_some() && !app.link().is_some_and(|l| l.ota_running());
    let label = if app.live { if app.is_sim() { "LIVE (SIM)" } else { "LIVE" } } else { "Preview" };
    let font = theme::font("button");
    let w = widgets::text_width(ui, label, font.clone()) + 40.0;
    let (r, resp) = ui.allocate_exact_size(vec2(w, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if app.live {
        if let Some(t0) = app.live_engaged_at {
            let a = t0.elapsed().as_secs_f32() / MOTION_DELIBERATE;
            if a < 1.0 && !app.persist.reduce_motion {
                let spread = 4.0 + 10.0 * a;
                p.rect_filled(r.expand(spread), CornerRadius::same(R_PILL), theme::alpha(ACCENT, 0.5 * (1.0 - a) * 0.5));
                ctx.request_repaint();
            }
        }
        let fill = if resp.hovered() { ACCENT_HOVER } else { ACCENT };
        p.rect_filled(r, CornerRadius::same(R_PILL), fill);
        let a = crate::app::breathe(ui);
        p.circle_filled(pos2(r.left() + 16.0, r.center().y), 4.0, theme::alpha(ON_ACCENT, a));
        p.text(pos2(r.left() + 27.0, r.center().y), Align2::LEFT_CENTER, label, font, ON_ACCENT);
    } else {
        p.rect_filled(r, CornerRadius::same(R_PILL), if resp.hovered() { HOVER } else { SURFACE_3 });
        p.rect_stroke(r, CornerRadius::same(R_PILL), Stroke::new(1.0, BORDER_SUBTLE), StrokeKind::Inside);
        p.circle_stroke(pos2(r.left() + 16.0, r.center().y), 3.5, Stroke::new(1.2, TEXT_MUTED));
        p.text(pos2(r.left() + 27.0, r.center().y), Align2::LEFT_CENTER, label, font, TEXT_MUTED);
    }
    if !enabled {
        ui.painter().rect_filled(r, CornerRadius::same(R_PILL), theme::alpha(SURFACE_1, 0.55));
    }
    let tip = if !enabled {
        if app.backend.is_none() { "Connect the bridge or start the simulator to drive the gifts" } else { "Unavailable during a firmware update" }
    } else if app.live {
        "Driving the real gifts. Click to switch to Preview."
    } else {
        "Lights only in the 3D preview. Gifts not driven."
    };
    let resp = resp.on_hover_ui(|ui| widgets::tooltip_with_keys(ui, tip, Some("Ctrl+L")));
    if resp.clicked() {
        app.toggle_live();
    }
}

fn no_audio(app: &mut App, ui: &mut Ui) {
    let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let avail = ui.available_rect_before_wrap();
    if hovering {
        widgets::dashed_rect(ui.painter(), avail.shrink(16.0), Stroke::new(1.5, ACCENT), 6.0, 5.0);
    }
    let recent = app.persist.recent_shows.clone();
    let mut open_recent = None;
    let act = widgets::empty_state(
        ui,
        &EmptyState {
            headline: "Make a light show",
            body: "Drop an audio file here (MP3, FLAC, WAV, OGG), or open a saved show.",
            primary: Some(("Open audio…", Some(icons::FILE_AUDIO))),
            secondary: Some("Open show…"),
            big: true,
        },
        |ui| {
            if !recent.is_empty() {
                ui.add_space(S7);
                widgets::overline(ui, "Recent shows");
                ui.add_space(6.0);
                for p in recent.iter().take(5) {
                    let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let (r, resp) = ui.allocate_exact_size(vec2(400.0, 32.0), Sense::click());
                    let pa = ui.painter();
                    if resp.hovered() {
                        pa.rect_filled(r, CornerRadius::same(R_SM), HOVER);
                    }
                    widgets::paint_icon(pa, pos2(r.left() + 16.0, r.center().y), icons::WAVEFORM, 15.0, TEXT_MUTED);
                    pa.text(pos2(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, name, theme::font("body"), TEXT);
                    let modified = std::fs::metadata(p).ok().and_then(|m| m.modified().ok()).map(|m| crate::health::fmt_ago(m.elapsed().unwrap_or_default())).unwrap_or_default();
                    pa.text(pos2(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, modified, theme::font("small"), TEXT_FAINT);
                    if resp.clicked() {
                        open_recent = Some(p.clone());
                    }
                }
            }
            if let Some(e) = &app.studio.player_err {
                ui.add_space(S5);
                widgets::text(ui, format!("{}  No audio output: {e}", icons::WARNING), "small", WARN);
            }
        },
    );
    match act {
        EmptyAction::Primary => app.studio.pick_audio(),
        EmptyAction::Secondary => app.studio.pick_show(&mut app.toasts),
        EmptyAction::None => {}
    }
    if let Some(p) = open_recent {
        app.studio.open_show(p, &mut app.toasts);
    }
}

fn analysing(app: &mut App, ui: &mut Ui, progress: f32, stage: String) {
    let avail = ui.available_rect_before_wrap();
    let w = 440.0;
    let r = Rect::from_min_size(pos2(avail.center().x - w / 2.0, avail.top() + avail.height() * 0.3), vec2(w, 180.0));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r));
    let name = app.studio.job_path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    widgets::card_ui(&mut c, |ui| {
        ui.set_width(w - 24.0);
        ui.horizontal(|ui| {
            widgets::spinner(ui, 18.0, INFO);
            widgets::text(ui, format!("Analysing {name}"), "title", TEXT);
        });
        ui.add_space(14.0);
        let steps = ["Decoding", "Bands", "Beats & tempo", "Sections & drops"];
        let cur = if progress < 0.35 {
            0
        } else if stage.contains("spectrum") || stage.contains("Mixing") {
            1
        } else if stage.contains("beat") {
            2
        } else {
            3
        };
        for (i, s) in steps.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.set_height(24.0);
                if i < cur {
                    widgets::text(ui, icons::CHECK, "body", OK);
                } else if i == cur {
                    let (rr, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    ui.painter().circle_filled(rr.center(), 4.0, theme::alpha(INFO, crate::app::breathe(ui)));
                } else {
                    let (rr, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    ui.painter().circle_stroke(rr.center(), 3.5, Stroke::new(1.0, BORDER_STRONG));
                }
                widgets::text(ui, *s, "body", if i == cur { TEXT } else if i < cur { TEXT_MUTED } else { TEXT_FAINT });
                if i == 0 && cur == 0 {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        widgets::text(ui, format!("{:.0}%", progress / 0.35 * 100.0), "value_small", TEXT_MUTED);
                    });
                }
            });
        }
        ui.add_space(10.0);
        let (br, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
        widgets::progress_bar(ui.painter(), br, progress, INFO);
        ui.add_space(12.0);
        if widgets::secondary_button(ui, None, "Cancel", true).clicked() {
            if let Some(j) = &app.studio.job {
                j.cancel();
            }
            app.studio.job = None;
        }
    });
    ui.ctx().request_repaint();
}

fn stage_preview(app: &mut App, ui: &mut Ui, rect: Rect) {
    let mut gifts: Vec<StageGift> = crate::pages::stage_page::stage_gifts(app);
    if gifts.is_empty() {
        // No gifts: preview 8 virtual boxes so the show can be designed offline.
        if let Some(f) = app.studio.last_frame {
            let mut f = f;
            f.slot_count = 8;
            let leds = app.lights.render_virtual(&f, 8);
            gifts = (0..8)
                .map(|i| StageGift {
                    id: 1000 + i as u64,
                    position: [(i as f32 - 3.5) * crate::stage::SPACING, 0.0],
                    yaw: 0.0,
                    leds: leds[i],
                    online: true,
                    selected: false,
                    label: format!("#{:02}", i + 1),
                })
                .collect();
        }
    }
    let opts = StageOptions { labels: false, show_floor_grid: true, ..Default::default() };
    // Re-fit when the gift set or the preview's shape changes.
    let key = gifts.len() * 100_000 + (rect.width() as usize / 8) * 300 + rect.height() as usize / 8;
    if app.stage_page.preview_framed_n != key {
        app.stage_page.preview_framed_n = key;
        crate::stage::fit_tight(&mut app.stage_preview, &gifts, rect.width() / rect.height().max(1.0));
    }
    app.stage_preview.show(ui, &gifts, &opts);
    // Floating buttons bottom-right.
    let br = Rect::from_min_size(rect.right_bottom() - vec2(196.0, 48.0), vec2(184.0, 36.0));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(br).layout(Layout::right_to_left(Align::Center)));
    c.spacing_mut().item_spacing.x = 6.0;
    let ctx = ui.ctx().clone();
    if widgets::secondary_button(&mut c, Some(icons::FULLSCREEN), "Perform", true).on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Performance mode", Some("F11"))).clicked() {
        app.set_performance(&ctx, true);
    }
    if widgets::secondary_button(&mut c, Some(icons::CROSSHAIR), "Frame", true).clicked() {
        crate::stage::fit_tight(&mut app.stage_preview, &gifts, rect.width() / rect.height().max(1.0));
    }
    if app.live {
        ui.painter().text(rect.left_top() + vec2(14.0, 14.0), Align2::LEFT_TOP, format!("{}  Mirroring the gifts", icons::BROADCAST), theme::font("small_medium"), ACCENT_TEXT);
    } else {
        ui.painter().text(rect.left_top() + vec2(14.0, 14.0), Align2::LEFT_TOP, "Preview", theme::font("small_medium"), TEXT_FAINT);
    }
}

fn transport(app: &mut App, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    p.rect_filled(rect, CornerRadius::ZERO, SURFACE_1);
    p.hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, BORDER_SUBTLE));
    p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, BORDER_SUBTLE));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(16.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
    let ui = &mut c;
    ui.spacing_mut().item_spacing.x = 6.0;
    if widgets::ghost_button(ui, icons::SKIP_BACK, "Go to start", Some("Home")).clicked() {
        app.studio.seek(0.0);
    }
    let playing = app.studio.is_playing();
    let (r, resp) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), MOTION_FAST);
    ui.painter().circle_filled(r.center(), 17.0, theme::mix(TEXT, TEXT_SECONDARY, t));
    widgets::paint_icon_fill(ui.painter(), r.center() + vec2(if playing { 0.0 } else { 1.0 }, 0.0), if playing { icons::PAUSE } else { icons::PLAY }, 17.0, BG);
    let resp = resp.on_hover_ui(|ui| widgets::tooltip_with_keys(ui, if playing { "Pause" } else { "Play" }, Some("Space")));
    if resp.clicked() {
        app.studio.toggle_play();
    }
    if widgets::ghost_button(ui, icons::SKIP_FORWARD, "Next section", Some("Ctrl+→")).clicked() {
        let pos = app.studio.position();
        if let Some(d) = app.studio.next_section_in(pos) {
            app.studio.seek(pos + d);
        }
    }
    let loop_on = app.studio.loop_on;
    if widgets::ghost_button_sized(ui, icons::REPEAT, "Loop", Some("L"), CONTROL_H, loop_on).clicked() {
        app.studio.loop_on = !loop_on;
        if app.studio.loop_on && app.studio.loop_region.is_none() {
            let p = app.studio.position();
            app.studio.loop_region = Some((p, p + 8.0));
        }
    }
    ui.add_space(8.0);
    let pos = app.studio.position();
    // Timecode (click to type).
    if let Some(buf) = &mut app.studio.editing_time {
        let r = ui.add(egui::TextEdit::singleline(buf).font(theme::font("timecode")).desired_width(130.0));
        r.request_focus();
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            let s = buf.clone();
            app.studio.editing_time = None;
            if let Some(t) = parse_time(&s) {
                app.studio.seek(t);
            }
        } else if ui.input(|i| i.key_pressed(Key::Escape)) || r.lost_focus() {
            app.studio.editing_time = None;
        }
    } else {
        let r = ui.add(egui::Label::new(widgets::rich(fmt_timecode(pos), "timecode", TEXT)).sense(Sense::click())).on_hover_text("Click to type a time");
        if r.clicked() {
            app.studio.editing_time = Some(fmt_timecode(pos));
        }
    }
    let musical = app
        .studio
        .track
        .as_ref()
        .and_then(|t| t.analysis.beat_position(pos))
        .map(|b| format!("{}.{}.{}", b.bar + 1, b.beat_in_bar + 1, (b.phase * 4.0) as u32 + 1))
        .unwrap_or("—".into());
    widgets::text(ui, musical, "value", TEXT_MUTED);
    ui.add_space(12.0);
    let right_w = 214.0;
    let mm_w = (ui.available_width() - right_w - 12.0).max(80.0);
    let (mr, mresp) = ui.allocate_exact_size(vec2(mm_w, 32.0), Sense::click_and_drag());
    timeline::minimap(app, ui, mr, &mresp);
    ui.add_space(12.0);
    // Zoom.
    if widgets::ghost_button_sized(ui, icons::ZOOM_OUT, "Zoom out", Some("-"), 24.0, false).clicked() {
        let p = app.studio.position();
        timeline::zoom_around(&mut app.studio, 1.0 / 1.4, p, None);
    }
    let mut z = (app.studio.target_pps.ln() - 4f32.ln()) / (2000f32.ln() - 4f32.ln());
    let zr = ui.add_sized(vec2(72.0, 20.0), egui::Slider::new(&mut z, 0.0..=1.0).show_value(false));
    if zr.changed() {
        app.studio.target_pps = (4f32.ln() + z * (2000f32.ln() - 4f32.ln())).exp();
    }
    if widgets::ghost_button_sized(ui, icons::ZOOM_IN, "Zoom in", Some("+"), 24.0, false).clicked() {
        let p = app.studio.position();
        timeline::zoom_around(&mut app.studio, 1.4, p, None);
    }
    if widgets::ghost_button_sized(ui, icons::FIT, "Zoom to fit song", Some("Shift+Z"), 24.0, false).clicked() {
        timeline::zoom_fit(&mut app.studio, None);
    }
}

fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some((m, r)) = s.split_once(':') {
        Some(m.parse::<f64>().ok()? * 60.0 + r.parse::<f64>().ok()?)
    } else {
        s.parse().ok()
    }
}

fn param_labels(effect: u8) -> (&'static str, &'static str) {
    let info = lumigift_fx::EFFECTS.get(effect as usize);
    (info.and_then(|i| i.speed).unwrap_or("Param 1"), info.and_then(|i| i.size).unwrap_or("Param 2"))
}

fn cue_inspector(app: &mut App, ui: &mut Ui) {
    let s = &mut app.studio;
    let Some(show) = s.show() else { return };
    if s.sel.is_empty() {
        if let Some(&ei) = s.sel_events.iter().next() {
            event_inspector(app, ui, ei);
            return;
        }
        ui.horizontal(|ui| {
            widgets::overline(ui, "Cue");
            widgets::text(ui, format!("{} cues · {} one-shots", show.cues.len(), show.events.len()), "value_small", TEXT_FAINT);
        });
        ui.add_space(8.0);
        widgets::text(ui, "Select a cue in the timeline to edit it.", "body", TEXT_MUTED);
        ui.add_space(10.0);
        if widgets::secondary_button(ui, Some(icons::PLUS), "Insert cue at playhead", true).on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Insert cue", Some("C"))).clicked() {
            s.insert_cue_at_playhead();
        }
        ui.add_space(16.0);
        reactivity(s, ui);
        return;
    }
    let sel: Vec<usize> = s.sel.iter().copied().collect();
    let cues: Vec<Cue> = sel.iter().filter_map(|i| show.cues.get(*i).cloned()).collect();
    if cues.is_empty() {
        return;
    }
    let first = cues[0].clone();
    let mixed = |f: &dyn Fn(&Cue) -> u32| cues.iter().any(|c| f(c) != f(&first));
    let bar = s.track.as_ref().and_then(|t| t.analysis.beat_position(first.time)).map(|b| b.bar + 1).unwrap_or(0);
    ui.horizontal(|ui| {
        widgets::overline(ui, if cues.len() > 1 { "Cues" } else { "Cue" });
        if cues.len() > 1 {
            widgets::text(ui, format!("{} selected", cues.len()), "value_small", TEXT_MUTED);
        } else {
            widgets::text(ui, format!("@ {} · bar {bar}", fmt_timecode(first.time)), "value_small", TEXT_MUTED);
        }
    });
    ui.add_space(10.0);
    let lw = 72.0;
    // Effect grid, 3 per row: the classic music effects (keys 1–8) first,
    // then the generic looks.
    widgets::text(ui, "Effect", "label", TEXT_MUTED);
    ui.add_space(4.0);
    let mut new_effect = None;
    let cell_w = ((ui.available_width() - 8.0) / 3.0).floor().max(78.0);
    egui::Grid::new("fx-grid").spacing(vec2(4.0, 4.0)).show(ui, |ui| {
        for i in 0..lumigift_fx::effect::ALL {
            let on = !mixed(&|c| c.effect as u32) && first.effect == i;
            let (r, resp) = ui.allocate_exact_size(vec2(cell_w, 28.0), Sense::click());
            let p = ui.painter();
            p.rect_filled(r, CornerRadius::same(R_SM), if on { ACCENT_SOFT } else if resp.hovered() { HOVER } else { SURFACE_2 });
            if on {
                p.rect_stroke(r, CornerRadius::same(R_SM), Stroke::new(1.0, theme::alpha(ACCENT, 0.7)), StrokeKind::Inside);
            }
            let c = if on { ACCENT_TEXT } else { TEXT_SECONDARY };
            widgets::paint_icon(p, pos2(r.left() + 14.0, r.center().y), icons::EFFECT[i as usize], 14.0, c);
            let font = theme::font("small_medium");
            let name = widgets::truncate(ui, EFFECT_NAMES[i as usize], &font, r.width() - 30.0);
            p.text(pos2(r.left() + 26.0, r.center().y), Align2::LEFT_CENTER, name, font, c);
            let key = (i < lumigift_fx::effect::COUNT).then(|| format!("{}", i + 1));
            let resp = resp.on_hover_ui(|ui| widgets::tooltip_with_keys(ui, EFFECT_NAMES[i as usize], key.as_deref()));
            if resp.clicked() {
                new_effect = Some(i);
            }
            if i % 3 == 2 {
                ui.end_row();
            }
        }
    });
    if let Some(e) = new_effect {
        s.edit_selected(|c| c.effect = e);
    }
    ui.add_space(10.0);
    // Palette.
    let mut new_pal = None;
    ui.horizontal(|ui| {
        widgets::column_label(ui, "Palette", lw, CONTROL_H, TEXT_MUTED);
        let label = if mixed(&|c| c.palette as u32) { "—".to_string() } else { PALETTE_NAMES[first.palette as usize % 8].to_string() };
        egui::ComboBox::from_id_salt("pal").width(200.0).selected_text(label).show_ui(ui, |ui| {
            for i in 0..8u8 {
                if widgets::palette_option(ui, PALETTE_NAMES[i as usize], &PALETTES[i as usize], 190.0, 26.0, 64.0).clicked() {
                    new_pal = Some(i);
                    ui.close();
                }
            }
        });
    });
    if !mixed(&|c| c.palette as u32) {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
        widgets::palette_strip(ui.painter(), Rect::from_min_max(pos2(r.left() + lw + 8.0, r.top()), pos2(r.left() + lw + 208.0, r.bottom())), &PALETTES[first.palette as usize % 8], 1.0);
    }
    if let Some(pl) = new_pal {
        s.edit_selected(|c| c.palette = pl);
    }
    ui.add_space(6.0);
    // Sliders (one checkpoint per drag gesture).
    let mut bright = first.brightness as f32 / 255.0 * 100.0;
    let mut hue = first.hue as f32;
    let mut p1 = first.params[0] as f32;
    let mut p2 = first.params[1] as f32;
    let mut hs = first.hue_speed;
    let (l1, l2) = param_labels(first.effect);
    type CueEdit = Box<dyn Fn(&mut Cue)>;
    let mut changed: Option<CueEdit> = None;
    let slider = |ui: &mut Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str, mixed_v: bool, rainbow: bool| -> (bool, bool) {
        let mut out = (false, false);
        ui.horizontal(|ui| {
            widgets::column_label(ui, label, lw, CONTROL_H, TEXT_MUTED);
            let w = (ui.available_width() - 56.0).clamp(80.0, 200.0);
            let resp = ui.add_sized(vec2(w, 20.0), egui::Slider::new(v, range).show_value(false));
            if rainbow {
                let rr = Rect::from_center_size(resp.rect.center() + vec2(0.0, 8.0), vec2(resp.rect.width() - 8.0, 2.0));
                let hues: Vec<Color32> = (0..=24).map(|k| widgets::rgb32(lumigift_fx::hsv((k * 255 / 24) as u8, 255, 255))).collect();
                let mesh = widgets::h_gradient(rr, &hues);
                ui.painter().add(egui::Shape::mesh(mesh));
            }
            let txt = if mixed_v { "—".to_string() } else if v.fract().abs() < 0.01 || v.abs() >= 10.0 { format!("{:.0}{suffix}", v) } else { format!("{:.1}{suffix}", v) };
            widgets::text(ui, txt, "value", TEXT_SECONDARY);
            out = (resp.drag_started() || (resp.changed() && !resp.dragged()), resp.changed());
        });
        out
    };
    let mut start_gesture = false;
    let (a, b) = slider(ui, "Brightness", &mut bright, 0.0..=100.0, "%", mixed(&|c| c.brightness as u32), false);
    start_gesture |= a;
    if b {
        let v = (bright / 100.0 * 255.0).round() as u8;
        changed = Some(Box::new(move |c: &mut Cue| c.brightness = v));
    }
    let (a, b) = slider(ui, "Hue", &mut hue, 0.0..=255.0, "", mixed(&|c| c.hue as u32), true);
    start_gesture |= a;
    if b {
        let v = hue as u8;
        changed = Some(Box::new(move |c: &mut Cue| c.hue = v));
    }
    let (a, b) = slider(ui, "Hue drift", &mut hs, -64.0..=64.0, "/s", mixed(&|c| c.hue_speed.to_bits()), false);
    start_gesture |= a;
    if b {
        changed = Some(Box::new(move |c: &mut Cue| c.hue_speed = hs));
    }
    let (a, b) = slider(ui, l1, &mut p1, 0.0..=255.0, "", mixed(&|c| c.params[0] as u32), false);
    start_gesture |= a;
    if b {
        let v = p1 as u8;
        changed = Some(Box::new(move |c: &mut Cue| c.params[0] = v));
    }
    let (a, b) = slider(ui, l2, &mut p2, 0.0..=255.0, "", mixed(&|c| c.params[1] as u32), false);
    start_gesture |= a;
    if b {
        let v = p2 as u8;
        changed = Some(Box::new(move |c: &mut Cue| c.params[1] = v));
    }
    if start_gesture {
        s.checkpoint();
    }
    if let Some(f) = changed {
        let sel = s.sel.clone();
        if let Some(show) = s.show_mut() {
            for i in sel {
                if let Some(c) = show.cues.get_mut(i) {
                    f(c);
                }
            }
        }
        s.dirty = true;
        s.edited.extend(cues.iter().map(|c| key_t(c.time)));
    }
    if first.effect == 4 {
        ui.add_space(6.0);
        widgets::note(ui, format!("{}  Strobe flashes quickly. The preview limits flashing on screen; the real gifts are not limited.", icons::WARNING), WARN_SOFT, WARN, 8.0);
    }
    ui.add_space(8.0);
    if cues.len() == 1 {
        ui.horizontal(|ui| {
            widgets::column_label(ui, "Start", lw, CONTROL_H, TEXT_MUTED);
            let mut t = first.time;
            let resp = ui.add(egui::DragValue::new(&mut t).speed(0.01).range(0.0..=s.duration()).custom_formatter(|v, _| fmt_timecode(v)).custom_parser(parse_time));
            if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                let t = s.snap_time_simple(t);
                s.checkpoint();
                let i = sel[0];
                if let Some(show) = s.show_mut()
                    && let Some(ni) = show.move_cue(i, t) {
                        s.sel = [ni].into();
                    }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, Some(icons::REPEAT), "Preview this cue", true).clicked() {
                let i = sel[0];
                let end = s.show().and_then(|sh| sh.cues.get(i + 1).map(|c| c.time)).unwrap_or(s.duration());
                s.loop_region = Some((first.time, end));
                s.loop_on = true;
                s.seek(first.time);
                if !s.is_playing() {
                    s.toggle_play();
                }
            }
            if widgets::danger_button(ui, Some(icons::TRASH), "Delete", false).clicked() {
                s.delete_selection();
            }
        });
    }
}

/// Show-level reactivity settings (saved with the show): beat sync,
/// onset sensitivity, smoothing and intensity. See `lumigift_audio::engine`.
fn reactivity(s: &mut Studio, ui: &mut Ui) {
    let Some(show) = s.show() else { return };
    let settings = show.settings.clone();
    let pos = s.position();
    let rhythm = s.engine.as_ref().map(|e| e.rhythm_at(pos)).unwrap_or(0.0);
    let syncing = s.engine.as_ref().is_some_and(|e| e.beat_sync_active(pos));
    let lw = 72.0;
    ui.horizontal(|ui| {
        widgets::overline(ui, "Reactivity").on_hover_text("How eagerly the lights react to the music. Saved with the show; double-click a slider to reset it.");
        if !settings.reactivity_is_default() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::text_button(ui, "Reset", TEXT_MUTED).on_hover_text("Back to the defaults").clicked() {
                    s.checkpoint();
                    if let Some(sh) = s.show_mut() {
                        sh.settings.reset_reactivity();
                    }
                }
            });
        }
    });
    ui.add_space(8.0);
    // Beat sync.
    ui.horizontal(|ui| {
        widgets::column_label(ui, "Beat sync", lw, CONTROL_H, TEXT_MUTED);
        let mut i = BeatSync::ALL.iter().position(|b| *b == settings.beat_sync).unwrap_or(0);
        let labels: Vec<&str> = BeatSync::ALL.iter().map(|b| b.label()).collect();
        if widgets::segmented(ui, "beat-sync", &mut i, &labels) {
            s.checkpoint();
            if let Some(sh) = s.show_mut() {
                sh.settings.beat_sync = BeatSync::ALL[i];
            }
        }
    });
    let (dot, status) = match settings.beat_sync {
        BeatSync::Auto if syncing => (OK, format!("Following the beat · rhythm {:.0}%", rhythm * 100.0)),
        BeatSync::Auto => (TEXT_FAINT, format!("No steady pulse · rhythm {:.0}%", rhythm * 100.0)),
        BeatSync::On => (WARN, "Always follows the beat grid".to_string()),
        BeatSync::Off => (TEXT_FAINT, "Never follows the beat grid".to_string()),
    };
    ui.horizontal(|ui| {
        ui.add_space(lw + 8.0);
        let (r, _) = ui.allocate_exact_size(vec2(6.0, 16.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 3.0, dot);
        widgets::text(ui, status, "small", TEXT_MUTED);
    })
    .response
    .on_hover_text("Rhythm confidence: how clearly the music has a steady pulse at the playhead. Auto follows the beat where it is at least 50%; elsewhere the lights flow with the phrasing instead.");
    ui.add_space(4.0);
    // Sliders, one undo step per drag gesture.
    let rows: [(&str, f32, &str); 3] = [
        ("Sensitivity", settings.sensitivity, "How many attacks flash and sparkle: low = only the strongest, high = every note"),
        ("Smoothing", settings.smoothing, "How softly levels rise and fall: low = snappy, high = flowing"),
        ("Intensity", settings.intensity, "How strongly loudness drives the lights: low = calm, high = dramatic"),
    ];
    let mut gesture = false;
    let mut new = [settings.sensitivity, settings.smoothing, settings.intensity];
    for (k, (label, value, tip)) in rows.into_iter().enumerate() {
        ui.horizontal(|ui| {
            widgets::column_label(ui, label, lw, CONTROL_H, TEXT_MUTED);
            let mut v = value * 100.0;
            let w = (ui.available_width() - 56.0).clamp(80.0, 200.0);
            let resp = ui.add_sized(vec2(w, 20.0), egui::Slider::new(&mut v, 0.0..=100.0).show_value(false).step_by(1.0)).on_hover_text(tip);
            widgets::text(ui, format!("{v:.0}%"), "value", TEXT_SECONDARY);
            gesture |= resp.drag_started() || (resp.changed() && !resp.dragged());
            if resp.double_clicked() {
                v = ShowSettings::default_reactivity()[k] * 100.0;
                gesture = true;
            }
            new[k] = v / 100.0;
        });
    }
    if new != [settings.sensitivity, settings.smoothing, settings.intensity] {
        if gesture {
            s.checkpoint();
        }
        if let Some(sh) = s.show_mut() {
            sh.settings.sensitivity = new[0];
            sh.settings.smoothing = new[1];
            sh.settings.intensity = new[2];
        }
        s.dirty = true;
    }
}

fn event_inspector(app: &mut App, ui: &mut Ui, ei: usize) {
    let s = &mut app.studio;
    let Some(e) = s.show().and_then(|sh| sh.events.get(ei).copied()) else { return };
    widgets::overline(ui, "One-shot");
    ui.add_space(4.0);
    widgets::text(ui, format!("@ {}", fmt_timecode(e.time)), "value_small", TEXT_MUTED);
    ui.add_space(10.0);
    let mut kind = match e.kind {
        OneShotKind::Flash { .. } => 0,
        OneShotKind::Blackout => 1,
        OneShotKind::Strobe { .. } => 2,
    };
    if widgets::segmented(ui, "evkind", &mut kind, &["Flash", "Blackout", "Strobe"]) {
        let k = match kind {
            0 => OneShotKind::Flash { hue: 0, palette: None },
            1 => OneShotKind::Blackout,
            _ => OneShotKind::Strobe { rate_hz: 8.0 },
        };
        s.checkpoint();
        if let Some(sh) = s.show_mut() {
            sh.events[ei].kind = k;
        }
    }
    ui.add_space(8.0);
    let mut d = e.duration;
    ui.horizontal(|ui| {
        widgets::text(ui, "Duration", "body", TEXT_MUTED);
        let r = ui.add(egui::DragValue::new(&mut d).speed(0.01).range(0.02..=8.0).suffix(" s"));
        if r.changed() {
            if let Some(sh) = s.show_mut() {
                sh.events[ei].duration = d;
            }
            s.dirty = true;
        }
    });
    ui.add_space(10.0);
    if widgets::danger_button(ui, Some(icons::TRASH), "Delete", false).clicked() {
        s.delete_selection();
    }
}
