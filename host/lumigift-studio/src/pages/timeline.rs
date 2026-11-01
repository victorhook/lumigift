//! The Studio timeline: ruler, sections, waveform, band lanes, cues and
//! one-shots, with zoom/scroll, snapping and cue editing (docs/UI_DESIGN.md §4.4).

use std::time::Instant;

use egui::{Align2, Color32, CornerRadius, Mesh, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use lumigift_audio::{Analysis, Beat, Cue, OneShotKind, SectionKind, Show};
use lumigift_fx::{EFFECT_NAMES, PALETTES, PALETTE_NAMES};

use crate::app::{App, fmt_timecode};
use crate::icons;
use crate::pages::studio::{Drag, Studio, key_t};
use crate::theme::{self, *};
use crate::widgets;

const RULER_H: f32 = 32.0;
/// Lane order: Sections, Waveform, Bass, Mid, Treble, Cues, One-shots.
const LANE_H: [f32; 7] = [24.0, 96.0, 28.0, 28.0, 28.0, 64.0, 28.0];
const LANE_NAMES: [&str; 7] = ["Sections", "Waveform", "Bass", "Mid", "Treble", "Cues", "One-shots"];
const LANE_ICONS: [&str; 7] = [icons::FLAG, icons::WAVEFORM, icons::SPEAKER, icons::SLIDERS, icons::SPARKLE, icons::PALETTE, icons::DIAMOND];
const PPS_MIN: f32 = 4.0;
const PPS_MAX: f32 = 2000.0;

pub fn zoom_around(s: &mut Studio, factor: f32, t: f64, x_rel: Option<f32>) {
    let x = x_rel.unwrap_or(((t - s.scroll_t) as f32 * s.pps).clamp(0.0, s.lane_w));
    s.target_pps = (s.target_pps * factor).clamp(PPS_MIN, PPS_MAX);
    s.zoom_anchor = Some((t, x));
}

pub fn zoom_fit(s: &mut Studio, lane_w: Option<f32>) {
    let w = lane_w.unwrap_or(s.lane_w).max(100.0);
    let d = s.duration().max(1.0);
    s.prev_view = Some((s.target_pps, s.scroll_t));
    s.target_pps = (w / d as f32 * 0.98).clamp(PPS_MIN, PPS_MAX);
    s.zoom_anchor = Some((0.0, 0.0));
}

pub fn zoom_selection(s: &mut Studio) {
    let Some(show) = s.show() else { return };
    if s.sel.is_empty() {
        return;
    }
    let a = show.cues[*s.sel.first().unwrap()].time;
    let last = *s.sel.iter().last().unwrap();
    let b = show.cues.get(last + 1).map(|c| c.time).unwrap_or(s.duration());
    s.prev_view = Some((s.target_pps, s.scroll_t));
    let span = (b - a).max(0.25);
    s.target_pps = ((s.lane_w * 0.9) / span as f32).clamp(PPS_MIN, PPS_MAX);
    s.zoom_anchor = Some((a - span * 0.05, 0.0));
}

fn section_name(a: &Analysis, i: usize) -> String {
    let s = &a.sections[i];
    let n = a.sections.len();
    if s.kind == SectionKind::Silence {
        return "Silence".into();
    }
    if i == 0 && s.kind != SectionKind::Intense {
        return "Intro".into();
    }
    if i + 1 == n && s.kind != SectionKind::Intense {
        return "Outro".into();
    }
    if s.starts_with_drop {
        return "Drop".into();
    }
    match s.kind {
        SectionKind::Calm => "Breakdown".into(),
        SectionKind::Medium => "Verse".into(),
        SectionKind::Intense => "Chorus".into(),
        SectionKind::Silence => "Silence".into(),
    }
}

/// Snap with candidates (grid, section starts, drops, playhead, loop edges), radius 8 px.
fn snap_drag(s: &Studio, t: f64, alt: bool) -> (f64, Option<String>) {
    if alt || !s.snap_on || s.snap == 0 {
        return (t, None);
    }
    let Some(track) = &s.track else { return (t, None) };
    let a = &track.analysis;
    let radius = 8.0 / s.pps as f64;
    let mut cands: Vec<f64> = a.sections.iter().map(|x| x.start).collect();
    cands.extend(a.drops.iter().map(|d| d.time));
    cands.push(s.position());
    if let Some((l0, l1)) = s.loop_region {
        cands.push(l0);
        cands.push(l1);
    }
    let best = cands.into_iter().filter(|c| (c - t).abs() <= radius).min_by(|x, y| (x - t).abs().total_cmp(&(y - t).abs()));
    let out = best.or_else(|| s.grid_snap(t)).unwrap_or(t);
    let label = a
        .beat_position(out)
        .map(|b| format!("Bar {} · beat {} · {}", b.bar + 1, b.beat_in_bar + 1, fmt_timecode(out)))
        .unwrap_or_else(|| fmt_timecode(out));
    (out, Some(label))
}

// Lane indices into `LANE_H` / `View::lanes`.
const SECTIONS: usize = 0;
const WAVEFORM: usize = 1;
const BASS: usize = 2;
const TREBLE: usize = 4;
const CUES: usize = 5;
const ONE_SHOTS: usize = 6;

/// One frame's timeline geometry: the time ↔ x mapping and the lane rects.
struct View {
    rect: Rect,
    lane_left: f32,
    lane_w: f32,
    pps: f32,
    scroll_t: f64,
    ruler: Rect,
    body: Rect,
    lanes: Vec<Rect>,
    collapsed: [bool; 7],
    /// How far the lanes overflow the available height (vertical scroll range).
    overflow: f32,
}

impl View {
    fn tx(&self, t: f64) -> f32 {
        self.lane_left + ((t - self.scroll_t) as f32) * self.pps
    }
    fn xt(&self, x: f32) -> f64 {
        self.scroll_t + ((x - self.lane_left) / self.pps) as f64
    }
}

/// Beat-grid density for the current zoom.
struct Grid {
    bar_every: u32,
    show_beats: bool,
    show_sub: bool,
}

impl Grid {
    fn new(bpm: f32, pps: f32) -> Self {
        let beat_px = 60.0 / bpm.max(1.0) * pps;
        let bar_px = beat_px * 4.0;
        let bar_every = [1, 2, 4, 8].into_iter().find(|k| bar_px * *k as f32 >= 40.0).unwrap_or(16);
        Self { bar_every, show_beats: beat_px >= 12.0, show_sub: beat_px / 4.0 >= 10.0 }
    }
}

/// Peak absolute sample over the whole waveform (normalisation reference).
fn waveform_peak(a: &Analysis) -> f32 {
    a.waveform.buckets.iter().fold(0.01f32, |m, b| m.max(b.max.abs()).max(b.min.abs()))
}

pub fn show(app: &mut App, ui: &mut Ui, rect: Rect) {
    let compact = app.compact;
    let head_w = if compact { 32.0 } else { 120.0 };
    let s = &mut app.studio;
    let Some(track) = s.track.clone() else { return };
    let a = track.analysis.clone();
    let dur = a.duration.max(0.1);
    let v = update_view(s, ui, rect, head_w, app.persist.lanes_collapsed, app.persist.follow_continuous, dur);
    let grid = Grid::new(a.bpm, v.pps);
    let beats = a.beats_in(v.xt(v.lane_left) - 1.0, v.xt(rect.right()) + 1.0);

    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::ZERO, BG);
    let pb = p.with_clip_rect(v.body);
    // Lane bodies (sunken wells).
    for (i, r) in v.lanes.iter().enumerate() {
        pb.rect_filled(*r, CornerRadius::ZERO, if i == CUES { SURFACE_1 } else { BG_SUNKEN });
        pb.hline(r.x_range(), r.bottom() + 0.5, Stroke::new(1.0, BORDER_SUBTLE));
    }
    paint_grid(&pb, &v, &a, beats, &grid);
    if !v.collapsed[SECTIONS] {
        paint_sections(&pb, &v, &a);
    }
    // Drops through energy lanes.
    for d in &a.drops {
        pb.vline(v.tx(d.time), v.lanes[BASS].top()..=v.lanes[TREBLE].bottom(), Stroke::new(2.0, theme::alpha(ACCENT, 0.4)));
    }
    if !v.collapsed[WAVEFORM] {
        paint_waveform(&pb, &v, &a, dur, |t| s.loop_on && s.loop_region.is_some_and(|(l0, l1)| t < l0 || t > l1));
    }
    paint_bands(&pb, &v, &a, dur, beats);

    let pointer = ui.input(|i| i.pointer.hover_pos());
    s.hover_t = pointer.map(|pp| v.xt(pp.x));
    let show = s.show().cloned().unwrap_or_default();
    // While moving cues: is the drop target refused (landing on another cue's start)?
    let drag_invalid: Option<bool> = matches!(s.drag, Drag::MoveCues { .. }).then(|| {
        show.cues.iter().enumerate().filter(|(i, _)| !s.sel.contains(i)).any(|(_, c)| {
            s.sel.iter().any(|i| ((show.cues[*i].time + s.drag_dt) - c.time).abs() < 1e-3)
        })
    });
    let cue_rects = if v.collapsed[CUES] { Vec::new() } else { paint_cues(&pb, ui, &v, s, &show, dur, pointer, drag_invalid) };
    let ev_rects = if v.collapsed[ONE_SHOTS] { Vec::new() } else { paint_events(&pb, &v, s, &show, pointer) };
    paint_ruler(&p, &v, s, beats, &grid);
    paint_overlays(&p, ui, &v, s, pointer);

    lane_headers(app, ui, &v, head_w, compact);
    let s = &mut app.studio;
    let hits = Hits { cues: cue_rects, events: ev_rects };
    handle_input(ui, &v, s, &show, dur, pointer, &hits, drag_invalid.unwrap_or(false));
    if s.is_playing() || s.drag != Drag::None {
        ui.ctx().request_repaint();
    }
}

