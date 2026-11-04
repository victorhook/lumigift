//! The offline analysis pipeline (see the module docs of [`super`]).

use std::time::Instant;

use super::dsp::{self, Frontend, N_BANDS, RawFrame};
use super::{
    ANALYSIS_VERSION, Analysis, Beat, DropEvent, ONSET_TIME_CORRECTION, Section, SectionKind,
    Spectrogram, Waveform, beat, nearest_within, onset, structure,
};
use crate::Result;
use crate::features::{FeatureFrame, SILENCE_DB};
use crate::job::JobContext;

pub(crate) fn analyze_with(mono: &[f32], sample_rate: u32, ctx: &JobContext) -> Result<Analysis> {
    let started = Instant::now();
    let fps = sample_rate as f64 / dsp::hop_for(sample_rate) as f64;
    let duration = mono.len() as f64 / sample_rate as f64;

    // 1. STFT.
    ctx.set_stage("Analysing spectrum");
    let raw = stft(mono, sample_rate, ctx)?;
    ctx.set_stage("Detecting beats");
    ctx.set_progress(0.6);

    // 2. Normalisation, and the note from the smoothed chroma.
    let (mut frames, silent) = normalise(&raw);
    let chroma: Vec<[f32; dsp::CHROMA_BINS]> = raw.iter().map(|r| r.chroma).collect();
    let short = dsp::smooth_chroma(&chroma, dsp::CHROMA_SMOOTH_HALF.0);
    let long = dsp::smooth_chroma(&chroma, dsp::CHROMA_SMOOTH_HALF.1);
    for (((f, s), l), &silent) in frames.iter_mut().zip(&short).zip(&long).zip(&silent) {
        if !silent {
            let (pc, clarity) = dsp::tone_of_scales(s, l);
            f.tone = FeatureFrame::tone_vector(pc, clarity);
        }
    }

    // 3. Onsets (vibrato-suppressed flux, adaptive threshold).
    let bands: Vec<[f32; N_BANDS]> = raw.iter().map(|r| r.bands_db).collect();
    let onsets = onset::pick(&onset::detection_function(&bands, &silent), &silent, fps);
    ctx.check_cancel()?;

    // 4./5. Tempo and beats per non-silent region.
    let flux: Vec<f32> = raw.iter().map(|r| r.flux).collect();
    let bcurve = beat::beat_onset_curve(&flux, fps);
    let tempo = beat::estimate_tempo(&bcurve, fps);
    let period = &tempo.period;
    ctx.set_progress(0.75);
    ctx.check_cancel()?;
    let regions = beat::active_regions(&silent, (1.0 * fps) as usize, (3.0 * fps) as usize);
    let (beats, bpm) = track_regions(&regions, &bcurve, period, &raw, &frames, &bands, fps);
    ctx.set_progress(0.85);
    ctx.check_cancel()?;

    // Rhythm confidence (pulse clarity) over time and for the whole track.
    let rhythm_hop = tempo.hop as f64 / fps;
    let coverage = beat_coverage(&beats, &onsets, tempo.clarity.len(), rhythm_hop);
    let rhythm = if beats.is_empty() {
        vec![0.0; tempo.clarity.len()]
    } else {
        beat::rhythm_confidence(&tempo, &coverage, &silent)
    };
    let rhythm_confidence = track_rhythm(&rhythm, &silent, tempo.hop);

    // 6. Drops and sections.
    ctx.set_stage("Finding sections");
    let energy = structure::energy_curve(&frames);
    let beat_times: Vec<f64> = beats.iter().map(|b| b.time).collect();
    let snap_to_beat = |t: f64, tol: f64| nearest_within(&beat_times, t, tol).unwrap_or(t);
    let drops: Vec<DropEvent> = structure::detect_drops(&energy, fps)
        .into_iter()
        .map(|d| DropEvent {
            time: snap_to_beat(d.frame / fps, 0.15),
            strength: d.strength,
        })
        .collect();

    // Novelty boundaries away from drops, snapped to a nearby downbeat (or beat).
    let bar_len = if bpm > 0.0 {
        4.0 * 60.0 / bpm as f64
    } else {
        2.0
    };
    let downbeat_times: Vec<f64> = beats
        .iter()
        .filter(|b| b.is_downbeat())
        .map(|b| b.time)
        .collect();
    let mut bounds: Vec<f64> = structure::section_boundaries(&frames, &bands, fps, 6.0)
        .into_iter()
        .map(|f| f as f64 / fps)
        .filter(|&t| drops.iter().all(|d| (d.time - t).abs() > 4.0))
        .map(|t| {
            nearest_within(&downbeat_times, t, bar_len / 2.0)
                .unwrap_or_else(|| snap_to_beat(t, bar_len / 8.0))
        })
        .collect();
    bounds.extend(drops.iter().map(|d| d.time));
    for &(a, b) in &regions {
        for t in [a as f64 / fps, b as f64 / fps] {
            if t > 1.0 && t < duration - 1.0 && bounds.iter().all(|&x| (x - t).abs() > 2.0) {
                bounds.push(t);
            }
        }
    }
    bounds.sort_by(f64::total_cmp);
    bounds.dedup_by(|a, b| (*a - *b).abs() < 2.0);
    let mut sections = build_sections(&bounds, duration, &frames, &energy, &drops, fps);
    for s in &mut sections {
        let a = (s.start / rhythm_hop).round() as usize;
        let b = ((s.end / rhythm_hop).round() as usize + 1).min(rhythm.len());
        s.rhythm = if b > a { dsp::mean(&rhythm[a..b]) } else { 0.0 };
    }

    // 7. Overviews.
    ctx.set_stage("Building overview");
    let waveform = Waveform::from_mono(mono, sample_rate);
    let spectrogram = build_spectrogram(&bands, &silent, fps, sample_rate);
    ctx.set_progress(1.0);

    Ok(Analysis {
        version: ANALYSIS_VERSION,
        content_hash: String::new(),
        sample_rate,
        duration,
        fps,
        frames,
        onsets,
        bpm,
        beats,
        sections,
        drops,
        rhythm,
        rhythm_hop,
        rhythm_confidence,
        waveform,
        spectrogram,
        analysis_seconds: started.elapsed().as_secs_f32(),
    })
}

