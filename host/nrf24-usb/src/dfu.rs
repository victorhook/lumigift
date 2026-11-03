//! Updating the bridge's own firmware over USB.
//!
//! The bridge has a USB DFU bootloader (`firmware/nrf24-usb-bridge/boot`).
//! [`update`] switches a running bridge into it (`DFU_DETACH` on the app's
//! DFU runtime interface), erases and writes the new app with DfuSe, reads it
//! back, and leaves DFU mode; the bridge then restarts into the new app.
//! It is the same protocol as
//! `dfu-util -d 1209:0002 -a 0 -s 0x08004000:leave -D app.bin`.
//!
//! The bootloader itself is never written, and the app's first page (its
//! vector table) is only committed at the very end, so an interrupted update
//! leaves the bridge waiting in DFU mode: run [`update`] again.

use std::num::NonZeroU8;
use std::time::{Duration, Instant};

use nrf24_usb_proto::{self as proto, dfu};
use nusb::MaybeFuture;
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};

use crate::{Bridge, Error, Result};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
/// Time for the bridge to reset and enumerate in its other mode.
const REENUMERATE_TIMEOUT: Duration = Duration::from_secs(8);
const LANG_US_ENGLISH: u16 = 0x0409;

// DFU states (DFU 1.1 table A.1).
const DFU_IDLE: u8 = 2;
const DFU_DNLOAD_IDLE: u8 = 5;
const DFU_MANIFEST_SYNC: u8 = 6;
const DFU_MANIFEST: u8 = 7;
const DFU_MANIFEST_WAIT_RESET: u8 = 8;
#[cfg(test)]
const DFU_UPLOAD_IDLE: u8 = 9;
const DFU_ERROR: u8 = 10;

/// A firmware image: bytes to write starting at `base`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    /// Load address, `None` for a raw binary (written at the start of the
    /// device's writable flash).
    pub base: Option<u32>,
    pub data: Vec<u8>,
}

impl Firmware {
    /// An ELF file (its loadable segments, by load address) or a raw binary.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.starts_with(b"\x7fELF") {
            parse_elf(bytes)
        } else if bytes.is_empty() {
            Err(Error::Dfu("empty firmware file".into()))
        } else {
            Ok(Self { base: None, data: bytes.to_vec() })
        }
    }
}

/// ELF32 little-endian: concatenate the `PT_LOAD` segments that carry file
/// data at their physical (load) addresses, gaps filled with 0xFF.
fn parse_elf(b: &[u8]) -> Result<Firmware> {
    let bad = |what: &str| Error::Dfu(format!("not a usable firmware ELF: {what}"));
    if b.len() < 52 || b[4] != 1 || b[5] != 1 {
        return Err(bad("expected 32-bit little-endian"));
    }
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let (phoff, phentsize, phnum) = (
        u32_at(0x1C).ok_or_else(|| bad("header"))? as usize,
        u16_at(0x2A).ok_or_else(|| bad("header"))? as usize,
        u16_at(0x2C).ok_or_else(|| bad("header"))? as usize,
    );
    let mut segs = Vec::new();
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        let field = |o: usize| u32_at(ph + o).ok_or_else(|| bad("program header"));
        const PT_LOAD: u32 = 1;
        let (ty, offset, paddr, filesz) = (field(0)?, field(4)? as usize, field(12)?, field(16)? as usize);
        if ty != PT_LOAD || filesz == 0 {
            continue;
        }
        let data = b.get(offset..offset + filesz).ok_or_else(|| bad("segment past end of file"))?;
        segs.push((paddr, data));
    }
    let start = segs.iter().map(|s| s.0).min().ok_or_else(|| bad("no loadable segments"))?;
    let end = segs.iter().map(|s| s.0 as u64 + s.1.len() as u64).max().unwrap_or(0);
    if end - start as u64 > 16 << 20 {
        return Err(bad("segments span more than 16 MiB (RAM addresses?)"));
    }
    let mut data = vec![0xFF; (end - start as u64) as usize];
    for (addr, seg) in segs {
        let o = (addr - start) as usize;
        data[o..o + seg.len()].copy_from_slice(seg);
    }
    Ok(Firmware { base: Some(start), data })
}

/// One run of equal pages from a DfuSe memory layout string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start: u32,
    pub pages: u32,
    pub page_size: u32,
    pub writable: bool,
}

