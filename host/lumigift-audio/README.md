# lumigift-audio

Audio → light-show pipeline for Lumigift: decoding, playback with an accurate
"heard now" clock, offline music analysis, the editable show model and the
engine that turns it all into `MusicFrame`s at 100 Hz. A real-time (live
input) path shares the same engine.

```text
 file ──► load_track (Job) ──► LoadedTrack { audio, analysis }
                                  │              │
                        Player::load(audio)      ├─► Show::auto_generate(&analysis) ─► edit ─► .lgshow
                                  │              │
                         player.position() ──► ShowEngine(analysis, show).frame_for_playback(pos, slots)
                                                               │
                                                      MusicFrame @ 100 Hz ─► lumigift-core
```

## Usage

```rust
use std::sync::Arc;
use lumigift_audio::*;

// 1. Load + analyse in the background (cached by file content hash).
let mut job = load_track("song.mp3", AnalysisCache::default_location());
// GUI: poll once per frame.
//   job.progress() -> 0..1, job.stage() -> "Decoding", job.cancel()
let track = loop {
    if let Some(r) = job.try_take() { break r?; }
    std::thread::sleep(std::time::Duration::from_millis(16));
};

// 2. Playback.
let player = Player::new()?;              // or Player::with_device(Some(id)), see Player::output_devices()
player.load(track.audio.clone());          // cheap, no copy
player.play();

// 3. A show (auto-generated, or Show::load("x.lgshow")).
let mut show = Show::auto_generate(&track.analysis);
show.track = TrackRef {
    path: track.path.display().to_string(),
    content_hash: track.audio.content_hash.clone(),
    title: track.audio.metadata.title.clone().unwrap_or_default(),
};

// 4. Engine at 100 Hz.
let mut engine = ShowEngine::new(track.analysis.clone(), show);
engine.set_lookahead(0.020);               // radio + render latency, see below
loop {
    let frame = engine.frame_for_playback(player.position(), 10 /* slots */);
    // core.broadcast_music(frame);
    std::thread::sleep(std::time::Duration::from_millis(10));
}
```

Examples:

```sh
cargo run -p lumigift-audio --release --example analyze -- song.mp3 [--cache] [--show out.lgshow]
cargo run -p lumigift-audio --release --example play -- song.mp3 [--seek 60] [--device ID]
cargo run -p lumigift-audio --release --example play -- --list
cargo run -p lumigift-audio --release --example bench      # synthetic accuracy + speed
cargo run -p lumigift-audio --release --example eagerness -- song.mp3   # how jumpy the lights are (also --synth, --legato)
```

## API overview

