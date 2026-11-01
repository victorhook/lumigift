//! Gift health model (docs/UI_DESIGN.md §4.1): per-metric levels with
//! hysteresis (enter 3 s, exit 10 s) and the overall state per gift.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::Color32;
use lumigift_core::{GiftView, OtaProgress, OtaTargetPhase, Uid};
use lumigift_proto::{ResetReason, status_flags};

use crate::theme::*;

const ENTER: Duration = Duration::from_secs(3);
const EXIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Level {
    #[default]
    Normal,
    Warn,
    Critical,
}

impl Level {
    pub fn color(self, normal: Color32) -> Color32 {
        match self {
            Level::Normal => normal,
            Level::Warn => WARN,
            Level::Critical => ERROR,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Online,
    Attention(String),
    Critical(String),
    Reconnecting,
    Offline(Option<Duration>),
    Updating(String),
}

impl State {
    pub fn rank(&self) -> u8 {
        match self {
            State::Critical(_) => 0,
            State::Attention(_) => 1,
            State::Updating(_) => 2,
            State::Reconnecting => 3,
            State::Online => 4,
            State::Offline(_) => 5,
        }
    }
    pub fn needs_attention(&self) -> bool {
        matches!(self, State::Attention(_) | State::Critical(_))
    }
    pub fn is_offline(&self) -> bool {
        matches!(self, State::Offline(_))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Metric {
    Link,
    Retries,
    Bcast,
    Battery,
    Sync,
    Reboots,
    Boot,
    Temp,
}

#[derive(Default, Clone)]
struct Hyst {
    shown: Level,
    pending: Option<(Level, Instant)>,
}

impl Hyst {
    fn update(&mut self, raw: Level, now: Instant) -> Level {
        if raw == self.shown {
            self.pending = None;
            return self.shown;
        }
        let need = if raw > self.shown { ENTER } else { EXIT };
        match self.pending {
            Some((lvl, since)) if lvl == raw => {
                if now.duration_since(since) >= need {
                    self.shown = raw;
                    self.pending = None;
                }
            }
            _ => self.pending = Some((raw, now)),
        }
        self.shown
    }
}

/// Everything the tiles / inspector need about one gift's health.
#[derive(Clone, Debug)]
pub struct Health {
    pub state: State,
    pub link_level: Level,
    pub battery_level: Level,
    pub bcast_level: Level,
    pub vbat_median_mv: Option<f32>,
    /// One plain-language sentence for the inspector.
    pub sentence: String,
}

#[derive(Default)]
pub struct HealthTracker {
    gifts: HashMap<Uid, HashMap<Metric, Hyst>>,
    pub nimh: bool,
    /// The show stream is running (broadcast reception only matters then).
    pub show_active: bool,
}

pub fn battery_pct(v: f32, nimh: bool) -> f32 {
    let pts: &[(f32, f32)] = if nimh {
        &[(1.00, 0.0), (1.10, 10.0), (1.15, 25.0), (1.20, 50.0), (1.25, 75.0), (1.30, 100.0)]
    } else {
        &[(0.95, 0.0), (1.05, 10.0), (1.15, 25.0), (1.25, 50.0), (1.35, 75.0), (1.45, 100.0)]
    };
    if v <= pts[0].0 {
        return 0.0;
    }
    for w in pts.windows(2) {
        let (v0, p0) = w[0];
        let (v1, p1) = w[1];
        if v <= v1 {
            return p0 + (p1 - p0) * (v - v0) / (v1 - v0);
        }
    }
    100.0
}

pub fn reset_reason_words(r: ResetReason) -> &'static str {
    match r {
        ResetReason::Brownout => "Brownout (battery dipped)",
        ResetReason::Watchdog => "Watchdog (firmware hang)",
        ResetReason::PowerOn => "Power on",
        ResetReason::Software => "Software reboot",
        ResetReason::Pin => "Reset pin",
        ResetReason::LowPower => "Low-power",
        ResetReason::Unknown => "Unknown",
    }
}

fn median(mut v: Vec<f32>) -> Option<f32> {
    v.retain(|x| x.is_finite() && *x > 0.0);
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[v.len() / 2])
}

impl HealthTracker {
    pub fn evaluate(&mut self, g: &GiftView, ota: Option<&OtaProgress>, now: Instant) -> Health {
        let hy = self.gifts.entry(g.uid).or_default();
        let mut upd = |m: Metric, raw: Level| hy.entry(m).or_default().update(raw, now);

        // Battery: median of the last 30 s (60 samples at 2 Hz).
        let hist = &g.history;
        let recent: Vec<f32> = hist.iter().rev().take(60).map(|s| s.vbat_mv).collect();
        let vbat_med = median(recent).or(if g.vbat_mv > 0 { Some(g.vbat_mv as f32) } else { None });

        let link_raw = if !g.online {
            Level::Normal
        } else if g.poll_success_pct < 70.0 {
            Level::Critical
        } else if g.poll_success_pct < 90.0 {
            Level::Warn
        } else {
            Level::Normal
        };
        let retries_raw = if !g.online {
            Level::Normal
        } else if g.avg_retries > 3.0 {
            Level::Critical
        } else if g.avg_retries > 1.0 {
            Level::Warn
        } else {
            Level::Normal
        };
        let streaming = self.show_active;
        let bcast_raw = match g.bcast_rx_pct.filter(|_| streaming) {
            Some(p) if g.online && p < 85.0 => Level::Critical,
            Some(p) if g.online && p < 95.0 => Level::Warn,
            _ => Level::Normal,
        };
        let batt_raw = match vbat_med {
            Some(v) if g.online && v < 1050.0 => Level::Critical,
            Some(v) if g.online && (v < 1150.0 || g.flags & status_flags::LOW_BATTERY != 0) => Level::Warn,
            _ => Level::Normal,
        };
        let synced = g.flags & status_flags::TIME_SYNCED != 0;
        let sync_raw = if g.online
            && g.status.is_some()
            && (g.sync_err_us >= 500 || (!synced && g.uptime > Duration::from_secs(5)))
        {
            Level::Warn
        } else {
            Level::Normal
        };
        let recent_reboot = g.last_reboot.is_some_and(|t| now.duration_since(t) < Duration::from_secs(600));
        let unplanned = matches!(g.reset_reason, ResetReason::Watchdog | ResetReason::Brownout | ResetReason::Unknown);
        let reboots_raw = if recent_reboot && g.reboots_seen >= 3 && unplanned {
            Level::Critical
        } else if (recent_reboot && unplanned)
            || (g.online
                && g.uptime < Duration::from_secs(600)
                && matches!(g.reset_reason, ResetReason::Watchdog | ResetReason::Brownout))
        {
            Level::Warn
        } else {
            Level::Normal
        };
        let in_ota = ota.is_some_and(|o| {
            o.finished.is_none() && o.targets.iter().any(|t| t.uid == g.uid)
        });
        let boot_raw = if g.online && g.in_bootloader && !in_ota { Level::Critical } else { Level::Normal };
        let temp_raw = if g.online && g.temp_c > 65 {
            Level::Critical
        } else if g.online && g.temp_c > 50 {
            Level::Warn
        } else {
            Level::Normal
        };

        let link = upd(Metric::Link, link_raw);
        let retries = upd(Metric::Retries, retries_raw);
        let bcast = upd(Metric::Bcast, bcast_raw);
        let batt = upd(Metric::Battery, batt_raw);
        let sync = upd(Metric::Sync, sync_raw);
        let reboots = upd(Metric::Reboots, reboots_raw);
        // Bootloader is immediate (no debounce): it's a definite state.
        let boot = boot_raw;
        let _ = upd(Metric::Boot, boot_raw);
        let temp = upd(Metric::Temp, temp_raw);

        let v = vbat_med.unwrap_or(0.0) / 1000.0;
        let mut reasons: Vec<(Level, String, String)> = Vec::new();
        let mut push = |lvl: Level, short: String, long: String| {
            if lvl != Level::Normal {
                reasons.push((lvl, short, long));
            }
        };
        push(boot, "In recovery".into(), "In recovery mode: update the firmware to bring it back.".into());
        push(
            batt,
            if batt == Level::Critical { "Battery critical".into() } else { "Low battery".into() },
            format!("Low battery ({v:.2} V): replace the AAA cell soon."),
        );
        push(
            link,
            "Weak link".into(),
            format!("Weak radio link ({:.0}% of polls answered): move it closer to the bridge.", g.poll_success_pct),
        );
        push(
            bcast,
            "Missing frames".into(),
            format!(
                "Missing show frames ({:.0}% received): lights may stutter.",
                g.bcast_rx_pct.unwrap_or(0.0)
            ),
        );
        push(
            reboots,
            if reboots == Level::Critical { "Reboot loop".into() } else { "Rebooted".into() },
            format!("Rebooted recently: {}.", reset_reason_words(g.reset_reason)),
        );
        push(retries, "Retries".into(), "Many radio retries: interference or distance.".into());
        push(temp, "Hot".into(), format!("Running hot ({} °C).", g.temp_c));
        push(sync, "Clock drift".into(), "Clock not locked to the bridge: effects may drift.".into());
        reasons.sort_by_key(|r| std::cmp::Reverse(r.0));

        let ota_phase = ota.and_then(|o| {
            if o.finished.is_some() {
                return None;
            }
            o.targets.iter().find(|t| t.uid == g.uid).map(|t| match t.phase {
                OtaTargetPhase::Waiting => "Waiting".to_string(),
                OtaTargetPhase::EnteringBootloader => "Rebooting".into(),
                OtaTargetPhase::Erasing => "Erasing".into(),
                OtaTargetPhase::Receiving => format!("Transferring {:.0}%", t.percent),
                OtaTargetPhase::Committing => "Verifying".into(),
                OtaTargetPhase::Verified | OtaTargetPhase::Booting => "Booting".into(),
                OtaTargetPhase::Done => "Done".into(),
                OtaTargetPhase::Failed => "Failed".into(),
                OtaTargetPhase::Cancelled => "Cancelled".into(),
            })
        });

        let since_seen = g.last_seen.map(|t| now.saturating_duration_since(t));
        let (state, sentence) = if let Some(ph) = ota_phase {
            (State::Updating(ph.clone()), format!("Updating firmware: {ph}."))
        } else if !g.online {
            match since_seen {
                Some(d) if d < Duration::from_secs(5) => {
                    (State::Reconnecting, "Not answering: reconnecting…".to_string())
                }
                d => (
                    State::Offline(d),
                    "Offline: switched off or out of range. That's fine at a party.".to_string(),
                ),
            }
        } else if let Some((lvl, short, long)) = reasons.first() {
            if *lvl == Level::Critical {
                (State::Critical(short.clone()), long.clone())
            } else {
                (State::Attention(short.clone()), long.clone())
            }
        } else {
            (State::Online, "Healthy: answering polls and receiving the show.".to_string())
        };

        Health {
            state,
            link_level: link.max(retries),
            battery_level: batt,
            bcast_level: bcast,
            vbat_median_mv: vbat_med,
            sentence,
        }
    }
}

/// "41m", "3h 12m", "2d 4h".
/// Hardware revision letter: 1 → 'A', 2 → 'B', …
pub fn hw_rev_letter(r: u8) -> char {
    (b'A' + r.saturating_sub(1)) as char
}

/// Median battery voltage (V) and estimated charge (0..1).
pub fn battery_state(h: Option<&Health>, nimh: bool) -> (Option<f32>, Option<f32>) {
    let v = h.and_then(|h| h.vbat_median_mv).map(|v| v / 1000.0);
    (v, v.map(|v| battery_pct(v, nimh) / 100.0))
}

/// Show slot as shown to users: 0-based slot 2 → "#03".
pub fn fmt_slot(s: u8) -> String {
    format!("#{:02}", s as u32 + 1)
}

/// `"s"` unless `n == 1`, for `format!("{n} gift{}", plural(n))`.
pub fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

pub fn fmt_uptime(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86400, (s % 86400) / 3600)
    }
}