impl Region {
    pub fn end(&self) -> u32 {
        self.start + self.pages * self.page_size
    }
}

/// Parse a DfuSe layout string such as `@Flash/0x08000000/16*001Ka,48*001Kg`.
pub fn parse_layout(s: &str) -> Option<Vec<Region>> {
    let mut parts = s.trim_start_matches('@').split('/');
    let _name = parts.next()?;
    let mut addr = u32::from_str_radix(parts.next()?.trim().trim_start_matches("0x"), 16).ok()?;
    let mut out = Vec::new();
    for area in parts.next()?.split(',') {
        let (count, rest) = area.trim().split_once('*')?;
        let pages: u32 = count.trim().parse().ok()?;
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let mut size: u32 = rest[..digits].parse().ok()?;
        let mut tail = rest[digits..].chars();
        let mut kind = tail.next()?;
        match kind {
            'K' => size *= 1024,
            'M' => size *= 1024 * 1024,
            ' ' | 'B' => {}
            _ => {}
        }
        if matches!(kind, 'K' | 'M' | ' ' | 'B') {
            kind = tail.next()?;
        }
        // DfuSe letters a..g: bit 0 readable, bit 1 erasable, bit 2 writable.
        let bits = (kind as u32).checked_sub('a' as u32 - 1)?;
        out.push(Region { start: addr, pages, page_size: size, writable: bits & 4 != 0 });
        addr = addr.checked_add(pages * size)?;
    }
    Some(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Switching the running bridge into its bootloader.
    Detach,
    Erase,
    Write,
    /// Reading the image back.
    Verify,
    /// Leaving DFU mode and waiting for the new app.
    Restart,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub serial: Option<String>,
    pub base: u32,
    pub bytes: usize,
    pub elapsed: Duration,
    /// `Info::fw_version` of the restarted app, if it came back in time.
    pub fw_version: Option<u16>,
}

/// Control transfers on a DFU interface (real USB, or a model in tests).
pub trait DfuIo {
    fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<()>;
    fn control_in(&mut self, request: u8, value: u16, length: u16) -> Result<Vec<u8>>;
}

#[derive(Debug, Clone, Copy)]
struct Status {
    status: u8,
    poll: Duration,
    state: u8,
}

/// DfuSe download / upload over a [`DfuIo`].
pub struct DfuSe<T> {
    io: T,
    transfer_size: usize,
}

impl<T: DfuIo> DfuSe<T> {
    pub fn new(io: T, transfer_size: usize) -> Self {
        Self { io, transfer_size: transfer_size.max(1) }
    }

    fn status(&mut self) -> Result<Status> {
        let r = self.io.control_in(dfu::GETSTATUS, 0, 6)?;
        if r.len() < 6 {
            return Err(Error::Dfu("short GETSTATUS reply".into()));
        }
        Ok(Status {
            status: r[0],
            poll: Duration::from_millis(u32::from_le_bytes([r[1], r[2], r[3], 0]) as u64),
            state: r[4],
        })
    }

    /// Bring the device to dfuIDLE from any state it can leave.
    fn reset_state(&mut self) -> Result<()> {
        let st = self.status()?;
        match st.state {
            DFU_IDLE => return Ok(()),
            DFU_ERROR => self.io.control_out(dfu::CLRSTATUS, 0, &[])?,
            _ => self.io.control_out(dfu::ABORT, 0, &[])?,
        }
        let st = self.status()?;
        if st.state != DFU_IDLE {
            return Err(Error::Dfu(format!("device stuck in DFU state {}", st.state)));
        }
        Ok(())
    }

    /// After a DNLOAD: poll GETSTATUS (which starts the operation) until the
    /// device is idle again.
    fn finish_dnload(&mut self, what: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let st = self.status()?;
            match st.state {
                DFU_DNLOAD_IDLE => return Ok(()),
                DFU_ERROR => {
                    let _ = self.io.control_out(dfu::CLRSTATUS, 0, &[]);
                    return Err(Error::Dfu(format!("{what}: {}", status_text(st.status))));
                }
                _ if Instant::now() > deadline => {
                    return Err(Error::Dfu(format!("{what}: device busy for too long")));
                }
                _ => std::thread::sleep(st.poll),
            }
        }
    }

    fn special(&mut self, cmd: u8, addr: u32, what: &str) -> Result<()> {
        let mut d = [cmd, 0, 0, 0, 0];
        d[1..].copy_from_slice(&addr.to_le_bytes());
        self.io.control_out(dfu::DNLOAD, 0, &d)?;
        self.finish_dnload(what)
    }

    /// Erase the pages covering `[base, base + data.len())`, write `data`,
    /// read it back (except `skip_verify` bytes at the start, which the
    /// bootloader only commits at manifestation) and leave DFU mode.
    pub fn download(
        &mut self,
        base: u32,
        data: &[u8],
        page_size: u32,
        skip_verify: usize,
        progress: &mut dyn FnMut(Phase, usize, usize),
    ) -> Result<()> {
        self.reset_state()?;
        let end = base + data.len() as u32;
        let first = base / page_size * page_size;
        let pages: Vec<u32> = (first..end).step_by(page_size as usize).collect();
        for (i, &p) in pages.iter().enumerate() {
            progress(Phase::Erase, i, pages.len());
            self.special(dfu::DFUSE_ERASE_PAGE, p, &format!("erase 0x{p:08X}"))?;
        }
        progress(Phase::Erase, pages.len(), pages.len());

        self.special(dfu::DFUSE_SET_ADDRESS, base, "set address")?;
        for (i, chunk) in data.chunks(self.transfer_size).enumerate() {
            progress(Phase::Write, i * self.transfer_size, data.len());
            let block = u16::try_from(i + 2).map_err(|_| Error::Dfu("image too large".into()))?;
            self.io.control_out(dfu::DNLOAD, block, chunk)?;
            self.finish_dnload(&format!("write 0x{:08X}", base as usize + i * self.transfer_size))?;
        }
        progress(Phase::Write, data.len(), data.len());

        // Read back from dfuIDLE; the address pointer is still `base`.
        self.io.control_out(dfu::ABORT, 0, &[])?;
        for (i, chunk) in data.chunks(self.transfer_size).enumerate() {
            let at = i * self.transfer_size;
            progress(Phase::Verify, at, data.len());
            let block = (i + 2) as u16;
            let got = self.io.control_in(dfu::UPLOAD, block, self.transfer_size as u16)?;
            for (k, (&want, &have)) in chunk.iter().zip(got.iter()).enumerate() {
                if at + k >= skip_verify && want != have {
                    return Err(Error::Dfu(format!(
                        "verify failed at 0x{:08X}: wrote {want:02X}, read {have:02X}",
                        base as usize + at + k
                    )));
                }
            }
            if got.len() < chunk.len() {
                return Err(Error::Dfu("short read during verify".into()));
            }
        }
        progress(Phase::Verify, data.len(), data.len());

        // Leave: zero-length DNLOAD from dfuIDLE, then GETSTATUS starts the
        // manifestation (the device writes the first page and resets).
        progress(Phase::Restart, 0, 1);
        self.io.control_out(dfu::ABORT, 0, &[])?;
        self.io.control_out(dfu::DNLOAD, 2, &[])?;
        match self.status() {
            Ok(st) if st.state == DFU_ERROR => Err(Error::Dfu(format!("manifestation: {}", status_text(st.status)))),
            Ok(st) if [DFU_MANIFEST_SYNC, DFU_MANIFEST, DFU_MANIFEST_WAIT_RESET].contains(&st.state) => {
                // A second GETSTATUS reports a failed manifestation (bad image);
                // a vanished device means it already restarted.
                std::thread::sleep(st.poll.max(Duration::from_millis(60)));
                match self.status() {
                    Ok(st) if st.state == DFU_ERROR => {
                        Err(Error::Dfu(format!("manifestation: {}", status_text(st.status))))
                    }
                    _ => Ok(()),
                }
            }
            // Gone already: it reset into the new app.
            _ => Ok(()),
        }
    }
}