| Item | Purpose |
|---|---|
| `load_track(path, Option<AnalysisCache>) -> Job<LoadedTrack>` | read, hash, decode, analyse (cache hit skips analysis) |
| `analyze_async(Arc<DecodedAudio>, cache) -> Job<Arc<Analysis>>` | analyse already decoded audio |
| `Job<T>` | `progress()`, `stage()`, `cancel()`, `is_finished()`, `try_take()`, `wait()`; dropping cancels |
| `decode_file(path) -> DecodedAudio` | blocking decode: interleaved `f32`, original rate/channels, `mono()`, `metadata` (title/artist/album/duration/cover art), `content_hash` |
| `Player` | `load`, `play`, `pause`, `toggle`, `stop`, `seek`, `set_volume`, `state`, `position`, `write_position`, `duration`, `output_latency`, `set_latency_offset`, `set_device`, `output_devices`, `last_error`. All `&self`, thread-safe |
| `Analysis` | `frames` (features ~100 fps), `onsets` (time, strength), `bpm`, `beats` (time, beat_in_bar, bar, bpm), `sections` (start, end, energy, kind, starts_with_drop, rhythm), `drops`, `rhythm` (confidence over time, every `rhythm_hop` s), `rhythm_confidence` (track), `waveform`, `spectrogram` |
| `Analysis` lookups | `features_at(t)`, `beat_position(t)`, `beat_index_at`, `beats_in`, `downbeats`, `nearest_beat`, `nearest_downbeat`, `snap(t, Snap::{Off,Beat,HalfBeat,Downbeat})`, `section_at`, `tempo_at`, `energy_at`, `rhythm_at`, `onsets_at(sensitivity)` |
| `Waveform::buckets_in(t0, t1, n)` | min/max/RMS for exactly `n` pixels of any visible range |
| `Spectrogram` | 32 log bands × ~50 columns/s as `u8` (`column`, `column_at`, `band_edges_hz`) |
| `AnalysisCache` | `default_location()` (`~/.cache/lumigift/analysis`), `new(dir)`, `load(hash)`, `store(&a)` |
| `ShowSettings` | `master_brightness`, `beat_flags`, `onset_flags`, `drop_flags`, and the reactivity settings `sensitivity`, `smoothing`, `intensity` (0..1, default 0.5) and `beat_sync: BeatSync::{Auto (default), On, Off}`; all `serde(default)` |
| `Show` | `cues`, `events` (one-shots), `settings`, `track`; `load`/`save` (`.lgshow` JSON), `insert_cue`, `move_cue`, `remove_cue`, `split_at`, `cue_at`, `cue_index_at`, `insert_event`, `move_event`, `remove_event`, `events_at`, `auto_generate` |
| `Cue` | `time`, `effect`, `palette`, `brightness`, `hue`, `hue_speed` (hue steps/s), `params`, `fade` (s), `slot_overrides`, `label` |
| `OneShot` | `time`, `duration`, `kind`: `Strobe { rate_hz }`, `Blackout`, `Flash { hue, palette }` |
| `ShowEngine` | `frame_at(t, slots)`, `frame_for_playback(heard_pos, slots)`, `peek_frame` (stateless, for scrubbing), `peek_frame_at(&mut EngineCursor, t, slots)` (a second time line, e.g. the local preview), `slot_frame` (per-slot overrides), `set_lookahead`, `rhythm_at`, `beat_sync_active`, `show_mut`, `set_show`, `set_provider`, `reset` |
| `FeatureProvider` | trait shared by `Analysis` and `live::LiveFeatures`: `features_at`, `beat_at`, `events_in(t0, t1)`, `duration`, plus defaulted `rhythm_confidence_at` (None = trust the beat grid) and `events_with_strength` |
| `live::*` | `LiveSource` trait, `CpalInputSource`, `MemorySource`, `RealtimeAnalyzer`, `LiveFeatures`, `LiveAnalysis` |

Effect ids and palette ids are those of `lumigift_fx::effect` and
`lumigift_fx::PALETTES` (`show::palette` has named constants).

## Timing: what "position" and "lookahead" mean

* `Player::position()` is the media time **being heard right now**. Each cpal
  callback reports when its first frame reaches the DAC (`playback` vs.
  `callback` timestamps). The player pairs that with `Instant::now()` and
  extrapolates between callbacks with the monotonic clock. A slew filter
  absorbs callback jitter, so the value is smooth and monotonic at any query
  rate. It holds still while a seek, pause or the end of the track is still in
  the output pipeline. Measured on PipeWire's ALSA device: 37 ms reported
  latency and 0.06 ms deviation from a straight line. PipeWire settles its
  buffer by ~14 ms during the first ~3 s after start, and the clock follows.
* `Player::set_latency_offset(s)` adds latency the backend does not know
  about (Bluetooth speakers, external DSP). Expose it as a calibration slider.
* `ShowEngine::frame_at(t)` describes the music at `t`. Lights light up
  `radio_latency` after you send a frame, so call
  `frame_at(player.position() + radio_latency)`, or set
  `engine.set_lookahead(radio_latency)` and call
  `frame_for_playback(player.position())`. Typical radio_latency is 10–30 ms
  (core queue + USB + air + gift render period). File analysis knows the
  future, so this lines up exactly.