/// Smooth zoom, playhead follow and scroll clamping; then lay out the lanes.
fn update_view(s: &mut Studio, ui: &Ui, rect: Rect, head_w: f32, collapsed: [bool; 7], follow_continuous: bool, dur: f64) -> View {
    let lane_left = rect.left() + head_w;
    let lane_w = (rect.width() - head_w).max(50.0);
    s.lane_w = lane_w;
    if s.fit_pending {
        s.fit_pending = false;
        zoom_fit(s, Some(lane_w));
        s.pps = s.target_pps;
        s.scroll_t = 0.0;
        s.zoom_anchor = None;
        s.prev_view = None;
    }
    // Smooth zoom (80 ms) anchored on the pointer/playhead.
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    if (s.pps - s.target_pps).abs() > 0.01 {
        let k = 1.0 - (-dt / 0.08).exp();
        s.pps += (s.target_pps - s.pps) * k;
        if (s.pps - s.target_pps).abs() < 0.02 * s.target_pps.max(1.0) * 0.01 {
            s.pps = s.target_pps;
        }
        ui.ctx().request_repaint();
    } else {
        s.pps = s.target_pps;
    }
    if let Some((t, x)) = s.zoom_anchor {
        s.scroll_t = t - (x / s.pps) as f64;
        if s.pps == s.target_pps {
            s.zoom_anchor = None;
        }
    }
    // Follow playhead.
    let pos = s.position();
    if s.follow && s.is_playing() && s.drag == Drag::None {
        let px = (pos - s.scroll_t) as f32 * s.pps;
        if follow_continuous {
            s.scroll_t = pos - (lane_w * 0.33 / s.pps) as f64;
        } else if px > lane_w * 0.85 || px < 0.0 {
            s.scroll_t = pos - (lane_w * 0.1 / s.pps) as f64;
        }
    }
    let view_secs = (lane_w / s.pps) as f64;
    s.scroll_t = s.scroll_t.clamp(-view_secs * 0.05, (dur - view_secs * 0.5).max(0.0));

    // Vertical layout (with scrolling when lanes overflow).
    let mut heights: Vec<f32> = (0..7).map(|i| if collapsed[i] { 12.0 } else { LANE_H[i] }).collect();
    let avail_h = rect.height() - RULER_H;
    let spare = avail_h - (heights.iter().sum::<f32>() + 7.0);
    if spare > 0.0 && !collapsed[WAVEFORM] {
        heights[WAVEFORM] += spare.min(96.0);
    }
    let total_h: f32 = heights.iter().sum::<f32>() + 7.0;
    let overflow = (total_h - avail_h).max(0.0);
    s.v_scroll = s.v_scroll.clamp(0.0, overflow);
    let mut lanes = Vec::with_capacity(heights.len());
    let mut y = rect.top() + RULER_H - s.v_scroll;
    for h in &heights {
        lanes.push(Rect::from_min_max(pos2(lane_left, y), pos2(rect.right(), y + h)));
        y += h + 1.0;
    }
    let ruler = Rect::from_min_max(pos2(lane_left, rect.top()), pos2(rect.right(), rect.top() + RULER_H));
    let body = Rect::from_min_max(pos2(lane_left, ruler.bottom()), rect.max);
    View { rect, lane_left, lane_w, pps: s.pps, scroll_t: s.scroll_t, ruler, body, lanes, collapsed, overflow }
}

/// Beat grid at adaptive density, plus section boundaries.
fn paint_grid(pb: &egui::Painter, v: &View, a: &Analysis, beats: &[Beat], grid: &Grid) {
    let body_y = v.body.y_range();
    for (k, b) in beats.iter().enumerate() {
        let x = v.tx(b.time);
        if b.is_downbeat() && b.bar % grid.bar_every == 0 {
            pb.vline(x, body_y, Stroke::new(1.0, BORDER));
        } else if grid.show_beats {
            pb.vline(x, body_y, Stroke::new(1.0, GRID));
        }
        if grid.show_sub {
            let next = beats.get(k + 1).map(|n| n.time).unwrap_or(b.time + 60.0 / a.bpm.max(1.0) as f64);
            for q in 1..4 {
                let xs = v.tx(b.time + (next - b.time) * q as f64 / 4.0);
                pb.vline(xs, body_y, Stroke::new(1.0, theme::alpha(GRID, 0.5)));
            }
        }
    }
    for sec in &a.sections {
        let x = v.tx(sec.start);
        if x > v.lane_left && x < v.rect.right() {
            pb.vline(x, body_y, Stroke::new(1.0, theme::alpha(VIZ_SECTION, 0.6)));
        }
    }
}