fn status_text(code: u8) -> String {
    let s = match code {
        0x00 => "OK",
        0x01 => "target error",
        0x02 => "file rejected",
        0x03 => "write error",
        0x04 => "erase error",
        0x05 => "not erased",
        0x06 => "program error",
        0x07 => "verify error",
        0x08 => "address out of range",
        0x09 => "incomplete download",
        0x0A => "image is not a valid app",
        0x0E => "unknown error",
        0x0F => "stalled request",
        _ => "error",
    };
    format!("{s} (status 0x{code:02X})")
}

/// DFU class requests on a claimed nusb interface.
struct UsbIo {
    intf: nusb::Interface,
    index: u16,
}

impl DfuIo for UsbIo {
    fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<()> {
        let req = ControlOut {
            control_type: ControlType::Class,
            recipient: Recipient::Interface,
            request,
            value,
            index: self.index,
            data,
        };
        Ok(self.intf.control_out(req, CONTROL_TIMEOUT).wait()?)
    }

    fn control_in(&mut self, request: u8, value: u16, length: u16) -> Result<Vec<u8>> {
        let req = ControlIn {
            control_type: ControlType::Class,
            recipient: Recipient::Interface,
            request,
            value,
            index: self.index,
            length,
        };
        Ok(self.intf.control_in(req, CONTROL_TIMEOUT).wait()?)
    }
}