* **Only add the latency a frame really goes through.** A frame rendered
  locally (the Studio's 3D preview) is visible at the next vsync, so it is
  computed for `position + display_dt`. Adding the radio latency there made
  the preview run 26–42 ms ahead of the sound, which reads as "eager" lights
  (visual-before-audio offsets are noticed far sooner than the reverse). The
  Studio therefore computes two frames per display frame
  (`pages/studio.rs`, `Studio::tick`): the preview at
  `position + display_dt` on its own `EngineCursor`, and, only when LIVE, the
  radio frame on the engine's cursor at
  * hardware: `position + latency_ms` (the calibrated radio + render latency
    of the real gifts; the Studio slider);
  * simulator: `position + display_dt + max(display_dt, 11 ms)`. The sim
    gifts get the frame within one 10 ms show tick plus ~1 ms of simulated
    USB/air and render it at once; the stage reads their LEDs back in the
    next GUI frame and shows them one vsync later. The hardware latency does
    not apply.
* Event flags (BEAT, DOWNBEAT, ONSET, DROP, strobe ticks) are set in the frame
  whose interval `(t_prev, t]` contains the event, so every event is sent once
  regardless of the call rate. Going backwards, or jumping forward more than
  0.25 s, counts as a seek: edge detection restarts without a burst of stale
  flags. The engine is deterministic.

## Per-slot overrides

A `MusicFrame` is broadcast to every gift, so per-slot overrides cannot travel
in it. `ShowEngine::slot_frame(&frame, t, slot)` applies them. The core can
render overridden slots host-side with `lumigift_fx::Renderer` and send `Leds`
packets for them, or a future protocol revision can carry them.
`has_slot_overrides(t)` tells you when that is needed.

## Analysis algorithms

* **Front-end:** mono mixdown, Hann STFT (2048 at 44.1/48 kHz), hop =
  rate/100, so exactly 100 frames/s at 44.1 and 48 kHz. Per frame: bass
  20–150 Hz, mid 150–2000 Hz, treble 2–12 kHz, RMS, 32 log bands
  (30 Hz–16 kHz), spectral flux on floored band levels.
* **Normalisation:** per track (never per section), each dB feature maps
  from `[max(p5, p97 − 30 dB), p97]` to 0..1, then `^1.5`, so it is relative
  to the track's loud passages (p97). Quiet and loud masters both use the
  full LED range (tested at −30 dB). The silence gate is −55 dBFS, lowered
  for very quiet masters. How strongly the lights follow these values is
  shaped later by the engine (see Reactivity).
* **Onsets** (`analysis/onset.rs`): the beat tracker keeps the plain flux;
  ONSET flags use a vibrato-suppressed flux (SuperFlux-style: the previous
  frame is max-filtered over ±1 log band, so a partial wobbling between bands
  is not new energy; normalised by `max(p99, 1 dB/band)`). Peaks must clear
  `median + k·MAD + floor` over a ±1 s window and be ≥ 90 ms apart (the
  stronger wins). Each onset's `strength` is `1 − s*`, where `s*` is the
  lowest show sensitivity at which it clears the threshold (`k` 6→2, `floor`
  0.20→0.04 as sensitivity goes 0→1; the default 0.5 is `median + 4·MAD +
  0.12`). So `Analysis::onsets` is the superset and `onsets_at(sensitivity)`
  what a show flags. Soft legato attacks peak ~60 ms after the note starts
  (the new pitch has to overtake the old one).
* **Rhythm confidence** (pulse clarity, `beat::rhythm_confidence`): per 1 s
  tempogram window, the product of
  * beat coverage: the fraction of tracked beats within ±4 s that have a
    default-sensitivity onset within ±60 ms, mapped through
    smoothstep(0.45, 0.75). A real pulse has attacks on its beats; a grid
    forced onto rubato notes mostly falls between them;
  * autocorrelation clarity: the normalised tempogram ACF at the decoded lag,
    smoothstep(0.10, 0.35), weighted 0.4 + 0.6·x;
  * tempo stability: how often each window's own best lag agrees (up to
    octaves) with the Viterbi tempo over ±3 windows, weighted 0.7 + 0.3·x;
  then median-of-5 and mean-of-7 smoothing. `Section::rhythm` is the mean
  over the section, `rhythm_confidence` the mean over non-silent windows.
  Measured: synthetic drums 0.97–1.00, an orchestral theme with percussion
  0.87, brushed background jazz 0.50 (varies 0.2–0.8 over time), the
  rubato solo violin 0.04, a synthetic rubato legato melody 0.00.
* **Tempo:** autocorrelation tempogram (8 s windows, 1 s hop, 60–200 BPM). A
  log-normal prior around 120 BPM (1 octave) counters octave errors, and
  Viterbi decoding over the lag lets tempo follow real changes but not jitter.
  Parabolic interpolation gives sub-frame precision.
* **Beats:** Ellis (2007) dynamic programming with the time-varying period,
  run separately per non-silent region. Weak edge beats are trimmed and beat
  times are refined to sub-frame precision. A measured constant 9.5 ms flux
  lead is removed.
* **Downbeats (4/4):** per region, choose the bar phase whose beats carry the
  most kick (low-band flux), bass energy and harmonic change between the four
  beats before and after (largest at bar lines, since drum loops cancel out).
* **Drops:** mean energy over the next 3 s vs. the previous 6 s
  (`post > 0.45`, `pre < 0.75 · post`, contrast > 0.25). The time is placed at
  the steepest rise and snapped to a beat.
