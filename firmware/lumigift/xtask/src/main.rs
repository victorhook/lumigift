//! `cargo xtask <command>` (run from `firmware/lumigift`):
//!
//! * `build`: release build of bootloader + app.
//! * `image [--version N] [--elf PATH]`: build the app and write
//!   `target/lumigift-app.img` (ImageHeader padded to 256 B + app bin) and `.bin`.
//! * `size`: flash/RAM usage of both images against the budgets, and a check
//!   that the RAM functions (`.data.ramfunc.*`) never branch back into flash.
//! * `flash [--dry-run] [--probe VID:PID[:SERIAL]]`: one-time SWD flash with
//!   probe-rs: bootloader ELF, then the app image at 0x0800_2800, then reset.
//! * `unprotect [--dry-run]`: remove the bootloader's write protection (WRP
//!   area A) over SWD with STM32CubeProgrammer, so a new bootloader can be
//!   flashed (it re-protects itself on first boot).
//!
//! The ELF is turned into a binary by a tiny ELF32 reader (PT_LOAD
//! segments by physical address), so no objcopy is needed.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

use lumigift_proto::ImageHeader;
use lumigift_proto::layout::{
    APP_MAX_SIZE, APP_REGION_BASE, APP_VECTORS, BOOT_BASE, BOOT_SHARED_SIZE, BOOT_SIZE, FLASH_BASE, FLASH_SIZE,
    IMAGE_HEADER_SIZE, RAM_BASE, RAM_SIZE,
};

const CHIP: &str = "STM32C011F6Px";
const TARGET: &str = "thumbv6m-none-eabi";
const HW_REV: u16 = 1;
const IMAGES: [&str; 2] = ["lumigift-boot", "lumigift-app"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn elf_path(name: &str) -> PathBuf {
    root().join("target").join(TARGET).join("release").join(name)
}

fn image_path() -> PathBuf {
    root().join("target/lumigift-app.img")
}

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("build") => build(&IMAGES),
        Some("image") => image(&args[1..]).map(|_| ()),
        Some("size") => build(&IMAGES).and_then(|_| size()),
        Some("flash") => flash(&args[1..]),
        Some("unprotect") => unprotect(&args[1..]),
        _ => {
            eprintln!(
                "usage: cargo xtask <build | image [--version N] [--elf PATH] | size | flash [--dry-run] [--probe SEL] | unprotect [--dry-run]>"
            );
            return ExitCode::from(2);
        }
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cmd: &mut Command) -> Result<()> {
    eprintln!("$ {cmd:?}");
    let st = cmd.status().map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    st.success().then_some(()).ok_or_else(|| format!("{:?} failed: {st}", cmd.get_program()))
}

fn build(packages: &[&str]) -> Result<()> {
    let mut c = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root()).args(["build", "--release"]);
    for p in packages {
        c.args(["-p", p]);
    }
    run(&mut c)
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

// -- image ---------------------------------------------------------------------

fn image(args: &[String]) -> Result<PathBuf> {
    let elf = match arg_value(args, "--elf") {
        Some(p) => PathBuf::from(p),
        None => {
            build(&["lumigift-app"])?;
            elf_path("lumigift-app")
        }
    };
    let fw_version = match arg_value(args, "--version") {
        Some(v) => v.parse::<u32>().map_err(|e| format!("--version: {e}"))?,
        None => match env::var("SOURCE_DATE_EPOCH") {
            Ok(v) => v.parse::<u32>().map_err(|e| format!("SOURCE_DATE_EPOCH: {e}"))?,
            Err(_) => SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32,
        },
    };
    let data = fs::read(&elf).map_err(|e| format!("{}: {e}", elf.display()))?;
    let bin = Elf::parse(&data)?.flatten(APP_VECTORS, APP_MAX_SIZE)?;
    let sp = u32::from_le_bytes(bin[0..4].try_into().unwrap());
    let reset = u32::from_le_bytes(bin[4..8].try_into().unwrap());
    if !(RAM_BASE..=RAM_BASE + RAM_SIZE).contains(&sp) || reset & 1 == 0 || reset < APP_VECTORS {
        return Err(format!("{} does not start with a vector table linked at {APP_VECTORS:#x}", elf.display()));
    }

    let git = git_hash();
    let h = ImageHeader::new(&bin, HW_REV, fw_version, git);
    let mut img = vec![0xFF; IMAGE_HEADER_SIZE as usize];
    img[..ImageHeader::ENCODED_LEN].copy_from_slice(&h.encode());
    img.extend_from_slice(&bin);
    assert!(ImageHeader::decode(&img).is_some());

    let out = image_path();
    fs::write(&out, &img).map_err(|e| format!("{}: {e}", out.display()))?;
    fs::write(out.with_extension("bin"), &bin).map_err(|e| e.to_string())?;
    println!(
        "{}: {} bytes (header {} + app {} / {} max), fw_version {}, git {}, app_crc {:#010x}",
        out.display(),
        img.len(),
        IMAGE_HEADER_SIZE,
        bin.len(),
        APP_MAX_SIZE,
        fw_version,
        String::from_utf8_lossy(&git),
        h.app_crc
    );
    Ok(out)
}