fn paint_sections(pb: &egui::Painter, v: &View, a: &Analysis) {
    let r = v.lanes[SECTIONS];
    for (i, sec) in a.sections.iter().enumerate() {
        let x0 = v.tx(sec.start).max(v.lane_left - 4.0);
        let x1 = v.tx(sec.end).min(v.rect.right() + 4.0);
        if x1 < v.lane_left || x0 > v.rect.right() {
            continue;
        }
        let br = Rect::from_min_max(pos2(x0 + 1.0, r.top() + 2.0), pos2(x1 - 1.0, r.bottom() - 2.0));
        pb.rect_filled(br, CornerRadius::same(R_XS), SURFACE_3);
        pb.rect_filled(Rect::from_min_size(br.min, vec2(2.0, br.height())), CornerRadius::ZERO, VIZ_SECTION);
        if br.width() > 40.0 {
            pb.with_clip_rect(br.intersect(v.body)).text(pos2(br.left() + 8.0, br.center().y), Align2::LEFT_CENTER, section_name(a, i), theme::font("small"), TEXT_SECONDARY);
        }
    }
    for d in &a.drops {
        widgets::paint_icon(pb, pos2(v.tx(d.time), r.center().y), icons::LIGHTNING, 14.0, ACCENT);
    }
}

fn paint_waveform(pb: &egui::Painter, v: &View, a: &Analysis, dur: f64, loop_dim: impl Fn(f64) -> bool) {
    let r = v.lanes[WAVEFORM];
    let tw0 = v.xt(v.lane_left).max(0.0);
    let tw1 = v.xt(v.rect.right()).min(dur);
    let buckets = a.waveform.buckets_in(tw0, tw1, (v.lane_w as usize).max(1));
    let mid = r.center().y;
    let half = r.height() * 0.46;
    let mut env = Mesh::default();
    let mut core = Mesh::default();
    let x_start = v.tx(tw0);
    let n = buckets.len().max(1);
    let colw = (v.tx(tw1) - x_start) / n as f32;
    // Normalise to the loudest bucket so quiet masters still fill the lane.
    let peak = waveform_peak(a);
    for (k, b) in buckets.iter().enumerate() {
        let x = x_start + k as f32 * colw;
        let t = tw0 + (k as f64 + 0.5) / n as f64 * (tw1 - tw0);
        let dim = if loop_dim(t) { 0.35 } else { 1.0 };
        // True (asymmetric) min/max envelope normalised to the track peak with
        // headroom, RMS core on top. Nothing is clamped, so loud parts keep their shape.
        let top = (b.max.max(0.0) / peak) * half * 0.92;
        let bot = (-b.min.min(0.0) / peak) * half * 0.92;
        let rms = (b.rms / peak) * half * 0.92;
        let c_env = theme::alpha(VIZ_LEVEL, 0.45 * dim);
        let c_core = theme::alpha(VIZ_LEVEL, 0.80 * dim);
        env.add_colored_rect(Rect::from_min_max(pos2(x, mid - top), pos2(x + colw.max(1.0), mid + bot)), c_env);
        core.add_colored_rect(Rect::from_min_max(pos2(x, mid - rms.min(top)), pos2(x + colw.max(1.0), mid + rms.min(bot))), c_core);
    }
    pb.add(Shape::mesh(env));
    pb.add(Shape::mesh(core));
}

/// Bass / mid / treble lanes: per-2-px features as filled areas.
fn paint_bands(pb: &egui::Painter, v: &View, a: &Analysis, dur: f64, beats: &[Beat]) {
    let step = 2usize;
    let cols = (v.lane_w as usize).max(1);
    let samples: Vec<(f32, [f32; 3])> = (0..cols / step + 1)
        .map(|k| {
            let x = v.lane_left + (k * step) as f32;
            let t = v.xt(x);
            if t < 0.0 || t > dur {
                (x, [0.0; 3])
            } else {
                let f = a.features_at(t);
                (x, [f.bass, f.mid, f.treble])
            }
        })
        .collect();
    for (band, col) in [VIZ_BASS, VIZ_MID, VIZ_TREBLE].into_iter().enumerate() {
        let lane = BASS + band;
        if v.collapsed[lane] {
            continue;
        }
        let r = v.lanes[lane];
        let line: Vec<Pos2> = samples
            .iter()
            .map(|(x, f)| pos2(*x, r.bottom() - f[band].clamp(0.0, 1.0) * (r.height() - 3.0)))
            .collect();
        let fill = theme::alpha(col, 0.35);
        pb.add(Shape::mesh(widgets::fill_under(&line, r.bottom(), fill, fill)));
        pb.add(Shape::line(line, Stroke::new(1.0, theme::alpha(col, 0.9))));
        if lane == BASS {
            for b in beats {
                pb.vline(v.tx(b.time), r.top()..=(r.top() + 4.0), Stroke::new(1.0, theme::alpha(VIZ_BEAT, 0.5)));
            }
        }
    }
}

/// Paint the cue blocks (and dragged copies); returns each visible cue's rect.
#[allow(clippy::too_many_arguments)]
fn paint_cues(pb: &egui::Painter, ui: &Ui, v: &View, s: &Studio, show: &Show, dur: f64, pointer: Option<Pos2>, drag_invalid: Option<bool>) -> Vec<(usize, Rect)> {
    let lane = v.lanes[CUES];
    let block = |x0: f32, x1: f32| Rect::from_min_max(pos2(x0, lane.top() + 4.0), pos2(x1.max(x0 + 2.0), lane.bottom() - 4.0));
    let end_of = |i: usize| show.cues.get(i + 1).map(|n| n.time).unwrap_or(dur);
    let pc = pb.with_clip_rect(lane.intersect(v.body));
    let now = Instant::now();
    let gen_t = s.generated_at.map(|g| now.duration_since(g).as_secs_f32() / 0.6);
    let mut rects = Vec::new();
    for (i, c) in show.cues.iter().enumerate() {
        let mut x0 = v.tx(c.time);
        let mut x1 = v.tx(end_of(i)) - 1.0;
        if let Some((ii, at)) = s.inserted_at
            && ii == i {
                let k = (now.duration_since(at).as_secs_f32() / MOTION_BASE).min(1.0);
                x1 = x0 + (x1 - x0) * egui::emath::easing::cubic_out(k);
                if k < 1.0 {
                    ui.ctx().request_repaint();
                }
            }
        if x1 < v.lane_left - 2.0 || x0 > v.rect.right() + 2.0 {
            continue;
        }
        x0 = x0.max(v.lane_left - 8.0);
        x1 = x1.min(v.rect.right() + 8.0);
        let br = block(x0, x1);
        rects.push((i, br));
        let selected = s.sel.contains(&i);
        let hovered = pointer.is_some_and(|pp| br.contains(pp)) && s.drag == Drag::None;
        let ghost = drag_invalid.is_some() && selected;
        draw_cue(&pc, ui, br, c, selected && !ghost, hovered, if ghost { 0.4 } else { 1.0 }, None);
        if let Some(g) = gen_t
            && g < 1.0 && s.generated.contains(&key_t(c.time)) {
                pc.rect_stroke(br, CornerRadius::same(R_SM), Stroke::new(1.0, theme::alpha(ACCENT_TEXT, 1.0 - g)), StrokeKind::Outside);
                ui.ctx().request_repaint();
            }
    }
    // Dragged copies.
    if let Some(invalid) = drag_invalid {
        let ddt = s.drag_dt;
        for &i in &s.sel {
            let Some(c) = show.cues.get(i) else { continue };
            let br = block(v.tx(c.time + ddt), v.tx(end_of(i) + ddt) - 1.0);
            draw_cue(&pc, ui, br, c, true, false, 1.0, invalid.then_some(ERROR));
        }
    }
    rects
}