* **Sections:** Foote checkerboard novelty (±8 s Gaussian-tapered kernel) on
  0.5 s block features (8 coarse bands + bass/mid/treble/level/onset,
  z-scored). Adaptive peak picking, ≥ 6 s apart, plus drops and silence edges,
  snapped to downbeats. Energy is relative within the track; kinds are
  Silence, Calm, Medium, Intense.
* **Live (`RealtimeAnalyzer`):** the same front-end, causal. AGC
  normalisation, onset peak detection with 10 ms delay, autocorrelation tempo
  over 6 s with hysteresis, and a beat PLL whose phase comes from a
  kick-weighted comb filter with a continuity prior. Downbeat comes from the
  per-slot kick energy. Drops compare the last 0.3 s with the 6 s before.

## Reactivity (show settings, applied by the engine)

All deterministic and independent of the call rate:

* **Beat sync** (`Auto` | `On` | `Off`): BEAT / DOWNBEAT flags, `beat_phase`,
  `beat_in_bar` and `bpm` come from the beat grid only where the rhythm
  confidence is ≥ `engine::RHYTHM_THRESHOLD` (0.5) in `Auto`, always in `On`,
  never in `Off`. Without sync the frame has `bpm = 0` (the gifts' phase-wrap
  beat detection stays off) and a free-running 4 s `beat_phase`, so Chase
  still moves, slowly. Live providers report no confidence and keep syncing.
* **Sensitivity** (default 0.5): ONSET is flagged when the strongest onset in
  the frame has `strength ≥ 1 − sensitivity`.
* **Smoothing** (default 0.5): bass / mid / treble / level pass through
  attack/release envelopes stepped on a fixed 10 ms grid (re-primed 3 s
  before `t` after a seek). τ_attack = (2 + 12·s) ms · (1 + 6·legato),
  τ_release = (20 + 120·s) ms · (1 + 5·legato), with legato = 1 − rhythm
  confidence: 8 / 80 ms on rhythmic music (as crisp as the gifts' own band
  smoothing), 56 / 480 ms on rubato music.
* **Intensity** (default 0.5): the sent value is
  `slow + depth · (fast − slow)`, where `slow` is a 1.5 s phrase envelope and
  `depth = 2 · intensity · (0.5 + 0.5 · rhythm)`: unchanged on rhythmic
  music, half the in-phrase jitter on legato, while quiet phrases stay quiet
  and loud ones loud. Solid, Breathe and Sparkle ignore loudness themselves,
  so their brightness is scaled by `1 − a + a · phrase` (`a = 0.8 · intensity`,
  phrase = the slow level envelope on a dB-linear scale): quiet passages
  dimmer, never dark.
* **Auto-generated shows** use the per-section rhythm confidence: sections
  without a pulse get Breathe / Solid (slow hue drift), plus Spectrum /
  Sparkle when medium and Rainbow when intense, 3 s cross-fades, a cue every
  ~24 s and no Strobe, Pulse, Chase, blackouts or flashes. Rhythmic sections
  are generated exactly as before.

`examples/eagerness.rs` (and `eagerness::measure`, used by `tests/sync.rs`)
renders a show through the engine and ten `lumigift_fx::Renderer`s at 100 Hz
and reports BEAT / ONSET flags per minute, brightness jumps > 40 % within
50 ms per gift per minute, the mean |Δbrightness| per 10 ms frame, and the
correlation between brightness and loudness (per second). Auto shows, before
→ after this tuning:

| Track | BEAT/min | ONSET/min | jumps/min | mean Δ %/frame | r(brightness, loudness) |
|---|---|---|---|---|---|
| Solo violin, rubato (85.8 s) | 105.6 → 0 | 744 → 116 | 70.5 → 0.1 | 2.95 → 0.21 | +0.64 → +0.65 |
| Synthetic legato melody (90 s) | 178 → 0 | 491 → 60 | 85.1 → 0 | 2.10 → 0.13 | +0.46 → +0.57 |
| Synthetic demo track (drums, 73.5 s) | 126.5 → 126.5 | 275 → 265 | 89.0 → 82.9 | 1.93 → 1.71 | +0.32 → +0.17 |

## Accuracy and speed (synthetic signals, `cargo test`, `examples/bench.rs`)

* Tempo within ±2 BPM at 75/95/120/124/126/128/140/170 BPM, across a
  120→128 BPM step and a 110→135 BPM ramp.
* Beat F-measure (±70 ms) 1.000 on the demo track (silence, quiet groove,
  build-up, drop, tempo change, outro). Mean offset −0.1 ms; every beat within
  15 ms. Downbeat F-measure 1.000.
