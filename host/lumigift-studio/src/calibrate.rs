//! Latency calibration ("tap along"), for real gifts.
//!
//! The latency slider says how far ahead of the heard audio position the
//! radio frames are computed. Two tap runs measure what it must be, and the
//! listener's own tapping bias (people tap a little early or late) cancels:
//!
//! 1. **Listen**: a click track on the selected output device, lights off.
//!    Each tap gives `player position at the tap − click time` = the delay
//!    the OS does not report (e.g. Bluetooth) + the tapping bias.
//! 2. **Watch**: silence, white flashes on all gifts through the normal show
//!    output. Each tap gives `tap − moment the flash was handed to the link`
//!    = radio + render delay + the same bias.
//!
//! Latency = watch − listen: frames sent that far ahead light the gifts as
//! the sound arrives (see `Studio::tick`, `RadioPath::Hardware`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Key, Layout, Sense, Ui, vec2};
use lumigift_audio::{DecodedAudio, Player};
use lumigift_core::{Link, ShowOutput};
use lumigift_proto::Rgb;

use crate::theme::{self, *};
use crate::{icons, widgets};

const PERIOD_S: f64 = 60.0 / 90.0;
const LEAD_IN_S: f64 = 1.5;
const BEATS: usize = 16;
const SAMPLE_RATE: u32 = 48_000;
const FLASH_MS: u64 = 80;
/// Taps further than this from a beat are ignored.
const MAX_OFFSET_S: f64 = 0.25;
/// Fewer taps than this per run: no result.
const MIN_TAPS: usize = 8;

enum Step {
    Intro,
    Listen { player: Player, offsets: Vec<f64>, done_at: f64 },
    Watch { start: Instant, sent: Vec<Instant>, offsets: Vec<f64>, flash_off: Option<Instant> },
    Result { listen: Vec<f64>, watch: Vec<f64> },
    Error(String),
}

pub struct Calibration {
    step: Step,
    listen: Vec<f64>,
    device: Option<String>,
}

/// What the app should do after showing the dialog.
pub enum Outcome {
    Open,
    Closed,
    Apply(f32),
}

impl Calibration {
    pub fn new(device: Option<String>) -> Self {
        Self { step: Step::Intro, listen: Vec::new(), device }
    }

    /// The calibration owns the show output while it runs.
    pub fn drives_lights(&self) -> bool {
        matches!(self.step, Step::Watch { .. })
    }

    /// Call every frame (before the dialog) with the link to the gifts.
    pub fn drive(&mut self, link: &Link) {
        let Step::Watch { start, sent, flash_off, .. } = &mut self.step else { return };
        let now = Instant::now();
        if let Some(off) = *flash_off
            && now >= off
        {
            link.set_show(ShowOutput::Blackout);
            *flash_off = None;
        }
        let k = sent.len();
        if k < BEATS && now >= *start + Duration::from_secs_f64(LEAD_IN_S + k as f64 * PERIOD_S) {
            link.set_show(ShowOutput::Solid(vec![Rgb::new(200, 200, 200); lumigift_proto::MAX_SLOTS as usize]));
            sent.push(Instant::now());
            *flash_off = Some(now + Duration::from_millis(FLASH_MS));
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Outcome {
        ctx.request_repaint();
        let mut out = Outcome::Open;
        let r = egui::Modal::new(egui::Id::unique("latency-calibration"))
            .backdrop_color(theme::alpha(Color32::BLACK, 0.5))
            .frame(crate::app::modal_frame())
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.horizontal(|ui| {
                    widgets::paint_icon(ui.painter(), ui.cursor().min + vec2(10.0, 12.0), icons::METRONOME, 18.0, ACCENT_TEXT);
                    ui.add_space(26.0);
                    widgets::text(ui, "Calibrate latency", "title", TEXT);
                });
                ui.add_space(10.0);
                out = self.body(ui);
            });
        if r.should_close() && matches!(out, Outcome::Open) {
            out = Outcome::Closed;
        }
        if !matches!(out, Outcome::Open)
            && let Step::Listen { player, .. } = &self.step
        {
            player.pause();
        }
        out
    }