/// Paint the one-shot diamonds; returns each visible event's hit rect.
fn paint_events(pb: &egui::Painter, v: &View, s: &Studio, show: &Show, pointer: Option<Pos2>) -> Vec<(usize, Rect)> {
    let lane = v.lanes[ONE_SHOTS];
    let pe = pb.with_clip_rect(lane.intersect(v.body));
    let mut rects = Vec::new();
    for (i, e) in show.events.iter().enumerate() {
        let mut t = e.time;
        if let Drag::MoveEvent { idx, .. } = s.drag
            && idx == i {
                t += s.drag_dt;
            }
        let x = v.tx(t);
        if x < v.lane_left - 10.0 || x > v.rect.right() + 10.0 {
            continue;
        }
        let c = pos2(x, lane.center().y);
        let r = Rect::from_center_size(c, vec2(16.0, 16.0));
        rects.push((i, r));
        let sel = s.sel_events.contains(&i);
        let hov = pointer.is_some_and(|pp| r.contains(pp));
        let pts = vec![c + vec2(0.0, -6.0), c + vec2(6.0, 0.0), c + vec2(0.0, 6.0), c + vec2(-6.0, 0.0)];
        let (fill, stroke, label) = match e.kind {
            OneShotKind::Flash { .. } => (TEXT, Stroke::NONE, "Flash"),
            OneShotKind::Blackout => (BG, Stroke::new(1.5, TEXT), "Blackout"),
            OneShotKind::Strobe { .. } => (ACCENT_TEXT, Stroke::NONE, "Strobe"),
        };
        if sel {
            widgets::soft_glow(&pe, c, 14.0, theme::alpha(ACCENT, 0.5));
        }
        pe.add(Shape::convex_polygon(pts.clone(), fill, stroke));
        if hov || sel {
            let mut closed = pts.clone();
            closed.push(pts[0]);
            pe.add(Shape::line(closed, Stroke::new(1.0, if sel { TEXT } else { BORDER_STRONG })));
        }
        let w = e.duration * v.pps;
        if w > 10.0 {
            pe.hline((x + 7.0)..=(x + w), c.y, Stroke::new(1.0, theme::alpha(if fill == BG { TEXT } else { fill }, 0.5)));
        }
        let next_x = show.events.get(i + 1).map(|n| v.tx(n.time)).unwrap_or(f32::MAX);
        if next_x - x > 70.0 {
            pe.text(pos2(x + 10.0, c.y), Align2::LEFT_CENTER, label, theme::font("small"), TEXT_MUTED);
        }
    }
    rects
}

/// Ruler: loop band, bar numbers and beat ticks (seconds ticks without a beat grid).
fn paint_ruler(p: &egui::Painter, v: &View, s: &Studio, beats: &[Beat], grid: &Grid) {
    let ruler = v.ruler;
    p.rect_filled(ruler, CornerRadius::ZERO, SURFACE_1);
    p.hline(ruler.x_range(), ruler.bottom() - 0.5, Stroke::new(1.0, BORDER_SUBTLE));
    let pr = p.with_clip_rect(ruler);
    if let Some((l0, l1)) = s.loop_region {
        let a0 = v.tx(l0);
        let a1 = v.tx(l1);
        let band = Rect::from_min_max(pos2(a0, ruler.top() + 2.0), pos2(a1, ruler.top() + 14.0));
        pr.rect_filled(band, CornerRadius::same(2), if s.loop_on { ACCENT_SOFT } else { theme::alpha(SURFACE_3, 0.8) });
        let bc = if s.loop_on { ACCENT } else { BORDER_STRONG };
        pr.vline(a0 + 1.0, band.y_range(), Stroke::new(2.0, bc));
        pr.vline(a1 - 1.0, band.y_range(), Stroke::new(2.0, bc));
    }
    let label = |x: f32, s: String| pr.text(pos2(x + 3.0, ruler.top() + 13.0), Align2::LEFT_CENTER, s, theme::font("value_small"), TEXT_MUTED);
    for b in beats {
        let x = v.tx(b.time);
        if b.is_downbeat() {
            if b.bar % grid.bar_every == 0 {
                pr.vline(x, (ruler.bottom() - 6.0)..=ruler.bottom(), Stroke::new(1.0, VIZ_BEAT));
                label(x, format!("{}", b.bar + 1));
            }
        } else if grid.show_beats {
            pr.vline(x, (ruler.bottom() - 4.0)..=ruler.bottom(), Stroke::new(1.0, BORDER_STRONG));
        }
    }
    if beats.is_empty() {
        let step = if v.pps > 60.0 { 1.0 } else if v.pps > 15.0 { 5.0 } else { 15.0 };
        let mut t = (v.xt(v.lane_left) / step).floor() * step;
        while t < v.xt(v.rect.right()) {
            let x = v.tx(t);
            pr.vline(x, (ruler.bottom() - 5.0)..=ruler.bottom(), Stroke::new(1.0, BORDER_STRONG));
            label(x, crate::app::fmt_mmss(t));
            t += step;
        }
    }
}

