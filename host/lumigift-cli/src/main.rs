//! `lumigift`: headless control of Lumigift gifts through the nRF24 USB bridge
//! (or the built-in simulator with `--sim`).
//!
//! Gifts can be referred to by slot (`s3`), node address (`@17`), UID prefix
//! (`3f2a91`), name, or `all`.

use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use lumigift_core::sim::{SimConfig, SimHandle};
use lumigift_core::*;

#[derive(Parser)]
#[command(
    name = "lumigift",
    version,
    about = "Control Lumigift gift boxes through the nRF24 USB bridge"
)]
struct Cli {
    /// Use the simulator with N gifts instead of hardware.
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "10", value_name = "N")]
    sim: Option<usize>,
    /// Simulator only: packet loss in percent (each direction, per attempt).
    /// Also turns off the demo's random reboots, for repeatable measurements.
    #[arg(long, global = true, value_name = "PCT")]
    sim_loss: Option<f32>,
    /// OTA: first-pass chunk rate in chunks/s (default: automatic, from the
    /// gifts' flash timing).
    #[arg(long, global = true, value_name = "CHUNKS_PER_S")]
    ota_rate: Option<f32>,
    /// Bridge serial number (default: first bridge found).
    #[arg(long, global = true)]
    serial: Option<String>,
    /// Radio channel the gifts are on (default 76).
    #[arg(long, global = true)]
    channel: Option<u8>,
    /// Don't wake dozing gifts on connect (by default a ~1.3 s Wake burst
    /// precedes the scan, since gifts doze after a minute without a host).
    #[arg(long, global = true)]
    no_wake: bool,
    /// More logging (-v info, -vv debug).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List connected bridges.
    Devices,
    /// Update the USB bridge's own firmware over USB (no ST-Link needed).
    /// Takes the bridge app's ELF or a raw .bin for 0x08004000.
    BridgeUpdate { firmware: std::path::PathBuf },
    /// Discover gifts and list them.
    Scan,
    /// Live status table (Ctrl-C to quit).
    Status {
        /// Print once and exit.
        #[arg(long)]
        once: bool,
        /// Refresh interval in milliseconds.
        #[arg(long, default_value_t = 500)]
        interval: u64,
    },
    /// Blink gifts.
    Blink {
        /// Gift (slot `s3`, address `@17`, UID prefix, name) or `all`.
        gift: String,
        /// Colour as hex (`ff0000`, `#00ff00`).
        #[arg(long, default_value = "ffffff")]
        color: String,
        #[arg(long, default_value_t = 3)]
        count: u8,
        /// Blink period in milliseconds.
        #[arg(long, default_value_t = 400)]
        period: u64,
    },
    /// Blink a distinctive pattern to find a gift.
    Identify { gift: String },
    /// Assign a show slot (`none` to unassign).
    SetSlot { gift: String, slot: String },
    /// Assign slots automatically (sorted by UID, or keeping existing ones with --keep).
    AutoSlots {
        #[arg(long)]
        keep: bool,
    },
    /// Reboot gifts.
    Reboot {
        gift: String,
        /// Stay in the bootloader.
        #[arg(long)]
        bootloader: bool,
    },
    /// Set a config value: max-brightness, idle-timeout (ms), power-budget (mA),
    /// channel (moves gifts and bridge), or a numeric key.
    Config {
        key: String,
        value: u16,
        /// Which gifts (default all; ignored for `channel`).
        #[arg(long, default_value = "all")]
        target: String,
    },
    /// Give a gift a name (host-side only).
    Rename { gift: String, name: String },
    /// Update firmware.
    Ota {
        /// Image file (ImageHeader + app).
        image: std::path::PathBuf,
        /// Comma-separated gift references (default: all known gifts).
        #[arg(long, value_delimiter = ',')]
        targets: Vec<String>,
        /// Also update gifts that already run this exact image.
        #[arg(long)]
        force: bool,
    },
    /// Rescue unreachable gifts: broadcast BootHold on the default channel
    /// while you power-cycle them, then list the gifts in their bootloader
    /// (update them with `ota` afterwards).
    Rescue {
        /// How long to keep broadcasting.
        #[arg(long, default_value_t = 30)]
        seconds: u64,
    },
    /// Put every gift to sleep (LEDs off, radio listening only briefly once
    /// a second). Gifts also doze by themselves a minute after the last host
    /// stops talking to them.
    Sleep,
    /// Wake dozing gifts (a ~1.3 s Wake burst, then a scan) and list them.
    Wake,
    /// Show one colour on every slot until Ctrl-C (or --seconds).
    Solid {
        color: String,
        #[arg(long)]
        seconds: Option<u64>,
    },
}