    fn body(&mut self, ui: &mut Ui) -> Outcome {
        let tapped = |ui: &mut Ui, label: &str| -> bool {
            let key = ui.input(|i| i.key_pressed(Key::Space));
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 120.0), Sense::click());
            let p = ui.painter();
            let lit = resp.is_pointer_button_down_on() || ui.input(|i| i.key_down(Key::Space));
            p.rect_filled(r, egui::CornerRadius::same(R_LG), if lit { ACCENT_SOFT } else { SURFACE_2 });
            widgets::paint_icon(p, r.center() - vec2(0.0, 14.0), icons::HAND_TAP, 28.0, if lit { ACCENT_TEXT } else { TEXT_SECONDARY });
            p.text(r.center() + vec2(0.0, 22.0), egui::Align2::CENTER_CENTER, label, theme::font("label"), TEXT_MUTED);
            key || resp.clicked()
        };
        match &mut self.step {
            Step::Intro => {
                widgets::text(ui, "Two short tap-alongs measure how far ahead the gifts must be driven so their light lands on the beat you hear. Your own tapping habit (a little early or late) cancels out.", "body", TEXT_SECONDARY);
                ui.add_space(8.0);
                widgets::text(ui, "1. Listen: tap Space on every click you hear (lights stay off).", "body", TEXT_SECONDARY);
                widgets::text(ui, "2. Watch: tap Space on every white flash of a gift (no sound).", "body", TEXT_SECONDARY);
                ui.add_space(14.0);
                let mut out = Outcome::Open;
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, Some(icons::PLAY), "Start: listen", true).clicked() {
                        self.step = match click_track(self.device.as_deref()) {
                            Ok((player, done_at)) => Step::Listen { player, offsets: Vec::new(), done_at },
                            Err(e) => Step::Error(e),
                        };
                    }
                    if widgets::secondary_button(ui, None, "Cancel", true).clicked() {
                        out = Outcome::Closed;
                    }
                });
                out
            }
            Step::Listen { player, offsets, done_at } => {
                let pos = player.position();
                widgets::text(ui, "Tap Space (or the pad) on every click you hear.", "body", TEXT_SECONDARY);
                ui.add_space(8.0);
                if tapped(ui, &format!("{} taps", offsets.len())) {
                    let beat = ((pos - LEAD_IN_S) / PERIOD_S).round();
                    let off = pos - (LEAD_IN_S + beat * PERIOD_S);
                    if (0.0..BEATS as f64).contains(&beat) && off.abs() < MAX_OFFSET_S {
                        offsets.push(off);
                    }
                }
                if pos >= *done_at {
                    player.pause();
                    self.listen = std::mem::take(offsets);
                    self.step = Step::Watch { start: Instant::now(), sent: Vec::new(), offsets: Vec::new(), flash_off: None };
                }
                Outcome::Open
            }
            Step::Watch { start, sent, offsets, .. } => {
                widgets::text(ui, "Now look at a gift and tap on every white flash.", "body", TEXT_SECONDARY);
                ui.add_space(8.0);
                if tapped(ui, &format!("{} taps", offsets.len())) {
                    let now = Instant::now();
                    if let Some(off) = sent
                        .iter()
                        .map(|s| now.duration_since(*s).as_secs_f64())
                        .filter(|o| *o < MAX_OFFSET_S + 0.2)
                        .min_by(|a, b| a.total_cmp(b))
                    {
                        offsets.push(off);
                    }
                }
                let end = *start + Duration::from_secs_f64(LEAD_IN_S + BEATS as f64 * PERIOD_S + 0.5);
                if Instant::now() >= end {
                    let watch = std::mem::take(offsets);
                    self.step = Step::Result { listen: std::mem::take(&mut self.listen), watch };
                }
                Outcome::Open
            }
            Step::Result { listen, watch } => {
                let mut out = Outcome::Open;
                match (median(listen), median(watch)) {
                    (Some(a), Some(b)) if listen.len() >= MIN_TAPS && watch.len() >= MIN_TAPS => {
                        let latency = ((b - a) * 1000.0) as f32;
                        let spread = (mad(listen) + mad(watch)) * 1000.0;
                        widgets::text(ui, format!("Latency: {latency:.0} ms"), "title", ACCENT_TEXT);
                        ui.add_space(4.0);
                        widgets::text(
                            ui,
                            format!(
                                "Light path {:.0} ms, unreported audio delay {:.0} ms (each with your tapping habit, which cancels). Tap spread ±{spread:.0} ms.",
                                b * 1000.0,
                                a * 1000.0
                            ),
                            "small",
                            TEXT_MUTED,
                        );
                        if spread > 60.0 {
                            ui.add_space(4.0);
                            widgets::text(ui, "The taps were quite irregular: consider running it again.", "small", WARN);
                        }
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if widgets::primary_button(ui, Some(icons::CHECK), "Apply", true).clicked() {
                                out = Outcome::Apply(latency.clamp(-50.0, 250.0));
                            }
                            if widgets::secondary_button(ui, None, "Again", true).clicked() {
                                self.step = Step::Intro;
                            }
                            if widgets::secondary_button(ui, None, "Cancel", true).clicked() {
                                out = Outcome::Closed;
                            }
                        });
                    }
                    _ => {
                        widgets::text(
                            ui,
                            format!(
                                "Not enough taps near the beats ({} listening, {} watching; at least {MIN_TAPS} each).",
                                listen.len(),
                                watch.len()
                            ),
                            "body",
                            WARN,
                        );
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if widgets::primary_button(ui, None, "Again", true).clicked() {
                                self.step = Step::Intro;
                            }
                            if widgets::secondary_button(ui, None, "Cancel", true).clicked() {
                                out = Outcome::Closed;
                            }
                        });
                    }
                }
                out
            }
            Step::Error(e) => {
                widgets::text(ui, format!("Could not play the click track: {e}"), "body", WARN);
                ui.add_space(14.0);
                let mut out = Outcome::Open;
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if widgets::secondary_button(ui, None, "Close", true).clicked() {
                        out = Outcome::Closed;
                    }
                });
                out
            }
        }
    }
}