/// Snap line + label, rubber band and the playhead.
fn paint_overlays(p: &egui::Painter, ui: &Ui, v: &View, s: &Studio, pointer: Option<Pos2>) {
    let (rect, ruler) = (v.rect, v.ruler);
    if let Some((st, label)) = &s.snap_line {
        let x = v.tx(*st);
        p.vline(x, (ruler.top())..=rect.bottom(), Stroke::new(1.0, ACCENT));
        let font = theme::font("small");
        let w = widgets::text_width(ui, label, font.clone()) + 16.0;
        let lr = Rect::from_min_size(pos2((x + 8.0).min(rect.right() - w - 4.0), ruler.bottom() + 4.0), vec2(w, 22.0));
        p.add(theme::shadow_popover().as_shape(lr, CornerRadius::same(R_SM)));
        p.rect_filled(lr, CornerRadius::same(R_SM), SURFACE_3);
        p.rect_stroke(lr, CornerRadius::same(R_SM), Stroke::new(1.0, BORDER), StrokeKind::Inside);
        p.text(lr.center(), Align2::CENTER_CENTER, label, font, TEXT_SECONDARY);
    }
    if let (Drag::Rubber { x0, y0 }, Some(pp)) = (s.drag, pointer) {
        let rr = Rect::from_two_pos(pos2(x0, y0), pp);
        p.rect_filled(rr, CornerRadius::ZERO, theme::alpha(ACCENT, 0.08));
        p.rect_stroke(rr, CornerRadius::ZERO, Stroke::new(1.0, theme::alpha(ACCENT, 0.6)), StrokeKind::Inside);
    }
    // Playhead (never eased).
    let px = v.tx(s.position());
    if px >= v.lane_left - 1.0 && px <= rect.right() {
        p.vline(px, (ruler.top() + 4.0)..=rect.bottom(), Stroke::new(1.0, ACCENT));
        let head = vec![pos2(px - 5.0, ruler.top() + 4.0), pos2(px + 5.0, ruler.top() + 4.0), pos2(px, ruler.top() + 11.0)];
        p.add(Shape::convex_polygon(head, ACCENT, Stroke::NONE));
    }
}

/// Left header column: snap/follow toggles, lane names (click collapses) and "+" buttons.
fn lane_headers(app: &mut App, ui: &mut Ui, v: &View, head_w: f32, compact: bool) {
    let (rect, ruler, lane_left) = (v.rect, v.ruler, v.lane_left);
    let s = &mut app.studio;
    let column = Rect::from_min_max(rect.min, pos2(lane_left, rect.bottom()));
    let hp = ui.painter_at(column);
    hp.rect_filled(column, CornerRadius::ZERO, SURFACE_1);
    hp.vline(lane_left - 0.5, rect.y_range(), Stroke::new(1.0, BORDER_SUBTLE));
    hp.hline(rect.left()..=lane_left, ruler.bottom() - 0.5, Stroke::new(1.0, BORDER_SUBTLE));
    // Snap/follow indicators in the ruler header.
    if !compact {
        let fr = Rect::from_min_size(pos2(rect.left() + 8.0, rect.top() + 4.0), vec2(head_w - 16.0, 24.0));
        let mut hc = ui.new_child(egui::UiBuilder::new().max_rect(fr).layout(egui::Layout::left_to_right(egui::Align::Center)));
        hc.spacing_mut().item_spacing.x = 2.0;
        if widgets::ghost_button_sized(&mut hc, icons::METRONOME, if s.snap_on { "Snap on" } else { "Snap off" }, Some("N"), 24.0, s.snap_on).clicked() {
            s.snap_on = !s.snap_on;
            if s.snap_on && s.snap == 0 {
                s.snap = 5;
            }
        }
        if widgets::ghost_button_sized(&mut hc, icons::ARROW_RIGHT, if s.follow { "Follow playhead: on" } else { "Follow playhead: off" }, Some("F"), 24.0, s.follow).clicked() {
            s.follow = !s.follow;
        }
    }
    let hp = hp.with_clip_rect(Rect::from_min_max(pos2(rect.left(), ruler.bottom()), pos2(lane_left, rect.bottom())));
    let mut toggle_lane = None;
    let mut add_cue = false;
    let mut add_marker = false;
    for (i, r) in v.lanes.iter().enumerate() {
        let hr = Rect::from_min_max(pos2(rect.left(), r.top()), pos2(lane_left - 1.0, r.bottom()));
        if hr.bottom() < ruler.bottom() || hr.top() > rect.bottom() {
            continue;
        }
        hp.hline(hr.x_range(), hr.bottom() + 0.5, Stroke::new(1.0, BORDER_SUBTLE));
        let resp = ui.interact(hr, ui.scope_id().with(("lanehdr", i)), Sense::click());
        let hov = resp.hovered();
        if v.collapsed[i] {
            if hov {
                hp.rect_filled(hr, CornerRadius::ZERO, HOVER);
            }
        } else {
            let cy = if hr.height() > 30.0 { hr.top() + 14.0 } else { hr.center().y };
            widgets::paint_icon(&hp, pos2(hr.left() + 16.0, cy), LANE_ICONS[i], 13.0, TEXT_MUTED);
            if !compact {
                hp.text(pos2(hr.left() + 30.0, cy), Align2::LEFT_CENTER, LANE_NAMES[i], theme::font("label"), if hov { TEXT_SECONDARY } else { TEXT_MUTED });
                if hov {
                    widgets::paint_icon(&hp, pos2(hr.right() - 12.0, cy), icons::CARET_DOWN, 11.0, TEXT_FAINT);
                }
                if i == CUES || i == ONE_SHOTS {
                    let br = Rect::from_center_size(pos2(hr.right() - (if hov { 32.0 } else { 14.0 }), cy), vec2(20.0, 20.0));
                    let b = ui.interact(br, ui.scope_id().with(("laneadd", i)), Sense::click());
                    hp.rect_filled(br, CornerRadius::same(R_XS), if b.hovered() { ACTIVE } else { Color32::TRANSPARENT });
                    widgets::paint_icon(&hp, br.center(), icons::PLUS, 12.0, if b.hovered() { TEXT } else { TEXT_FAINT });
                    let (tip, key) = if i == CUES { ("Insert cue at playhead", "C") } else { ("Add one-shot at playhead", "M") };
                    if b.on_hover_ui(|ui| widgets::tooltip_with_keys(ui, tip, Some(key))).clicked() {
                        if i == CUES { add_cue = true } else { add_marker = true }
                    }
                }
            }
        }
        if resp.clicked() {
            toggle_lane = Some(i);
        }
    }
    if let Some(i) = toggle_lane {
        app.persist.lanes_collapsed[i] = !app.persist.lanes_collapsed[i];
    }
    let s = &mut app.studio;
    if add_cue {
        s.insert_cue_at_playhead();
    }
    if add_marker {
        let t = s.snap_time_simple(s.position());
        s.add_marker_at(t);
    }
}

/// Hit rects of the cues and one-shots painted this frame.
struct Hits {
    cues: Vec<(usize, Rect)>,
    events: Vec<(usize, Rect)>,
}

impl Hits {
    fn cue(&self, pp: Pos2) -> Option<(usize, Rect)> {
        self.cues.iter().rev().find(|(_, r)| r.contains(pp)).copied()
    }
    fn event(&self, pp: Pos2) -> Option<usize> {
        self.events.iter().rev().find(|(_, r)| r.contains(pp)).map(|(i, _)| *i)
    }
}