/// First 8 hex characters of HEAD as ASCII ("00000000" outside git).
fn git_hash() -> [u8; 8] {
    let out = Command::new("git").current_dir(root()).args(["rev-parse", "HEAD"]).output();
    let mut h = *b"00000000";
    if let Ok(o) = out
        && o.status.success()
        && o.stdout.len() >= 8
    {
        h.copy_from_slice(&o.stdout[..8]);
    }
    h
}

// -- size ----------------------------------------------------------------------

fn size() -> Result<()> {
    println!("{:<14} {:>8} {:>8} {:>6}   {:>6} {:>6}", "image", "flash", "budget", "used", "RAM", "budget");
    for (name, budget) in [("lumigift-boot", BOOT_SIZE - 16), ("lumigift-app", APP_MAX_SIZE)] {
        let path = elf_path(name);
        let data = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let elf = Elf::parse(&data)?;
        let (mut flash, mut ram) = (0u32, 0u32);
        for s in elf.sections() {
            if s.flags & SHF_ALLOC == 0 || s.size == 0 || s.name == ".bootinfo" {
                continue;
            }
            let in_flash = (FLASH_BASE..FLASH_BASE + FLASH_SIZE).contains(&s.addr);
            let in_ram = (RAM_BASE..RAM_BASE + RAM_SIZE).contains(&s.addr);
            if in_flash {
                flash += s.size;
            }
            if in_ram {
                ram += s.size;
                if s.typ == SHT_PROGBITS {
                    flash += s.size; // .data initialisers live in flash
                }
            }
        }
        let ram_budget = RAM_SIZE - BOOT_SHARED_SIZE;
        println!(
            "{name:<14} {flash:>8} {budget:>8} {:>5.1}%   {ram:>6} {ram_budget:>6}  (static; the rest is stack)",
            flash as f64 * 100.0 / budget as f64
        );
        if flash > budget {
            return Err(format!("{name} exceeds its flash budget"));
        }
        check_ram_functions(name, &elf)?;
    }
    println!("(bootloader budget excludes its 16-byte info block at {:#x})", BOOT_BASE + BOOT_SIZE - 16);
    Ok(())
}