/// Raw spectral frames centred on `i * hop` (zero-padded at both ends).
/// Reports progress 0..0.6.
fn stft(mono: &[f32], sample_rate: u32, ctx: &JobContext) -> Result<Vec<RawFrame>> {
    let mut fe = Frontend::new(sample_rate);
    let hop = dsp::hop_for(sample_rate);
    let win = fe.window_len;
    let n_frames = if mono.is_empty() {
        0
    } else {
        mono.len() / hop + 1
    };
    let mut raw = Vec::with_capacity(n_frames);
    let mut buf = vec![0f32; win];
    for i in 0..n_frames {
        let start = (i * hop) as isize - (win / 2) as isize;
        if start >= 0 && start as usize + win <= mono.len() {
            buf.copy_from_slice(&mono[start as usize..start as usize + win]);
        } else {
            for (j, b) in buf.iter_mut().enumerate() {
                let k = start + j as isize;
                *b = if k >= 0 && (k as usize) < mono.len() {
                    mono[k as usize]
                } else {
                    0.0
                };
            }
        }
        raw.push(fe.process(&buf));
        if i % 2000 == 0 {
            ctx.check_cancel()?;
            ctx.set_progress(0.6 * i as f32 / n_frames.max(1) as f32);
        }
    }
    Ok(raw)
}