/// Wheel zoom/scroll, cursor feedback, drags, clicks and the context menu.
#[allow(clippy::too_many_arguments)]
fn handle_input(ui: &mut Ui, v: &View, s: &mut Studio, show: &Show, dur: f64, pointer: Option<Pos2>, hits: &Hits, drag_invalid: bool) {
    let (rect, ruler, lane_left) = (v.rect, v.ruler, v.lane_left);
    let (cue_lane, ev_lane) = (v.lanes[CUES], v.lanes[ONE_SHOTS]);
    let area = Rect::from_min_max(pos2(lane_left, rect.top()), rect.max);
    let resp = ui.interact(area, ui.scope_id().with("timeline"), Sense::click_and_drag());
    // Wheel.
    if resp.hovered()
        && let Some(pp) = pointer {
            let (scroll, mods, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers, i.zoom_delta()));
            let x_rel = pp.x - lane_left;
            let tp = v.xt(pp.x);
            let pan = |s: &mut Studio, d: f32| {
                s.scroll_t -= (d / s.pps) as f64;
                s.zoom_anchor = None;
            };
            if zoom_delta != 1.0 {
                zoom_around(s, zoom_delta, tp, Some(x_rel));
            } else if mods.command || mods.alt {
                let d = if scroll.y != 0.0 { scroll.y } else { scroll.x };
                if d != 0.0 {
                    zoom_around(s, (d * 0.004).exp(), tp, Some(x_rel));
                }
            } else if mods.shift {
                pan(s, if scroll.x != 0.0 { scroll.x } else { scroll.y });
            } else if scroll.x != 0.0 {
                pan(s, scroll.x);
            } else if scroll.y != 0.0 {
                if v.overflow > 0.0 {
                    s.v_scroll = (s.v_scroll - scroll.y).clamp(0.0, v.overflow);
                } else {
                    pan(s, scroll.y);
                }
            }
        }
    let press = resp.interact_pointer_pos();
    let (alt, shift, ctrl) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift, i.modifiers.command));
    let near_loop_edge = |s: &Studio, x: f32| s.loop_region.and_then(|(l0, l1)| {
        if (x - v.tx(l0)).abs() < 5.0 { Some(0) } else if (x - v.tx(l1)).abs() < 5.0 { Some(1) } else { None }
    });

    // Cursor feedback.
    if let Some(pp) = pointer {
        let resize = match hits.cue(pp) {
            Some((_, r)) => (pp.x - r.left()).abs() < 6.0,
            None => ruler.contains(pp) && near_loop_edge(s, pp.x).is_some(),
        };
        if resize {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }

    if resp.drag_started_by(egui::PointerButton::Middle) {
        if let Some(pp) = press {
            s.drag = Drag::Pan { start_scroll: s.scroll_t, start_x: pp.x };
        }
    } else if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(pp) = press {
            let t = v.xt(pp.x);
            if ruler.contains(pp) {
                s.drag = match (near_loop_edge(s, pp.x), s.loop_region) {
                    (Some(k), _) => Drag::LoopEdge(k),
                    (None, Some((l0, l1))) if !shift && t > l0 && t < l1 && pp.y < ruler.top() + 15.0 => Drag::LoopMove { grab_t: t },
                    _ if shift => Drag::LoopCreate { start: s.snap_time_simple(t) },
                    _ => Drag::Scrub,
                };
            } else if cue_lane.contains(pp) && !v.collapsed[CUES] {
                if let Some((i, r)) = hits.cue(pp) {
                    if (pp.x - r.left()).abs() < 6.0 && i > 0 {
                        s.checkpoint();
                        s.drag = Drag::CueEdge { idx: i };
                        s.sel = [i].into();
                    } else {
                        if !s.sel.contains(&i) {
                            if !ctrl {
                                s.sel.clear();
                            }
                            s.sel.insert(i);
                        }
                        s.sel_events.clear();
                        s.drag = Drag::MoveCues { grab_t: t, primary: i };
                        s.drag_dt = 0.0;
                    }
                } else {
                    s.drag = Drag::Rubber { x0: pp.x, y0: pp.y };
                }
            } else if ev_lane.contains(pp) && !v.collapsed[ONE_SHOTS] {
                if let Some(i) = hits.event(pp) {
                    s.sel_events = [i].into();
                    s.sel.clear();
                    s.drag = Drag::MoveEvent { idx: i, grab_t: t };
                    s.drag_dt = 0.0;
                } else {
                    s.drag = Drag::Rubber { x0: pp.x, y0: pp.y };
                }
            } else {
                s.drag = Drag::Scrub;
            }
        }
    if resp.dragged()
        && let Some(pp) = press {
            drag_update(s, show, v.xt(pp.x), pp.x, dur, alt);
        }
    if resp.drag_stopped() {
        drag_finish(s, hits, pointer.or(press), ctrl, drag_invalid);
    }
    if resp.clicked()
        && let Some(pp) = press.or(pointer) {
            let t = v.xt(pp.x).clamp(0.0, dur);
            if ruler.contains(pp) {
                s.seek(t);
            } else if cue_lane.contains(pp) {
                s.sel_events.clear();
                match hits.cue(pp) {
                    Some((i, _)) if ctrl => {
                        if !s.sel.remove(&i) {
                            s.sel.insert(i);
                        }
                    }
                    Some((i, _)) if shift && !s.sel.is_empty() => {
                        let a0 = *s.sel.first().unwrap();
                        s.sel = (a0.min(i)..=a0.max(i)).collect();
                    }
                    Some((i, _)) => s.sel = [i].into(),
                    None => s.sel.clear(),
                }
            } else if ev_lane.contains(pp) {
                s.sel.clear();
                s.sel_events = hits.event(pp).into_iter().collect();
            } else {
                s.seek(t);
                s.clear_selection();
            }
        }
    if resp.double_clicked()
        && let Some(pp) = press.or(pointer) {
            let t = s.snap_time_simple(v.xt(pp.x).clamp(0.0, dur));
            if cue_lane.contains(pp) && hits.cue(pp).is_none_or(|(_, r)| (pp.x - r.left()).abs() > 20.0) {
                s.insert_cue_at(t);
            } else if ev_lane.contains(pp) && hits.event(pp).is_none() {
                s.add_marker_at(t);
            }
        }
    // Right-clicking an unselected cue selects it first.
    if resp.secondary_clicked()
        && let Some((i, _)) = pointer.filter(|pp| cue_lane.contains(*pp)).and_then(|pp| hits.cue(pp))
        && !s.sel.contains(&i) {
            s.sel = [i].into();
        }
    resp.context_menu(|ui| context_menu(ui, s));
}