/// Functions placed in RAM (`.data.ramfunc.*`) run while the flash is busy,
/// so they must never branch into flash: scan their Thumb code for `BL`
/// (32-bit, PC-relative) and `BLX Rm` and check every target.
fn check_ram_functions(name: &str, elf: &Elf) -> Result<()> {
    let ram = RAM_BASE..RAM_BASE + RAM_SIZE;
    let funcs: Vec<Symbol> =
        elf.symbols().into_iter().filter(|s| s.is_func && s.size > 0 && ram.contains(&(s.addr & !1))).collect();
    let mut bad = Vec::new();
    for f in &funcs {
        let start = f.addr & !1;
        let code = elf.bytes_at(start, f.size).ok_or_else(|| format!("{name}: cannot read {}", f.name))?;
        let hw = |i: usize| u16::from_le_bytes([code[i], code[i + 1]]);
        let mut i = 0;
        while i + 2 <= code.len() {
            let h = hw(i);
            if h & 0xF800 == 0xF000 && i + 4 <= code.len() && hw(i + 2) & 0xD000 == 0xD000 {
                // BL: S:I1:I2:imm10:imm11:0, I1 = !(J1 ^ S), I2 = !(J2 ^ S).
                let l = hw(i + 2) as u32;
                let s = (h as u32 >> 10) & 1;
                let j1 = (l >> 13) & 1;
                let j2 = (l >> 11) & 1;
                let i1 = !(j1 ^ s) & 1;
                let i2 = !(j2 ^ s) & 1;
                let mut off = (s << 24) | (i1 << 23) | (i2 << 22) | ((h as u32 & 0x3FF) << 12) | ((l & 0x7FF) << 1);
                if s == 1 {
                    off |= 0xFE00_0000;
                }
                let target = (start + i as u32 + 4).wrapping_add(off);
                if !ram.contains(&target) {
                    bad.push(format!("{} + {i:#x}: BL {target:#010x}", f.name));
                }
                i += 4;
                continue;
            }
            if h & 0xFF87 == 0x4780 {
                bad.push(format!("{} + {i:#x}: BLX r{} (indirect call)", f.name, (h >> 3) & 0xF));
            }
            i += 2;
        }
    }
    if !funcs.is_empty() {
        let total: u32 = funcs.iter().map(|f| f.size).sum();
        println!("{name:<14} {} RAM function(s), {total} B, no calls into flash: {}", funcs.len(), bad.is_empty());
    }
    if bad.is_empty() { Ok(()) } else { Err(format!("{name}: RAM code calls into flash:\n  {}", bad.join("\n  "))) }
}

// -- unprotect -----------------------------------------------------------------

/// STM32CubeProgrammer CLI (the WRP option bytes are not exposed by probe-rs).
fn cube_programmer() -> PathBuf {
    let home = env::var("HOME").unwrap_or_default();
    let local = PathBuf::from(home).join("opt/STM32CubeProgrammer/bin/STM32_Programmer_CLI");
    if local.exists() { local } else { PathBuf::from("STM32_Programmer_CLI") }
}

fn unprotect(args: &[String]) -> Result<()> {
    // WRP1A_STRT > WRP1A_END disables WRP area A (RDP must be level 0).
    let cli = cube_programmer();
    let cmd = ["-c", "port=SWD", "mode=UR", "-ob", "WRP1A_STRT=0xF", "WRP1A_END=0x0"];
    println!("{} {}", cli.display(), cmd.join(" "));
    if args.iter().any(|a| a == "--dry-run") {
        return Ok(());
    }
    run(Command::new(&cli).args(cmd))
}

// -- flash ---------------------------------------------------------------------

fn flash(args: &[String]) -> Result<()> {
    let dry = args.iter().any(|a| a == "--dry-run");
    build(&["lumigift-boot"])?;
    let img = image(&[])?;
    let boot = elf_path("lumigift-boot");
    let mut common = format!("--chip {CHIP}");
    if let Some(p) = arg_value(args, "--probe") {
        common += &format!(" --probe {p}");
    }
    let cmds: Vec<Vec<String>> = [
        // Writes pages 0..=4 (bootloader + info block) only. Fails while the
        // bootloader is write-protected (it protects itself on first boot):
        // run `cargo xtask unprotect` first to re-flash it.
        format!("download {common} {}", boot.display()),
        // Writes the app region pages it covers; the config page is untouched.
        format!("download {common} --binary-format bin --base-address {APP_REGION_BASE:#010x} {}", img.display()),
        format!("reset {common}"),
    ]
    .iter()
    .map(|c| c.split(' ').map(String::from).collect())
    .collect();
    let have_probe_rs = Command::new("probe-rs").arg("--version").output().is_ok();
    if dry || !have_probe_rs {
        if !have_probe_rs {
            eprintln!("probe-rs not found (install: https://probe.rs/docs/getting-started/installation/); run:");
        }
        for c in &cmds {
            println!("probe-rs {}", c.join(" "));
        }
        return if dry { Ok(()) } else { Err("probe-rs missing".into()) };
    }
    for c in &cmds {
        run(Command::new("probe-rs").args(c))?;
    }
    Ok(())
}

// -- minimal ELF32 little-endian reader ------------------------------------------

const PT_LOAD: u32 = 1;
const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHF_ALLOC: u32 = 2;

struct Elf<'a> {
    d: &'a [u8],
}