/// Track-adaptive normalisation into [`FeatureFrame`]s, plus the per-frame
/// silence mask.
fn normalise(raw: &[RawFrame]) -> (Vec<FeatureFrame>, Vec<bool>) {
    // Silence gate: absolute, but lowered for very quiet masters.
    let mut loud: Vec<f32> = raw
        .iter()
        .map(|r| r.rms_db)
        .filter(|&d| d > -100.0)
        .collect();
    let gate = SILENCE_DB
        .min(dsp::percentile(&mut loud, 95.0) - 40.0)
        .max(-80.0);
    let silent: Vec<bool> = raw.iter().map(|r| r.rms_db < gate).collect();
    let active = |get: fn(&RawFrame) -> f32| -> Vec<f32> {
        raw.iter()
            .zip(&silent)
            .filter(|(_, s)| !**s)
            .map(|(r, _)| get(r))
            .collect()
    };
    let range = |get: fn(&RawFrame) -> f32| -> (f32, f32) {
        let mut v = active(get);
        if v.is_empty() {
            return (-60.0, 0.0);
        }
        let hi = dsp::percentile(&mut v, 97.0);
        let p5 = dsp::percentile(&mut v, 5.0);
        let lo = p5
            .max(hi - dsp::NORM_RANGE_DB)
            .min(hi - dsp::NORM_MIN_RANGE_DB);
        (lo, hi)
    };
    let (nb, nm, nt, nl) = (
        range(|r| r.bass_db),
        range(|r| r.mid_db),
        range(|r| r.treble_db),
        range(|r| r.rms_db),
    );
    let flux_ref = dsp::percentile(&mut active(|r| r.flux), 99.0).max(1e-3);
    let frames = raw
        .iter()
        .zip(&silent)
        .map(|(r, &s)| {
            if s {
                FeatureFrame {
                    rms_db: r.rms_db,
                    ..Default::default()
                }
            } else {
                FeatureFrame {
                    bass: dsp::map_db(r.bass_db, nb.0, nb.1),
                    mid: dsp::map_db(r.mid_db, nm.0, nm.1),
                    treble: dsp::map_db(r.treble_db, nt.0, nt.1),
                    level: dsp::map_db(r.rms_db, nl.0, nl.1),
                    onset: (r.flux / flux_ref).min(1.0),
                    rms_db: r.rms_db,
                    tone: [0.0, 0.0],
                }
            }
        })
        .collect();
    (frames, silent)
}

/// Beat tracking and bar phase per active region. Returns the beats and the
/// dominant tempo (median inter-beat interval; 0 if there are no beats).
fn track_regions(
    regions: &[(usize, usize)],
    bcurve: &[f32],
    period: &[f32],
    raw: &[RawFrame],
    frames: &[FeatureFrame],
    bands: &[[f32; N_BANDS]],
    fps: f64,
) -> (Vec<Beat>, f32) {
    let bass_flux: Vec<f32> = raw.iter().map(|r| r.bass_flux).collect();
    let bass: Vec<f32> = frames.iter().map(|f| f.bass).collect();
    let mut beats: Vec<Beat> = Vec::new();
    let mut all_ibis: Vec<f32> = Vec::new();
    let mut bar: u32 = 0;
    for &(a, b) in regions {
        let bf = beat::track_beats(bcurve, period, a, b);
        if bf.len() < 2 {
            continue;
        }
        let times: Vec<f64> = bf.iter().map(|f| f / fps + ONSET_TIME_CORRECTION).collect();
        all_ibis.extend(times.windows(2).map(|w| (w[1] - w[0]) as f32));
        let frames_idx: Vec<usize> = bf
            .iter()
            .map(|f| (f.round() as usize).min(raw.len() - 1))
            .collect();
        let phase = beat::downbeat_phase(&frames_idx, &bass_flux, &bass, bands);
        let bpms = beat::local_bpm(&times);
        for (i, (&time, &bpm)) in times.iter().zip(&bpms).enumerate() {
            let beat_in_bar = ((i + 4 - phase) % 4) as u8;
            // Each region (after silence) starts a new bar.
            if !beats.is_empty() && (i == 0 || beat_in_bar == 0) {
                bar += 1;
            }
            beats.push(Beat {
                time,
                beat_in_bar,
                bar,
                bpm,
            });
        }
    }
    let bpm = if all_ibis.is_empty() {
        0.0
    } else {
        60.0 / dsp::percentile(&mut all_ibis, 50.0)
    };
    (beats, bpm)
}

/// Per tempogram window: the fraction of beats within ±4 s whose time has a
/// default-sensitivity onset within ±60 ms (NaN where there are no beats).
/// Music with a pulse has attacks on (most of) its beats; a beat grid
/// stretched over rubato notes mostly falls between them.
fn beat_coverage(beats: &[Beat], onsets: &[super::Onset], n_win: usize, hop_s: f64) -> Vec<f32> {
    let times: Vec<f64> = onsets
        .iter()
        .filter(|o| o.strength >= 0.5)
        .map(|o| o.time)
        .collect();
    let hit: Vec<bool> = beats
        .iter()
        .map(|b| nearest_within(&times, b.time, 0.06).is_some())
        .collect();
    (0..n_win)
        .map(|j| {
            let c = j as f64 * hop_s;
            let a = beats.partition_point(|b| b.time < c - 4.0);
            let b = beats.partition_point(|b| b.time < c + 4.0);
            if b <= a {
                return f32::NAN;
            }
            hit[a..b].iter().filter(|h| **h).count() as f32 / (b - a) as f32
        })
        .collect()
}