/// Apply a pointer move (`t` / `x` under the pointer) to the active drag.
fn drag_update(s: &mut Studio, show: &Show, t_raw: f64, x: f32, dur: f64, alt: bool) {
    let t = t_raw.clamp(0.0, dur);
    match s.drag {
        Drag::Scrub => s.seek(t),
        Drag::Pan { start_scroll, start_x } => {
            s.scroll_t = start_scroll - ((x - start_x) / s.pps) as f64;
            s.zoom_anchor = None;
        }
        Drag::LoopCreate { start } => {
            let e = s.snap_time_simple(t);
            s.loop_region = Some((start.min(e), start.max(e)));
            s.loop_on = true;
        }
        Drag::LoopEdge(k) => {
            let e = s.snap_time_simple(t);
            if let Some((l0, l1)) = s.loop_region {
                s.loop_region = Some(if k == 0 { (e.min(l1 - 0.05), l1) } else { (l0, e.max(l0 + 0.05)) });
            }
        }
        Drag::LoopMove { grab_t } => {
            if let Some((l0, l1)) = s.loop_region {
                let (n0, _) = snap_drag(s, l0 + t - grab_t, alt);
                let dd = n0 - l0;
                s.loop_region = Some((l0 + dd, l1 + dd));
                s.drag = Drag::LoopMove { grab_t: grab_t + dd };
            }
        }
        Drag::MoveCues { grab_t, primary } => {
            let p0 = show.cues.get(primary).map(|c| c.time).unwrap_or(0.0);
            let (snapped, label) = snap_drag(s, (p0 + t_raw - grab_t).max(0.0), alt);
            s.drag_dt = snapped - p0;
            s.snap_line = label.map(|l| (snapped, l));
        }
        Drag::CueEdge { idx } => {
            let prev = show.cues.get(idx.wrapping_sub(1)).map(|c| c.time).unwrap_or(0.0);
            let next = show.cues.get(idx + 1).map(|c| c.time).unwrap_or(dur);
            let (snapped, label) = snap_drag(s, t, alt);
            let nt = snapped.clamp(prev + 0.01, next - 0.01);
            if let Some(c) = s.show_mut().and_then(|sh| sh.cues.get_mut(idx)) {
                c.time = nt;
            }
            s.dirty = true;
            s.snap_line = label.map(|l| (nt, l));
        }
        Drag::MoveEvent { idx, grab_t } => {
            let e0 = show.events.get(idx).map(|e| e.time).unwrap_or(0.0);
            let (snapped, label) = snap_drag(s, (e0 + t_raw - grab_t).max(0.0), alt);
            s.drag_dt = snapped - e0;
            s.snap_line = label.map(|l| (snapped, l));
        }
        _ => {}
    }
}

/// Commit the active drag (move cues/event, rubber-band select) and reset drag state.
fn drag_finish(s: &mut Studio, hits: &Hits, release: Option<Pos2>, ctrl: bool, drag_invalid: bool) {
    match s.drag {
        // A refused move (dropping onto another cue's start) is simply discarded.
        Drag::MoveCues { .. } if s.drag_dt.abs() > 1e-6 && !drag_invalid => {
            let times = s.move_selected_cues(s.drag_dt);
            s.edited.extend(times.iter().map(|t| key_t(*t)));
        }
        Drag::MoveEvent { idx, .. } if s.drag_dt.abs() > 1e-6 => {
            let ddt = s.drag_dt;
            s.checkpoint();
            if let Some(sh) = s.show_mut() {
                let t = sh.events.get(idx).map(|e| e.time + ddt).unwrap_or(0.0);
                if let Some(ni) = sh.move_event(idx, t) {
                    s.sel_events = [ni].into();
                }
            }
        }
        Drag::Rubber { x0, y0 } => {
            if let Some(pp) = release {
                let rr = Rect::from_two_pos(pos2(x0, y0), pp);
                if !ctrl {
                    s.clear_selection();
                }
                s.sel.extend(hits.cues.iter().filter(|(_, r)| r.intersects(rr)).map(|(i, _)| *i));
                s.sel_events.extend(hits.events.iter().filter(|(_, r)| r.intersects(rr)).map(|(i, _)| *i));
            }
        }
        _ => {}
    }
    s.drag = Drag::None;
    s.drag_dt = 0.0;
    s.snap_line = None;
}

fn context_menu(ui: &mut Ui, s: &mut Studio) {
    if s.sel.is_empty() {
        if ui.button(format!("{}   Insert cue here", icons::PLUS)).clicked() {
            if let Some(t) = s.hover_t {
                let t = s.snap_time_simple(t);
                s.insert_cue_at(t);
            }
            ui.close();
        }
        return;
    }
    ui.menu_button(format!("{}   Effect", icons::SPARKLE), |ui| {
        for i in 0..8u8 {
            if ui.button(format!("{}   {}", icons::EFFECT[i as usize], EFFECT_NAMES[i as usize])).clicked() {
                s.edit_selected(|c| c.effect = i);
                ui.close();
            }
        }
    });
    ui.menu_button(format!("{}   Palette", icons::PALETTE), |ui| {
        for i in 0..8u8 {
            if widgets::palette_option(ui, PALETTE_NAMES[i as usize], &PALETTES[i as usize], 180.0, 24.0, 48.0).clicked() {
                s.edit_selected(|c| c.palette = i);
                ui.close();
            }
        }
    });
    ui.menu_button(format!("{}   Brightness", icons::SUN), |ui| {
        for pct in [100u32, 80, 60, 40, 20] {
            if ui.button(format!("{pct}%")).clicked() {
                let v = (pct * 255 / 100) as u8;
                s.edit_selected(|c| c.brightness = v);
                ui.close();
            }
        }
    });
    if ui.button(format!("{}   Apply to section", icons::FLAG)).clicked() {
        apply_to_section(s);
        ui.close();
    }
    ui.separator();
    if ui.button(format!("{}   Duplicate", icons::COPY)).clicked() {
        s.duplicate();
        ui.close();
    }
    if ui.button(widgets::rich(format!("{}   Delete", icons::TRASH), "button", ERROR)).clicked() {
        s.delete_selection();
        ui.close();
    }
}