/// Play a click track on its own player (the loaded song is untouched).
/// Returns the player and the position at which the run is over.
fn click_track(device: Option<&str>) -> Result<(Player, f64), String> {
    let total = LEAD_IN_S + BEATS as f64 * PERIOD_S + 0.6;
    let mut samples = vec![0f32; (total * SAMPLE_RATE as f64) as usize];
    let click_len = (0.03 * SAMPLE_RATE as f64) as usize;
    for k in 0..BEATS {
        let at = ((LEAD_IN_S + k as f64 * PERIOD_S) * SAMPLE_RATE as f64) as usize;
        for i in 0..click_len {
            let t = i as f32 / SAMPLE_RATE as f32;
            let env = (-t * 180.0).exp();
            if let Some(s) = samples.get_mut(at + i) {
                *s = 0.7 * env * (2.0 * std::f32::consts::PI * 1800.0 * t).sin();
            }
        }
    }
    let player = Player::with_device(device).map_err(|e| e.to_string())?;
    player.load(Arc::new(DecodedAudio::from_samples(samples, SAMPLE_RATE, 1)));
    player.play();
    Ok((player, total - 0.3))
}

fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    Some(s[s.len() / 2])
}

/// Median absolute deviation.
fn mad(v: &[f64]) -> f64 {
    let Some(m) = median(v) else { return 0.0 };
    median(&v.iter().map(|x| (x - m).abs()).collect::<Vec<_>>()).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_and_spread() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[0.03, -0.01, 0.02]), Some(0.02));
        assert!((mad(&[0.0, 0.01, 0.02, 0.03, 0.5]) - 0.01).abs() < 1e-9);
    }
}