/// Track-level rhythm confidence: the mean over non-silent windows.
fn track_rhythm(rhythm: &[f32], silent: &[bool], hop: usize) -> f32 {
    let vals: Vec<f32> = rhythm
        .iter()
        .enumerate()
        .filter(|(j, _)| {
            let i = j * hop;
            i < silent.len() && !silent[i]
        })
        .map(|(_, &v)| v)
        .collect();
    dsp::mean(&vals)
}

fn build_sections(
    bounds: &[f64],
    duration: f64,
    frames: &[FeatureFrame],
    energy: &[f32],
    drops: &[DropEvent],
    fps: f64,
) -> Vec<Section> {
    let mut edges: Vec<f64> = vec![0.0];
    edges.extend(
        bounds
            .iter()
            .copied()
            .filter(|&t| t > 0.5 && t < duration - 0.5),
    );
    edges.push(duration);
    // Merge sections shorter than 4 s into their predecessor; drop edges and
    // the end always survive.
    let mut merged: Vec<f64> = vec![edges[0]];
    for &e in &edges[1..] {
        let strong = e >= duration || drops.iter().any(|d| (d.time - e).abs() < 0.01);
        if e - merged.last().unwrap() < 4.0 {
            if !strong {
                continue;
            }
            if merged.len() > 1 {
                merged.pop();
            }
        }
        merged.push(e);
    }
    let n = frames.len();
    let mut secs: Vec<(Section, f32, bool)> = merged
        .windows(2)
        .map(|w| {
            let a = ((w[0] * fps) as usize).min(n);
            let b = ((w[1] * fps) as usize).clamp(a, n);
            let slice = &frames[a..b];
            let silent_frac =
                slice.iter().filter(|f| f.is_silent()).count() as f32 / slice.len().max(1) as f32;
            let e = dsp::mean(&energy[a..b]);
            let sec = Section {
                start: w[0],
                end: w[1],
                energy: 0.0,
                kind: SectionKind::Medium,
                starts_with_drop: drops.iter().any(|d| (d.time - w[0]).abs() < 1.0),
                rhythm: 0.0,
            };
            (sec, e, silent_frac > 0.8)
        })
        .collect();
    let active: Vec<f32> = secs.iter().filter(|s| !s.2).map(|s| s.1).collect();
    let (lo, hi) = active
        .iter()
        .fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    for (s, e, silent) in secs.iter_mut() {
        if *silent {
            s.energy = 0.0;
            s.kind = SectionKind::Silence;
            continue;
        }
        let rel = if hi - lo > 0.05 {
            (*e - lo) / (hi - lo)
        } else {
            0.5
        };
        let abs = (*e / hi.max(1e-3)).clamp(0.0, 1.0);
        s.energy = (0.6 * rel + 0.4 * abs).clamp(0.0, 1.0);
        s.kind = if s.energy < 0.35 {
            SectionKind::Calm
        } else if s.energy < 0.7 {
            SectionKind::Medium
        } else {
            SectionKind::Intense
        };
    }
    secs.into_iter().map(|s| s.0).collect()
}

fn build_spectrogram(
    bands: &[[f32; N_BANDS]],
    silent: &[bool],
    fps: f64,
    sample_rate: u32,
) -> Spectrogram {
    let mut all: Vec<f32> = bands
        .iter()
        .zip(silent)
        .filter(|(_, s)| !**s)
        .flat_map(|(b, _)| b.iter().copied())
        .collect();
    let top = dsp::percentile(&mut all, 99.5);
    let floor = top - 60.0;
    let step = 2;
    let mut data = Vec::with_capacity(bands.len() / step * N_BANDS + N_BANDS);
    for chunk in bands.chunks(step) {
        for k in 0..N_BANDS {
            let v = chunk.iter().map(|r| r[k]).sum::<f32>() / chunk.len() as f32;
            data.push((((v - floor) / 60.0).clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    Spectrogram {
        columns_per_second: fps / step as f64,
        bands: N_BANDS,
        band_edges_hz: dsp::band_edges(sample_rate),
        data,
    }
}