fn is_bridge(d: &nusb::DeviceInfo) -> bool {
    d.vendor_id() == proto::USB_VID && (d.product_id() == proto::USB_PID || d.product_id() == dfu::USB_PID)
}

/// The bridge to update: the one with `serial`, or the only one connected
/// (in either mode).
fn find(serial: Option<&str>) -> Result<nusb::DeviceInfo> {
    let mut found: Vec<_> = nusb::list_devices()
        .wait()?
        .filter(is_bridge)
        .filter(|d| serial.is_none_or(|s| d.serial_number() == Some(s)))
        .collect();
    match found.len() {
        0 => Err(Error::NotFound(serial.map(str::to_owned))),
        1 => Ok(found.remove(0)),
        _ => Err(Error::Dfu("several bridges connected: pick one by serial number".into())),
    }
}

fn wait_for(pid: u16, serial: Option<&str>) -> Result<nusb::DeviceInfo> {
    let deadline = Instant::now() + REENUMERATE_TIMEOUT;
    loop {
        let dev = nusb::list_devices().wait()?.find(|d| {
            d.vendor_id() == proto::USB_VID
                && d.product_id() == pid
                && serial.is_none_or(|s| d.serial_number() == Some(s))
        });
        if let Some(d) = dev {
            return Ok(d);
        }
        if Instant::now() > deadline {
            return Err(Error::Timeout);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Open a device that may have just enumerated: udev grants access (the
/// `uaccess` ACL) a moment after the device appears, so retry briefly.
fn open_retrying(info: &nusb::DeviceInfo) -> Result<nusb::Device> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match info.open().wait() {
            Err(e) if e.kind() == nusb::ErrorKind::PermissionDenied && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            r => return Ok(r?),
        }
    }
}

/// Is there a bridge waiting in its bootloader?
pub fn in_bootloader(serial: Option<&str>) -> Result<bool> {
    Ok(find(serial)?.product_id() == dfu::USB_PID)
}

/// Replace the firmware of the bridge with `serial` (or the only bridge).
/// Works whether the bridge is running its app or already waits in DFU mode.
pub fn update(fw: &Firmware, serial: Option<&str>, progress: &mut dyn FnMut(Phase, usize, usize)) -> Result<Report> {
    let t0 = Instant::now();
    let mut info = find(serial)?;
    let serial = info.serial_number().map(str::to_owned);

    if info.product_id() == proto::USB_PID {
        progress(Phase::Detach, 0, 1);
        let rt = info
            .interfaces()
            .find(|i| i.class() == dfu::CLASS && i.subclass() == dfu::SUBCLASS && i.protocol() == dfu::PROTOCOL_RUNTIME)
            .map(|i| i.interface_number())
            .ok_or_else(|| {
                Error::Dfu("this bridge firmware has no USB update support; flash it once over SWD".into())
            })?;
        {
            let dev = open_retrying(&info)?;
            log::debug!("dfu: opened app ({:?})", t0.elapsed());
            let intf = dev.detach_and_claim_interface(rt).wait()?;
            log::debug!("dfu: claimed runtime interface {rt} ({:?})", t0.elapsed());
            // The bridge resets ~20 ms after accepting; a lost status stage is fine.
            let r = UsbIo { intf, index: rt as u16 }.control_out(dfu::DETACH, 1000, &[]);
            log::debug!("dfu: DETACH -> {r:?} ({:?})", t0.elapsed());
        }
        info = wait_for(dfu::USB_PID, serial.as_deref())?;
        log::debug!("dfu: bootloader enumerated ({:?})", t0.elapsed());
        progress(Phase::Detach, 1, 1);
    }

    let dev = open_retrying(&info)?;
    let (if_num, string_index, transfer_size) = {
        let cfg = dev.active_configuration().map_err(|e| Error::Usb(e.to_string()))?;
        let alt = cfg
            .interface_alt_settings()
            .find(|a| a.class() == dfu::CLASS && a.subclass() == dfu::SUBCLASS && a.protocol() == dfu::PROTOCOL_DFU_MODE)
            .ok_or_else(|| Error::Dfu("bootloader has no DFU interface".into()))?;
        let xfer = alt
            .descriptors()
            .find(|d| d.descriptor_type() == dfu::FUNCTIONAL_DESCRIPTOR && d.descriptor_len() >= 7)
            .map(|d| u16::from_le_bytes([d.as_bytes()[5], d.as_bytes()[6]]) as usize)
            .unwrap_or(256);
        (alt.interface_number(), alt.string_index(), xfer)
    };
    let layout = string_index
        .map(|i: NonZeroU8| dev.get_string_descriptor(i, LANG_US_ENGLISH, CONTROL_TIMEOUT).wait())
        .transpose()
        .map_err(|e| Error::Usb(e.to_string()))?
        .and_then(|s| parse_layout(&s))
        .ok_or_else(|| Error::Dfu("bootloader did not describe its flash layout".into()))?;
    let app = *layout
        .iter()
        .find(|r| r.writable)
        .ok_or_else(|| Error::Dfu("bootloader reports no writable flash".into()))?;
    let app_end = layout.iter().filter(|r| r.writable).map(Region::end).max().unwrap_or(app.end());

    let base = fw.base.unwrap_or(app.start);
    let end = base as u64 + fw.data.len() as u64;
    if base < app.start || end > app_end as u64 {
        return Err(Error::Dfu(format!(
            "image 0x{base:08X}..0x{end:08X} does not fit the app region 0x{:08X}..0x{app_end:08X} \
             (built for a different flash layout?)",
            app.start
        )));
    }
    let skip_verify = if base == app.start { app.page_size as usize } else { 0 };

    let intf = dev.detach_and_claim_interface(if_num).wait()?;
    let mut dfu = DfuSe::new(UsbIo { intf, index: if_num as u16 }, transfer_size);
    dfu.download(base, &fw.data, app.page_size, skip_verify, progress)?;
    drop(dfu);
    drop(dev);

    let fw_version = wait_for(proto::USB_PID, serial.as_deref()).ok().and_then(|_| {
        // Access is granted by udev a moment after the device appears.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match Bridge::open(serial.as_deref()) {
                Ok(mut b) => return b.get_info(Duration::from_secs(1)).ok().map(|i| i.fw_version),
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => return None,
            }
        }
    });
    progress(Phase::Restart, 1, 1);
    Ok(Report { serial, base, bytes: fw.data.len(), elapsed: t0.elapsed(), fw_version })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_string() {
        let l = parse_layout("@Flash/0x08000000/16*001Ka,48*001Kg").unwrap();
        assert_eq!(
            l,
            vec![
                Region { start: 0x0800_0000, pages: 16, page_size: 1024, writable: false },
                Region { start: 0x0800_4000, pages: 48, page_size: 1024, writable: true },
            ]
        );
        let st = parse_layout("@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg").unwrap();
        assert_eq!(st[1].start, 0x0801_0000);
        assert_eq!(st[2].page_size, 128 * 1024);
        assert!(parse_layout("garbage").is_none());
    }

    /// A 2-segment ELF32 with a RAM (.data) segment loaded from flash.
    fn elf(segs: &[(u32, &[u8])]) -> Vec<u8> {
        let mut b = vec![0u8; 52];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = 1;
        b[5] = 1;
        b[0x1C..0x20].copy_from_slice(&52u32.to_le_bytes());
        b[0x2A..0x2C].copy_from_slice(&32u16.to_le_bytes());
        b[0x2C..0x2E].copy_from_slice(&(segs.len() as u16).to_le_bytes());
        let mut off = 52 + 32 * segs.len();
        let mut payload = Vec::new();
        for (paddr, data) in segs {
            let mut ph = [0u8; 32];
            ph[0..4].copy_from_slice(&1u32.to_le_bytes());
            ph[4..8].copy_from_slice(&(off as u32).to_le_bytes());
            ph[8..12].copy_from_slice(&0x2000_0000u32.to_le_bytes());
            ph[12..16].copy_from_slice(&paddr.to_le_bytes());
            ph[16..20].copy_from_slice(&(data.len() as u32).to_le_bytes());
            b.extend_from_slice(&ph);
            payload.extend_from_slice(data);
            off += data.len();
        }
        b.extend_from_slice(&payload);
        b
    }

    #[test]
    fn elf_segments_by_load_address() {
        let f = Firmware::parse(&elf(&[(0x0800_4000, &[1, 2, 3, 4]), (0x0800_4008, &[9, 9])])).unwrap();
        assert_eq!(f.base, Some(0x0800_4000));
        assert_eq!(f.data, vec![1, 2, 3, 4, 0xFF, 0xFF, 0xFF, 0xFF, 9, 9]);
        let raw = Firmware::parse(&[7, 7, 7]).unwrap();
        assert_eq!(raw.base, None);
    }

    /// The bootloader's behaviour (usbd-dfu + its first-page deferral) as a
    /// model: DfuSe state machine over a 64 KiB flash with 16 locked pages.
    struct Model {
        flash: Vec<u8>,
        state: u8,
        status: u8,
        pointer: u32,
        pending: Option<Pending>,
        page0: Option<Vec<u8>>,
        manifested: bool,
        fail_write_at: Option<u32>,
    }

    #[derive(Clone)]
    enum Pending {
        Erase(u32),
        SetAddress(u32),
        Write(u32, Vec<u8>),
        Leave,
    }

    const BASE: u32 = 0x0800_0000;
    const APP: u32 = 0x0800_4000;
    const XFER: usize = 256;

    impl Model {
        fn new() -> Self {
            Self {
                flash: vec![0xFF; 64 * 1024],
                state: DFU_IDLE,
                status: 0,
                pointer: APP,
                pending: None,
                page0: None,
                manifested: false,
                fail_write_at: None,
            }
        }
        fn app(&self, addr: u32, len: usize) -> &[u8] {
            let o = (addr - BASE) as usize;
            &self.flash[o..o + len]
        }
        fn error(&mut self, status: u8) {
            self.state = DFU_ERROR;
            self.status = status;
        }
        fn execute(&mut self, p: Pending) {
            match p {
                Pending::Erase(a) => {
                    if !(APP..BASE + 0x1_0000).contains(&a) || a % 1024 != 0 {
                        return self.error(0x08);
                    }
                    if a == APP {
                        self.page0 = None;
                    }
                    let o = (a - BASE) as usize;
                    self.flash[o..o + 1024].fill(0xFF);
                }
                Pending::SetAddress(a) => self.pointer = a,
                Pending::Write(a, d) => {
                    if self.fail_write_at == Some(a) {
                        return self.error(0x06);
                    }
                    for (i, b) in d.into_iter().enumerate() {
                        let at = a + i as u32;
                        if at < APP + 1024 {
                            self.page0.get_or_insert_with(|| vec![0xFF; 1024])[(at - APP) as usize] = b;
                        } else {
                            let o = (at - BASE) as usize;
                            if self.flash[o] != 0xFF {
                                return self.error(0x05);
                            }
                            self.flash[o] = b;
                        }
                    }
                }
                Pending::Leave => {
                    if let Some(p0) = self.page0.take() {
                        let o = (APP - BASE) as usize;
                        self.flash[o..o + 1024].copy_from_slice(&p0);
                    }
                    self.manifested = true;
                    self.state = DFU_MANIFEST_WAIT_RESET;
                    return;
                }
            }
            self.state = 3; // dfuDNLOAD-SYNC
        }
    }

    impl DfuIo for &mut Model {
        fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<()> {
            match request {
                dfu::DNLOAD => {
                    if self.state != DFU_IDLE && self.state != DFU_DNLOAD_IDLE {
                        self.error(0x0F);
                        return Err(Error::Usb("stall".into()));
                    }
                    self.pending = Some(if data.is_empty() {
                        Pending::Leave
                    } else if value == 0 {
                        let a = u32::from_le_bytes(data[1..5].try_into().unwrap());
                        match data[0] {
                            dfu::DFUSE_ERASE_PAGE => Pending::Erase(a),
                            dfu::DFUSE_SET_ADDRESS => Pending::SetAddress(a),
                            _ => panic!("unknown DfuSe command"),
                        }
                    } else {
                        Pending::Write(self.pointer + (value as u32 - 2) * XFER as u32, data.to_vec())
                    });
                    self.state = if data.is_empty() { DFU_MANIFEST_SYNC } else { 3 };
                }
                dfu::ABORT => self.state = DFU_IDLE,
                dfu::CLRSTATUS => {
                    self.state = DFU_IDLE;
                    self.status = 0;
                }
                r => panic!("unexpected request {r}"),
            }
            Ok(())
        }

        fn control_in(&mut self, request: u8, value: u16, length: u16) -> Result<Vec<u8>> {
            match request {
                dfu::GETSTATUS => {
                    // Reply first (busy), then run the pending operation, like
                    // usbd-dfu; the next GETSTATUS sees the result.
                    let reply_state = match (self.state, &self.pending) {
                        (3, Some(_)) => 4,
                        (DFU_MANIFEST_SYNC, Some(_)) => DFU_MANIFEST,
                        (3, None) => {
                            self.state = DFU_DNLOAD_IDLE;
                            DFU_DNLOAD_IDLE
                        }
                        (s, _) => s,
                    };
                    let reply = vec![self.status, 0, 0, 0, reply_state, 0];
                    if let Some(p) = self.pending.take() {
                        self.execute(p);
                    }
                    Ok(reply)
                }
                dfu::UPLOAD => {
                    let a = self.pointer + (value as u32 - 2) * XFER as u32;
                    self.state = DFU_UPLOAD_IDLE;
                    Ok(self.app(a, length as usize).to_vec())
                }
                r => panic!("unexpected request {r}"),
            }
        }
    }

    fn image(len: usize) -> Vec<u8> {
        let mut d: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
        d[..8].copy_from_slice(&[0x00, 0x50, 0x00, 0x20, 0xF1, 0x40, 0x00, 0x08]);
        d
    }

    #[test]
    fn download_writes_verifies_and_commits_first_page_last() {
        let mut m = Model::new();
        let img = image(10_000);
        let mut phases = Vec::new();
        DfuSe::new(&mut m, XFER)
            .download(APP, &img, 1024, 1024, &mut |p, _, _| {
                if phases.last() != Some(&p) {
                    phases.push(p)
                }
            })
            .unwrap();
        assert!(m.manifested);
        assert_eq!(m.app(APP, img.len()), &img[..]);
        assert_eq!(phases, vec![Phase::Erase, Phase::Write, Phase::Verify, Phase::Restart]);
        // The bootloader pages were never touched.
        assert!(m.flash[..0x4000].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn device_error_is_reported_and_app_stays_invalid() {
        let mut m = Model::new();
        m.fail_write_at = Some(APP + 4 * XFER as u32);
        let err = DfuSe::new(&mut m, XFER).download(APP, &image(4000), 1024, 1024, &mut |_, _, _| {}).unwrap_err();
        assert!(err.to_string().contains("program error"), "{err}");
        assert!(!m.manifested);
        // The vector table page was erased and never committed: no valid app,
        // so the bootloader keeps waiting for the next attempt.
        assert!(m.app(APP, 8).iter().all(|&b| b == 0xFF));
        // A retry from the error state works.
        m.fail_write_at = None;
        DfuSe::new(&mut m, XFER).download(APP, &image(4000), 1024, 1024, &mut |_, _, _| {}).unwrap();
        assert!(m.manifested);
    }

    #[test]
    fn verify_catches_corruption() {
        struct Corrupt<'a>(&'a mut Model);
        impl DfuIo for Corrupt<'_> {
            fn control_out(&mut self, r: u8, v: u16, d: &[u8]) -> Result<()> {
                (&mut *self.0).control_out(r, v, d)
            }
            fn control_in(&mut self, r: u8, v: u16, l: u16) -> Result<Vec<u8>> {
                let mut out = (&mut *self.0).control_in(r, v, l)?;
                if r == dfu::UPLOAD && v == 9 {
                    out[3] ^= 0x10;
                }
                Ok(out)
            }
        }
        let mut m = Model::new();
        let err = DfuSe::new(Corrupt(&mut m), XFER).download(APP, &image(4000), 1024, 1024, &mut |_, _, _| {}).unwrap_err();
        assert!(err.to_string().contains("verify failed"), "{err}");
        assert!(!m.manifested);
    }
}