* Drop detected 0.00–0.01 s from the truth (test tolerance ±0.5 s); sections
  exact at build-up/drop.
* mp3/flac/ogg/m4a encodings (via ffmpeg) give the same tempo and F > 0.9.
* Live: tempo ±2 BPM, beat F-measure 0.987 after an 8 s lock, drop reported
  0.13 s after it happens.
* Onsets (default sensitivity): ≥ 98 % of synthetic drum beats flagged within
  30 ms (≥ 90 % even at sensitivity 0), hats included. Synthetic legato
  melody: note F-measure 0.97 (±100 ms), 90 onsets for 96 notes (was 713).
  Rubato solo violin: 166 onsets in 85.8 s, 1.9/s (was 1,065, 12.4/s).
* Speed (release, synthetic 4-minute 44.1 kHz track): analysis 0.14 s. Real
  3:21 mp3: decode 0.34 s + analysis 0.18 s.

## Live sources and perfect sync for Spotify desktop (future)

`LiveSource` is the plug-in point: anything that delivers interleaved `f32`
blocks. `LiveAnalysis::start(Box<dyn LiveSource>)` runs it through
`RealtimeAnalyzer` into a `LiveFeatures`, which is a `FeatureProvider`, so it
drops into `ShowEngine::new(features.clone(), show)` and you call
`frame_at(features.now(), slots)`. `CpalInputSource` works today with any
cpal input device. On PipeWire/PulseAudio, set the default source to a
monitor (`pactl set-default-source <sink>.monitor`) and the ALSA
`pipewire`/`pulse` device records what other apps play.

Causal analysis always trails the audio (~25–50 ms) and cannot see a drop
coming. The way to get file-quality sync with Spotify desktop is to **delay
the audio instead of predicting the lights**:

1. Create a virtual sink for Spotify:
   `pactl load-module module-null-sink sink_name=lumigift sink_properties=device.description=Lumigift`
   and route Spotify to it (pavucontrol, `pw-metadata`, or a WirePlumber
   rule matching `application.name = "spotify"`).
2. Capture `lumigift.monitor` (a PipeWire capture stream: a future
   `PipeWireSource: LiveSource` via the `pipewire` crate, which needs
   libpipewire dev headers. cpal's `pipewire` feature also works).
3. Play the captured audio to the real speakers through our own `Player`-like
   output with a fixed delay `D` (e.g. 300–500 ms ring buffer). The
   `RealtimeAnalyzer` sees every sample `D` seconds before it is heard, which
   equals `D` seconds of look-ahead. Treat the live stream as a growing
   "file": analysis runs at capture time, and the output clock (cpal
   timestamps, exactly as in `Player`) gives the heard position. The engine
   then calls `frame_at(heard_position + radio_latency)` on a provider that
   already holds features for that time. Drops can even be detected before
   they are heard, using the same contrast detector with `D` as look-ahead.
4. Spotify's own UI is then `D` ahead of the sound, which is fine for a party.

Spotify Connect via librespot would be another `LiveSource`, using a custom
`Sink` that forwards decoded PCM. It is blocked on upstream.

## Known limitations

* Rhythm confidence describes ~8–10 s passages; a pulse that starts or stops
  abruptly switches beat sync a few seconds late or early. Beat sync `On`
  forces the grid.
* Rainbow's colour speed is fixed in `lumigift-fx`, so auto shows only use it
  for intense passages without a pulse.

* 4/4 assumed; downbeat phase is chosen per non-silent region (a 2-beat fill
  inside a region shifts bars until the next silence).
* Tempo is limited to 60–200 BPM, and tracks around 85/170 BPM may be reported
  at either octave (both are fine for lights).
* "Drops" are energy rises. In non-electronic music they mark loud choruses
  (strength 0.25–0.4); auto shows only add strobe bursts for strength ≥ 0.3.
* Section labels are unnamed (no verse/chorus classification).
* The very first beat of a file that starts exactly on a beat at t = 0 is
  missed (flux needs a previous frame).
* Live: needs ~3–8 s to lock tempo; beat phase is predicted, not detected.
* `Player` decodes nothing itself (holds the whole track in RAM, ~85 MB for 4
  min stereo f32). Changing device restarts the stream (brief gap).
* Output latency is whatever the backend reports. PipeWire's ALSA device
  settles over ~3 s after stream start, and Bluetooth needs
  `set_latency_offset`.