struct Section {
    name: String,
    typ: u32,
    flags: u32,
    addr: u32,
    offset: u32,
    size: u32,
    link: u32,
}

struct Symbol {
    name: String,
    addr: u32,
    size: u32,
    is_func: bool,
}

impl<'a> Elf<'a> {
    fn parse(d: &'a [u8]) -> Result<Self> {
        if d.len() < 52 || &d[..4] != b"\x7fELF" || d[4] != 1 || d[5] != 1 {
            return Err("not a 32-bit little-endian ELF file".into());
        }
        Ok(Self { d })
    }
    fn u16(&self, o: usize) -> u16 {
        u16::from_le_bytes([self.d[o], self.d[o + 1]])
    }
    fn u32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.d[o..o + 4].try_into().unwrap())
    }
    /// NUL-terminated string at file offset `o`.
    fn cstr(&self, o: usize) -> String {
        let s = &self.d[o..];
        String::from_utf8_lossy(&s[..s.iter().position(|&b| b == 0).unwrap_or(0)]).into_owned()
    }

    /// Lay out the PT_LOAD segments by physical (load) address relative to
    /// `base`, gaps filled with 0xFF.
    fn flatten(&self, base: u32, max: u32) -> Result<Vec<u8>> {
        let (phoff, phentsize, phnum) = (self.u32(28) as usize, self.u16(42) as usize, self.u16(44) as usize);
        let mut out: Vec<u8> = Vec::new();
        for i in 0..phnum {
            let p = phoff + i * phentsize;
            let (typ, offset, paddr, filesz) =
                (self.u32(p), self.u32(p + 4) as usize, self.u32(p + 12), self.u32(p + 16) as usize);
            if typ != PT_LOAD || filesz == 0 {
                continue;
            }
            if paddr < base || paddr + filesz as u32 > base + max {
                return Err(format!("segment at {paddr:#x}+{filesz:#x} outside {base:#x}..{:#x}", base + max));
            }
            let start = (paddr - base) as usize;
            if out.len() < start + filesz {
                out.resize(start + filesz, 0xFF);
            }
            out[start..start + filesz].copy_from_slice(&self.d[offset..offset + filesz]);
        }
        if out.len() < 8 {
            return Err("no loadable segments".into());
        }
        Ok(out)
    }

    fn sections(&self) -> Vec<Section> {
        let (shoff, shentsize, shnum, shstrndx) =
            (self.u32(32) as usize, self.u16(46) as usize, self.u16(48) as usize, self.u16(50) as usize);
        let strtab = self.u32(shoff + shstrndx * shentsize + 16) as usize;
        (0..shnum)
            .map(|i| {
                let s = shoff + i * shentsize;
                Section {
                    name: self.cstr(strtab + self.u32(s) as usize),
                    typ: self.u32(s + 4),
                    flags: self.u32(s + 8),
                    addr: self.u32(s + 12),
                    offset: self.u32(s + 16),
                    size: self.u32(s + 20),
                    link: self.u32(s + 24),
                }
            })
            .collect()
    }

    /// `size` bytes at virtual address `addr` from an allocated PROGBITS section.
    fn bytes_at(&self, addr: u32, size: u32) -> Option<&[u8]> {
        let secs = self.sections();
        let s = secs.iter().find(|s| {
            s.typ == SHT_PROGBITS && s.flags & SHF_ALLOC != 0 && addr >= s.addr && addr + size <= s.addr + s.size
        })?;
        let o = (s.offset + addr - s.addr) as usize;
        self.d.get(o..o + size as usize)
    }

    fn symbols(&self) -> Vec<Symbol> {
        let secs = self.sections();
        let Some(symtab) = secs.iter().find(|s| s.typ == SHT_SYMTAB) else {
            return Vec::new();
        };
        let strtab = secs[symtab.link as usize].offset as usize;
        (0..(symtab.size / 16) as usize)
            .map(|k| {
                let e = symtab.offset as usize + k * 16;
                Symbol {
                    name: self.cstr(strtab + self.u32(e) as usize),
                    addr: self.u32(e + 4),
                    size: self.u32(e + 8),
                    is_func: self.d[e + 12] & 0xF == 2,
                }
            })
            .collect()
    }
}