/// Copy the first selected cue's look onto every cue in its section.
fn apply_to_section(s: &mut Studio) {
    let Some(track) = &s.track else { return };
    let Some(show) = s.show() else { return };
    let Some(&i) = s.sel.first() else { return };
    let src = show.cues[i].clone();
    let Some((_, sec)) = track.analysis.section_at(src.time) else { return };
    let (a, b) = (sec.start, sec.end);
    s.checkpoint();
    if let Some(sh) = s.show_mut() {
        for c in sh.cues.iter_mut().filter(|c| c.time >= a - 1e-3 && c.time < b) {
            c.effect = src.effect;
            c.palette = src.palette;
            c.brightness = src.brightness;
            c.hue = src.hue;
            c.params = src.params;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_cue(p: &egui::Painter, ui: &Ui, r: Rect, c: &Cue, selected: bool, hovered: bool, opacity: f32, outline: Option<Color32>) {
    let pal = &PALETTES[c.palette as usize % PALETTES.len()];
    let w = r.width();
    let cr = CornerRadius::same(if w < 20.0 { R_XS } else { R_SM });
    let bright = if hovered { 0.36 } else { 0.28 };
    // Body: palette gradient darkened, over SURFACE_2.
    p.rect_filled(r, cr, theme::alpha(SURFACE_2, opacity));
    let clip = p.with_clip_rect(r.intersect(p.clip_rect()));
    let colors = pal.map(|s| theme::alpha(theme::mix(SURFACE_2, widgets::rgb32(s), bright), opacity));
    let mesh = widgets::h_gradient(r, &colors);
    // Round the gradient by insetting 1 px (corners keep the body colour).
    clip.with_clip_rect(r.shrink2(vec2(1.0, 0.0))).add(Shape::mesh(mesh));
    // Left-to-right text scrim.
    let mut scrim = Mesh::default();
    let sw = 120.0f32.min(w);
    scrim.colored_vertex(r.left_top(), Color32::from_black_alpha((102.0 * opacity) as u8));
    scrim.colored_vertex(r.left_bottom(), Color32::from_black_alpha((102.0 * opacity) as u8));
    scrim.colored_vertex(pos2(r.left() + sw, r.top()), Color32::TRANSPARENT);
    scrim.colored_vertex(pos2(r.left() + sw, r.bottom()), Color32::TRANSPARENT);
    scrim.add_triangle(0, 1, 2);
    scrim.add_triangle(1, 3, 2);
    clip.add(Shape::mesh(scrim));
    // Top palette strip (6 px, full brightness).
    let strip = Rect::from_min_size(r.min, vec2(w, 6.0));
    let sclip = clip.with_clip_rect(strip.intersect(clip.clip_rect()));
    widgets::palette_strip(&sclip, strip, pal, opacity);
    // Start edge line.
    clip.vline(r.left() + 1.0, r.y_range(), Stroke::new(2.0, theme::alpha(widgets::rgb32(pal[0]), opacity)));
    // Bottom brightness bar.
    let bw = w * c.brightness as f32 / 255.0;
    clip.rect_filled(Rect::from_min_size(pos2(r.left(), r.bottom() - 3.0), vec2(bw, 3.0)), CornerRadius::ZERO, theme::alpha(TEXT, 0.5 * opacity));
    // Labels.
    let e = c.effect as usize % 8;
    if w >= 14.0 {
        let tc = theme::alpha(TEXT, opacity);
        if w < 36.0 {
            widgets::paint_icon(&clip, pos2(r.left() + w / 2.0, r.center().y), icons::EFFECT[e], 13.0, tc);
        } else {
            widgets::paint_icon(&clip, pos2(r.left() + 14.0, r.top() + 20.0), icons::EFFECT[e], 13.0, tc);
            let name = widgets::truncate(ui, EFFECT_NAMES[e], &theme::font("label"), w - 34.0);
            clip.text(pos2(r.left() + 26.0, r.top() + 20.0), Align2::LEFT_CENTER, name, theme::font("label"), tc);
            if w >= 80.0 {
                let line = format!("{} · {:.0}%", PALETTE_NAMES[c.palette as usize % 8], c.brightness as f32 / 255.0 * 100.0);
                let line = widgets::truncate(ui, &line, &theme::font("small"), w - 20.0);
                clip.text(pos2(r.left() + 8.0, r.top() + 38.0), Align2::LEFT_CENTER, line, theme::font("small"), theme::alpha(TEXT_SECONDARY, opacity));
            }
        }
    }
    if let Some(o) = outline {
        p.rect_stroke(r, cr, Stroke::new(1.5, o), StrokeKind::Inside);
    } else if selected {
        p.rect_stroke(r.expand(1.0), cr, Stroke::new(2.0, theme::alpha(ACCENT, 0.8)), StrokeKind::Outside);
        p.rect_stroke(r, cr, Stroke::new(1.5, TEXT), StrokeKind::Inside);
        let grip = Rect::from_center_size(pos2(r.left() + 2.0, r.center().y), vec2(4.0, 20.0));
        p.rect_filled(grip, CornerRadius::same(2), ACCENT);
    } else if hovered {
        p.rect_stroke(r, cr, Stroke::new(1.0, BORDER_STRONG), StrokeKind::Inside);
    }
}

/// Overview minimap in the transport bar: full-song waveform, sections, visible window.
pub fn minimap(app: &mut App, ui: &mut Ui, r: Rect, resp: &egui::Response) {
    let s = &mut app.studio;
    let Some(track) = &s.track else { return };
    let a = track.analysis.clone();
    let dur = a.duration.max(0.1);
    let p = ui.painter();
    p.rect_filled(r, CornerRadius::same(R_XS), BG_SUNKEN);
    let inner = r.shrink2(vec2(4.0, 4.0));
    let n = inner.width() as usize;
    let b = a.waveform.buckets_in(0.0, dur, n.max(1));
    let peak = waveform_peak(&a);
    let mut mesh = Mesh::default();
    let mid = inner.center().y - 2.0;
    for (k, w) in b.iter().enumerate() {
        let x = inner.left() + k as f32;
        let h = (w.max.abs().max(w.min.abs()) / peak).min(1.0) * (inner.height() * 0.42);
        mesh.add_colored_rect(Rect::from_min_max(pos2(x, mid - h), pos2(x + 1.0, mid + h)), TEXT_FAINT);
    }
    p.add(Shape::mesh(mesh));
    let tx = |t: f64| inner.left() + (t / dur) as f32 * inner.width();
    for (i, sec) in a.sections.iter().enumerate() {
        let c = if i % 2 == 0 { theme::alpha(VIZ_SECTION, 0.7) } else { theme::alpha(VIZ_SECTION, 0.35) };
        p.rect_filled(Rect::from_min_max(pos2(tx(sec.start) + 1.0, inner.bottom() - 2.0), pos2(tx(sec.end) - 1.0, inner.bottom())), CornerRadius::ZERO, c);
    }
    let v0 = s.scroll_t.max(0.0);
    let v1 = (s.scroll_t + (s.lane_w / s.pps) as f64).min(dur);
    let vr = Rect::from_min_max(pos2(tx(v0), r.top() + 1.0), pos2(tx(v1).max(tx(v0) + 3.0), r.bottom() - 1.0));
    p.rect_filled(vr, CornerRadius::same(2), Color32::from_rgba_premultiplied(10, 10, 10, 10));
    p.rect_stroke(vr, CornerRadius::same(2), Stroke::new(1.0, BORDER_STRONG), StrokeKind::Inside);
    let px = tx(s.position());
    p.vline(px, r.y_range(), Stroke::new(1.0, ACCENT));
    // Interaction: drag the window; click elsewhere jumps.
    let xt = |x: f32| ((x - inner.left()) / inner.width()) as f64 * dur;
    if resp.drag_started()
        && let Some(pp) = resp.interact_pointer_pos() {
            let t = xt(pp.x);
            if vr.contains(pp) {
                s.drag = Drag::Minimap { grab_t: t - s.scroll_t };
            } else {
                s.scroll_t = t - (v1 - v0) / 2.0;
                s.drag = Drag::Minimap { grab_t: (v1 - v0) / 2.0 };
            }
            s.zoom_anchor = None;
            s.follow = false;
        }
    if resp.dragged()
        && let (Drag::Minimap { grab_t }, Some(pp)) = (s.drag, resp.interact_pointer_pos()) {
            s.scroll_t = xt(pp.x) - grab_t;
        }
    if resp.drag_stopped() {
        s.drag = Drag::None;
    }
    if resp.clicked()
        && let Some(pp) = resp.interact_pointer_pos() {
            let t = xt(pp.x);
            s.scroll_t = t - (v1 - v0) / 2.0;
            s.zoom_anchor = None;
        }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
}