type Res<T = ()> = Result<T, String>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "off",
        1 => "info",
        _ => "debug",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Res {
    match &cli.cmd {
        Cmd::Devices => return devices(cli.sim.is_some()),
        Cmd::BridgeUpdate { firmware } => return bridge_update(firmware, cli.serial.as_deref()),
        _ => {}
    }
    let (link, sim) = open(&cli)?;
    match cli.cmd {
        Cmd::Devices | Cmd::BridgeUpdate { .. } => unreachable!(),
        Cmd::Scan => {
            let n = link.rescan().wait().map_err(e)?;
            println!("{n} gift(s) online");
            print_table(&link.snapshot(), sim.as_ref());
        }
        Cmd::Status { once, interval } => status(
            &link,
            sim.as_ref(),
            once,
            Duration::from_millis(interval.max(50)),
        ),
        Cmd::Blink {
            gift,
            color,
            count,
            period,
        } => {
            let t = target(&link, &gift)?;
            let c = parse_color(&color)?;
            report(
                link.blink(t, c, count, Duration::from_millis(period))
                    .wait(),
            )?;
            // Let the blink finish before the process (and the link) exits.
            std::thread::sleep(Duration::from_millis(period * count as u64));
        }
        Cmd::Identify { gift } => {
            let t = target(&link, &gift)?;
            report(link.identify(t).wait())?;
        }
        Cmd::SetSlot { gift, slot } => {
            let uid = single(&link, &gift)?;
            let slot = if slot.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(slot.parse::<u8>().map_err(e)?)
            };
            link.set_slot(uid, slot).wait().map_err(e)?;
            println!("ok");
        }
        Cmd::AutoSlots { keep } => {
            let mode = if keep {
                SlotAssign::KeepExisting
            } else {
                SlotAssign::ByUid
            };
            report(link.auto_assign_slots(mode).wait())?;
            print_table(&link.snapshot(), sim.as_ref());
        }
        Cmd::Reboot { gift, bootloader } => {
            let t = target(&link, &gift)?;
            report(link.reboot(t, bootloader).wait())?;
        }
        Cmd::Config {
            key,
            value,
            target: tsel,
        } => {
            let k = match key.to_ascii_lowercase().replace('_', "-").as_str() {
                "max-brightness" | "brightness" => config_key::MAX_BRIGHTNESS,
                "idle-timeout" | "idle-timeout-ms" => config_key::IDLE_TIMEOUT_MS,
                "power-budget" | "power-budget-ma" => config_key::POWER_BUDGET_MA,
                "channel" => {
                    let v =
                        u8::try_from(value).map_err(|_| "channel must be 0..=125".to_string())?;
                    return report(link.set_channel(v).wait());
                }
                other => other
                    .parse::<u8>()
                    .map_err(|_| format!("unknown config key {other:?}"))?,
            };
            let t = target(&link, &tsel)?;
            report(link.set_config(t, k, value).wait())?;
        }
        Cmd::Rename { gift, name } => {
            let uid = single(&link, &gift)?;
            link.rename(uid, Some(name)).map_err(e)?;
            println!("ok");
        }
        Cmd::Ota {
            image,
            targets,
            force,
        } => ota(&link, &image, &targets, force, sim.is_some())?,
        Cmd::Rescue { seconds } => rescue(&link, sim.as_ref(), seconds)?,
        Cmd::Sleep => {
            let n = link.sleep_all().wait().map_err(e)?;
            println!("{n} gift(s) asleep");
            let awake = link.snapshot().online_count();
            if awake > 0 {
                println!("{awake} gift(s) still answer; try again");
            }
        }
        Cmd::Wake => {
            let n = link.wake_all().wait().map_err(e)?;
            println!("{n} gift(s) online");
            print_table(&link.snapshot(), sim.as_ref());
        }
        Cmd::Solid { color, seconds } => {
            let c = parse_color(&color)?;
            link.set_show(ShowOutput::Solid(vec![c; proto::MAX_SLOTS as usize]));
            let s = link.snapshot();
            let unassigned = s
                .gifts
                .iter()
                .filter(|g| g.online && g.slot.is_none())
                .count();
            if unassigned > 0 {
                eprintln!(
                    "note: {unassigned} online gift(s) have no slot and stay dark (try `auto-slots`)"
                );
            }
            println!(
                "showing #{:02x}{:02x}{:02x} on all slots{}",
                c.r,
                c.g,
                c.b,
                if seconds.is_none() {
                    ", Ctrl-C to stop"
                } else {
                    ""
                }
            );
            let end = seconds.map(|s| Instant::now() + Duration::from_secs(s));
            while end.is_none_or(|t| Instant::now() < t) {
                std::thread::sleep(Duration::from_millis(100));
            }
            link.set_show(ShowOutput::Blackout);
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    link.shutdown();
    Ok(())
}

fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

fn devices(sim: bool) -> Res {
    if sim {
        println!("simulator");
        return Ok(());
    }
    let list = nrf24_usb::list().map_err(e)?;
    if list.is_empty() {
        if nrf24_usb::dfu::in_bootloader(None).unwrap_or(false) {
            println!("a bridge is waiting in its USB bootloader (1209:0002): run `lumigift bridge-update`");
        } else {
            println!("no bridge found (VID 1209, PID 0001)");
        }
    }
    for d in list {
        println!(
            "bus {} addr {:3}  serial {:<16} {}",
            d.bus,
            d.address,
            d.serial.as_deref().unwrap_or("-"),
            d.product.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

fn bridge_update(path: &std::path::Path, serial: Option<&str>) -> Res {
    use nrf24_usb::dfu::{self, Phase};
    let bytes = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let fw = dfu::Firmware::parse(&bytes).map_err(e)?;
    let mut last = None;
    let report = dfu::update(&fw, serial, &mut |phase, done, total| {
        let name = match phase {
            Phase::Detach => "switching to bootloader",
            Phase::Erase => "erasing",
            Phase::Write => "writing",
            Phase::Verify => "verifying",
            Phase::Restart => "restarting",
        };
        let pct = (done * 100).checked_div(total).unwrap_or(100);
        if last != Some((phase, pct / 5)) {
            last = Some((phase, pct / 5));
            print!("\r  {name:<24} {pct:3}%\x1b[K");
            let _ = std::io::stdout().flush();
        }
    })
    .map_err(e)?;
    println!();
    println!(
        "updated bridge {} ({} bytes at 0x{:08X}) in {:.1} s{}",
        report.serial.as_deref().unwrap_or("?"),
        report.bytes,
        report.base,
        report.elapsed.as_secs_f32(),
        match report.fw_version {
            Some(v) => format!(", running fw {}.{}", v >> 8, v & 0xFF),
            None => String::from(" (did not see the app come back yet)"),
        }
    );
    Ok(())
}

fn open(cli: &Cli) -> Res<(Link, Option<SimHandle>)> {
    let mut cfg = LinkConfig::default();
    if let Some(ch) = cli.channel {
        cfg.radio.channel = ch;
    }
    if let Some(r) = cli.ota_rate {
        cfg.ota.chunks_per_s = r.max(0.0);
    }
    // `sleep` need not wake anything first, and `wake` sends its own burst.
    cfg.wake_on_connect = !cli.no_wake && !matches!(cli.cmd, Cmd::Sleep | Cmd::Wake);
    let (link, sim) = match cli.sim {
        Some(n) => {
            cfg.metadata = MetadataLocation::Disabled;
            let mut sc = SimConfig::demo(n);
            if let Some(loss) = cli.sim_loss {
                sc.loss_pct = loss;
                sc.reboot_rate_per_hour = 0.0;
            }
            let (l, h) = Link::simulated(cfg, sc);
            (l, Some(h))
        }
        None => (Link::open_usb(cfg, cli.serial.clone()), None),
    };
    // Wait for the bridge.
    let s = link.wait_for(Duration::from_secs(3), |s| {
        s.bridge.state == BridgeState::Connected
            || matches!(s.bridge.state, BridgeState::Disconnected(_))
    });
    if let Some(BridgeState::Disconnected(why)) = s.map(|s| s.bridge.state.clone()) {
        return Err(why);
    }
    if !link.is_connected() {
        return Err("bridge did not answer".into());
    }
    // Wait for the initial sweep so gift references resolve.
    let mut last = -1i32;
    link.wait_for(Duration::from_secs(15), |s| {
        let pct = (s.discovery.sweep_progress * 100.0) as i32;
        if pct / 10 != last / 10 && !s.discovery.initial_sweep_done {
            eprint!("\rscanning... {pct:3}%");
            let _ = std::io::stderr().flush();
            last = pct;
        }
        s.discovery.initial_sweep_done
    });
    if last >= 0 {
        eprint!("\r                 \r");
    }
    Ok((link, sim))
}

fn target(link: &Link, sel: &str) -> Res<Target> {
    if sel.eq_ignore_ascii_case("all") {
        return Ok(Target::All);
    }
    let v = link.snapshot().resolve(sel).map_err(e)?;
    Ok(Target::Gifts(v))
}

fn single(link: &Link, sel: &str) -> Res<Uid> {
    let v = link.snapshot().resolve(sel).map_err(e)?;
    match v.as_slice() {
        [u] => Ok(*u),
        _ => Err(format!(
            "{sel:?} matches {} gifts; be more specific",
            v.len()
        )),
    }
}

fn report(r: Result<CmdReport, LinkError>) -> Res {
    let r = r.map_err(e)?;
    println!("{r}");
    if r.ok.is_empty() && !r.failed.is_empty() {
        Err("command failed".into())
    } else {
        Ok(())
    }
}

fn parse_color(s: &str) -> Res<Rgb> {
    let h = s.trim_start_matches('#');
    if h.len() != 6 {
        return Err(format!("colour must be 6 hex digits, got {s:?}"));
    }
    let v = u32::from_str_radix(h, 16).map_err(|_| format!("bad colour {s:?}"))?;
    Ok(Rgb::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

// ---------------------------------------------------------------------------
// Status table
// ---------------------------------------------------------------------------

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 86400 {
        format!("{}d{:02}h", s / 86400, s % 86400 / 3600)
    } else if s >= 3600 {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

fn led_cells(leds: &[Rgb; 4]) -> String {
    leds.iter()
        .map(|c| format!("\x1b[38;2;{};{};{}m●\x1b[0m", c.r, c.g, c.b))
        .collect()
}

fn print_table(s: &Snapshot, sim: Option<&SimHandle>) {
    let simg = sim.map(|h| h.gifts()).unwrap_or_default();
    println!(
        "{:<4} {:<4} {:<8} {:<12} {:<8} {:>10} {:>8} {:>6} {:>5} {:>5} {:>6} {:>6} {:>4} {:>6}{}",
        "SLOT",
        "ADDR",
        "UID",
        "NAME",
        "STATE",
        "FW",
        "UPTIME",
        "VBAT",
        "LINK%",
        "RETRY",
        "BCAST%",
        "SYNCµs",
        "°C",
        "RESETS",
        if sim.is_some() { "  LEDS" } else { "" }
    );
    for g in &s.gifts {
        let state = if g.asleep {
            "asleep"
        } else if !g.online {
            "offline"
        } else if g.address_conflict {
            "conflict"
        } else if g.in_bootloader {
            "boot"
        } else {
            "ok"
        };
        let leds = simg
            .iter()
            .find(|x| x.uid == g.uid)
            .map(|x| format!("  {}", led_cells(&x.leds)))
            .unwrap_or_default();
        println!(
            "{:<4} {:<4} {:<8} {:<12} {:<8} {:>10} {:>8} {:>6} {:>5.0} {:>5.2} {:>6} {:>6} {:>4} {:>6}{}",
            g.slot.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
            g.node_addr,
            g.uid.short(),
            g.meta
                .name
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(12)
                .collect::<String>(),
            state,
            g.fw_version,
            g.status
                .map(|_| fmt_dur(g.uptime))
                .unwrap_or_else(|| "-".into()),
            if g.status.is_some() {
                format!("{}mV", g.vbat_mv)
            } else {
                "-".into()
            },
            g.poll_success_pct,
            g.avg_retries,
            g.bcast_rx_pct
                .map(|p| format!("{p:.0}"))
                .unwrap_or_else(|| "-".into()),
            if g.sync_err_us == u16::MAX {
                "-".into()
            } else {
                g.sync_err_us.to_string()
            },
            g.temp_c,
            format!("{}/{}", g.resets, g.reboots_seen),
            leds
        );
    }
}

fn status(link: &Link, sim: Option<&SimHandle>, once: bool, interval: Duration) {
    if once {
        // Give freshly discovered gifts a moment to report telemetry.
        let s = link
            .wait_for(Duration::from_millis(1500), |s| {
                s.gifts
                    .iter()
                    .filter(|g| g.online)
                    .all(|g| g.status.is_some())
            })
            .unwrap_or_else(|| link.snapshot());
        print_header(&s);
        print_table(&s, sim);
        return;
    }
    print!("\x1b[2J");
    loop {
        let s = link.snapshot();
        print!("\x1b[H");
        print_header(&s);
        print_table(&s, sim);
        println!("\x1b[K");
        for ev in s
            .events
            .iter()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let age = s.taken.saturating_duration_since(ev.at).as_secs();
            let lvl = match ev.level {
                LogLevel::Info => " ",
                LogLevel::Warn => "!",
                LogLevel::Error => "E",
            };
            println!("{lvl} {:>5}s ago  {}\x1b[K", age, ev.message);
        }
        print!("\x1b[J");
        let _ = std::io::stdout().flush();
        std::thread::sleep(interval);
    }
}

fn print_header(s: &Snapshot) {
    let b = &s.bridge;
    let state = match &b.state {
        BridgeState::Connected => "connected".to_string(),
        BridgeState::Connecting => "connecting".to_string(),
        BridgeState::Disconnected(w) => format!("DISCONNECTED: {w}"),
    };
    println!(
        "{} [{}] ch {}  rtt {}µs  | {}/{} online{} | show {:.0} fps ({:.0} pkt/s, {} dropped) | polls {:.0}/s ({:.0} ok) | tx fail {} | bridge tx {} ack {} fail {} drop {}\x1b[K",
        b.description,
        state,
        b.radio.channel,
        b.ping_rtt_us.unwrap_or(0),
        s.online_count(),
        s.gifts.len(),
        match s.asleep_count() {
            0 => String::new(),
            n => format!(", {n} asleep"),
        },
        s.link.frames_per_s,
        s.link.show_packets_per_s,
        s.link.show_dropped,
        s.link.polls_per_s,
        s.link.poll_ok_per_s,
        s.link.tx_failed,
        b.stats.tx_sent,
        b.stats.tx_acked,
        b.stats.tx_failed,
        b.stats.events_dropped,
    );
    if s.waking {
        println!("waking dozing gifts…\x1b[K");
    }
    if s.discovery.sweeping {
        println!("scanning {:.0}%\x1b[K", s.discovery.sweep_progress * 100.0);
    }
    println!("\x1b[K");
}

// ---------------------------------------------------------------------------
// OTA
// ---------------------------------------------------------------------------

fn ota(link: &Link, path: &std::path::Path, targets: &[String], force: bool, sim: bool) -> Res {
    let image = std::fs::read(path).map_err(|x| format!("{}: {x}", path.display()))?;
    let mut snap = link.snapshot();
    // Default targets: every gift found online, or asleep (the update wakes
    // them). One more sweep if the first found nothing (seen on a host so
    // busy that the initial sweep missed the only gift).
    let reachable = |s: &Snapshot| s.gifts.iter().filter(|g| g.online || g.asleep).map(|g| g.uid).collect::<Vec<Uid>>();
    if targets.is_empty() && reachable(&snap).is_empty() && !sim {
        let _ = link.rescan().wait();
        snap = link.snapshot();
    }
    let uids: Vec<Uid> = if targets.is_empty() {
        reachable(&snap)
    } else {
        let mut v = Vec::new();
        for t in targets {
            for u in snap.resolve(t).map_err(e)? {
                if !v.contains(&u) {
                    v.push(u);
                }
            }
        }
        v
    };
    if uids.is_empty() {
        return Err("no target gifts online".into());
    }
    let mut p = link.start_ota_with(image, uids, force);
    let mut lines = 0;
    let result = loop {
        let r = p.wait_timeout(Duration::from_millis(200));
        if let Some(o) = link.snapshot().ota.clone() {
            draw_ota(&o, &mut lines);
        }
        if let Some(r) = r {
            break r;
        }
    };
    if let Some(o) = link.snapshot().ota.clone() {
        draw_ota(&o, &mut lines);
    }
    let r = result.map_err(e)?;
    println!(
        "updated {} gift(s) to fw {} in {:.2} s ({} chunks at {:.0}/s, {} repair rounds){}",
        r.updated.len(),
        r.fw_version,
        r.duration.as_secs_f32(),
        r.chunks_sent,
        r.chunk_rate,
        r.rounds,
        if r.skipped.is_empty() {
            String::new()
        } else {
            format!(", {} already up to date", r.skipped.len())
        }
    );
    let phases: Vec<String> = r
        .phase_times
        .iter()
        .map(|(p, d)| format!("{p:?} {:.0} ms", d.as_secs_f64() * 1000.0))
        .collect();
    println!("  phases: {}", phases.join(", "));
    if sim {
        // The simulator runs in real time with the bridge's air timing and
        // the gifts' flash timings, so this is the hardware estimate.
        println!(
            "  estimated time on hardware: {:.2} s (simulated radio, bridge and flash timing)",
            r.duration.as_secs_f32()
        );
    }
    for (u, why) in &r.failed {
        println!("  FAILED {}: {why}", u.short());
    }
    if r.failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} gift(s) failed", r.failed.len()))
    }
}

fn rescue(link: &Link, sim: Option<&SimHandle>, seconds: u64) -> Res {
    println!(
        "Broadcasting BootHold on channel {} for {seconds} s: power-cycle the unreachable gift(s) now.",
        proto::DEFAULT_CHANNEL
    );
    if let Some(h) = sim {
        // Demo: replug every simulated gift whose app is unreachable.
        let gifts = h.gifts();
        let s = link.snapshot();
        for g in gifts {
            if !s.gift(&g.uid).is_some_and(|v| v.online) {
                h.power_cycle(g.index, Duration::from_millis(500));
            }
        }
    }
    let r = link
        .rescue(Duration::from_secs(seconds))
        .wait()
        .map_err(e)?;
    println!(
        "{} gift(s) in their bootloader ({} BootHold sent in {:.1} s):",
        r.in_bootloader.len(),
        r.boot_holds_sent,
        r.duration.as_secs_f32()
    );
    let s = link.snapshot();
    for u in &r.in_bootloader {
        let addr = s.gift(u).map(|g| g.node_addr).unwrap_or(0);
        let label = s.gift(u).map(|g| g.label()).unwrap_or_else(|| u.short());
        println!("  {label} at @{addr}");
    }
    println!("Update them with `lumigift ota <image>`.");
    Ok(())
}

fn draw_ota(o: &OtaProgress, lines: &mut usize) {
    if *lines > 0 {
        print!("\x1b[{}A", *lines);
    }
    let mut out = Vec::new();
    out.push(format!(
        "OTA fw {}  {:?}  round {}  {} chunks sent / {}  {:.1}s",
        o.fw_version,
        o.phase,
        o.round,
        o.chunks_sent,
        o.chunks,
        o.finished
            .unwrap_or_else(Instant::now)
            .duration_since(o.started)
            .as_secs_f32()
    ));
    for t in &o.targets {
        let w = 30usize;
        let fill = ((t.percent / 100.0) * w as f32).round() as usize;
        out.push(format!(
            "  {:<12} [{}{}] {:>5.1}% {:<18} retries {:<3} {}",
            t.label,
            "#".repeat(fill.min(w)),
            " ".repeat(w - fill.min(w)),
            t.percent,
            format!("{:?}", t.phase),
            t.retries,
            t.error.as_deref().unwrap_or("")
        ));
    }
    for l in &out {
        println!("{l}\x1b[K");
    }
    *lines = out.len();
    let _ = std::io::stdout().flush();
}
