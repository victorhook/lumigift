//! Lumigift Studio: the desktop app (fleet monitor, show studio, 3D stage, firmware updates).
//!
//! ```text
//! lumigift-studio [--sim [N]] [--audio FILE] [--show FILE.lgshow] [--firmware FILE.lgfw]
//! ```

mod app;
mod backend;
mod fonts;
mod health;
mod icons;
mod pages;
mod palette;
mod screenshot;
mod stage;
mod theme;
mod toasts;
mod widgets;

use std::path::PathBuf;

fn usage() -> ! {
    eprintln!(
        "usage: lumigift-studio [--sim [N]] [--audio FILE] [--show FILE.lgshow] [--firmware FILE.lgfw]\n\
         \x20      lumigift-studio --write-demo-wav OUT.wav | --write-test-firmware OUT.lgfw"
    );
    std::process::exit(2)
}

/// Developer helper: write the lumigift-audio synthetic demo track as 16-bit WAV.
fn write_demo_wav(path: &str) -> std::io::Result<()> {
    let t = lumigift_audio::synth::demo_track(44_100);
    let mut b = Vec::with_capacity(44 + t.samples.len() * 4);
    let n = t.samples.len() as u32;
    let data_len = n * 4;
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&44_100u32.to_le_bytes());
    b.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in &t.samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        b.extend_from_slice(&v.to_le_bytes());
        b.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, b)
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut opts = app::StartOptions {
        sim: None,
        open_audio: None,
        open_show: None,
        open_firmware: None,
        screenshot_dir: std::env::var_os("LUMIGIFT_SCREENSHOT").map(PathBuf::from),
    };
    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sim" => {
                let n = args.peek().and_then(|s| s.parse::<usize>().ok());
                if n.is_some() {
                    args.next();
                }
                opts.sim = Some(n.unwrap_or(10).clamp(1, 30));
            }
            "--audio" => opts.open_audio = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--show" => opts.open_show = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--firmware" => opts.open_firmware = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--screenshot" => opts.screenshot_dir = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--write-demo-wav" => {
                let p = args.next().unwrap_or_else(|| usage());
                write_demo_wav(&p).expect("write wav");
                return Ok(());
            }
            "--write-test-firmware" => {
                let p = args.next().unwrap_or_else(|| usage());
                let img = lumigift_core::sim::build_test_image(1_793_000_000, 21_000, 7);
                std::fs::write(p, img).expect("write image");
                return Ok(());
            }
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    let shot = opts.screenshot_dir.is_some();
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Lumigift Studio")
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1024.0, 640.0]),
        renderer: eframe::Renderer::Wgpu,
        persist_window: !shot,
        ..Default::default()
    };
    eframe::run_native("lumigift-studio", native, Box::new(move |cc| Ok(Box::new(app::App::new(cc, opts)))))
}