pub fn fmt_ago(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else {
        format!("{}h ago", s / 3600)
    }
}

/// fw_version as `YYYY-MM-DD HH:MM` (UTC) if it looks like a timestamp, else `v{n}`.
pub fn fmt_fw(v: u32) -> String {
    if v >= 1_600_000_000 {
        let (y, mo, d, h, mi) = civil(v as i64);
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
    } else {
        format!("v{v}")
    }
}

/// Short form for tiles: `10-31` or `v3`.
pub fn fmt_fw_short(v: u32) -> String {
    if v >= 1_600_000_000 {
        let (_, mo, d, _, _) = civil(v as i64);
        format!("{mo:02}-{d:02}")
    } else {
        format!("v{v}")
    }
}

fn civil(ts: i64) -> (i64, u32, u32, u32, u32) {
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    // Howard Hinnant's days_from_civil inverse.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (secs / 3600) as u32, ((secs % 3600) / 60) as u32)
}

/// Wall-clock `HH:MM:SS` (local time is not available without a tz crate; UTC offset from env TZ is ignored).
pub fn fmt_wall(t: std::time::SystemTime) -> String {
    let s = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let s = s + local_offset_secs();
    let (_, _, _, h, m) = civil(s);
    format!("{h:02}:{m:02}:{:02}", s.rem_euclid(60))
}

/// Local UTC offset, read once from `date +%z` (falls back to UTC).
fn local_offset_secs() -> i64 {
    use std::sync::OnceLock;
    static OFF: OnceLock<i64> = OnceLock::new();
    *OFF.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                let s = s.trim();
                if s.len() != 5 {
                    return None;
                }
                let sign = if s.starts_with('-') { -1 } else { 1 };
                let h: i64 = s[1..3].parse().ok()?;
                let m: i64 = s[3..5].parse().ok()?;
                Some(sign * (h * 3600 + m * 60))
            })
            .unwrap_or(0)
    })
}

/// UID grouped `3A9F 11C2 …`.
pub fn fmt_uid(u: &Uid) -> String {
    let h = u.to_hex().to_uppercase();
    h.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(" ")
}
