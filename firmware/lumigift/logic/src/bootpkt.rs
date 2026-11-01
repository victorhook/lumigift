//! Decoder for the packet subset the bootloader handles.
//!
//! `lumigift_proto::Packet::decode` also parses every show frame, which
//! costs ~1.6 KiB of flash the bootloader does not need to spend. This decoder
//! follows the same wire format for the packets the bootloader acts on;
//! the tests check it against `Packet::decode`.

use lumigift_proto::{OTA_CHUNK, OtaBegin, OtaChunk, Rgb, Unicast, kind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootPacket {
    BootHold,
    OtaBegin(OtaBegin),
    OtaChunk(OtaChunk),
    SetNodeAddr {
        uid: [u8; 12],
        node_addr: u8,
    },
    /// Poll, GetIdentity, Blink, Reboot, OtaQuery, OtaCommit.
    Unicast(Unicast),
    /// Show frames, `SetSlot`/`SetConfig` (app only), unknown kinds,
    /// malformed packets.
    Other,
}

fn u16_at(b: &[u8; 32], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8; 32], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Minimum length per kind (trailing bytes are tolerated, as in proto);
/// 0 = not a bootloader packet.
fn min_len(k: u8) -> usize {
    match k {
        kind::BOOT_HOLD => 1,
        kind::POLL | kind::GET_IDENTITY => 2,
        kind::REBOOT | kind::OTA_COMMIT => 3,
        kind::OTA_QUERY => 4,
        kind::BLINK => 7,
        kind::OTA_BEGIN => 10,
        kind::SET_NODE_ADDR => 14,
        kind::OTA_CHUNK => 4 + OTA_CHUNK,
        _ => 0,
    }
}

pub fn decode(data: &[u8]) -> BootPacket {
    let Some(&k) = data.first() else {
        return BootPacket::Other;
    };
    let need = min_len(k);
    if need == 0 || data.len() < need || data.len() > 32 {
        return BootPacket::Other;
    }
    // Work on a zero-padded copy: fixed offsets, no bounds-check paths.
    let mut b = [0u8; 32];
    b[..data.len()].copy_from_slice(data);
    let seq = b[1];
    match k {
        kind::BOOT_HOLD => BootPacket::BootHold,
        kind::OTA_BEGIN => {
            BootPacket::OtaBegin(OtaBegin { session: b[1], image_len: u32_at(&b, 2), image_crc: u32_at(&b, 6) })
        }
        kind::OTA_CHUNK => {
            let mut data = [0u8; OTA_CHUNK];
            data.copy_from_slice(&b[4..4 + OTA_CHUNK]);
            BootPacket::OtaChunk(OtaChunk { session: b[1], index: u16_at(&b, 2), data })
        }
        kind::SET_NODE_ADDR => {
            let mut uid = [0u8; 12];
            uid.copy_from_slice(&b[1..13]);
            BootPacket::SetNodeAddr { uid, node_addr: b[13] }
        }
        kind::POLL => BootPacket::Unicast(Unicast::Poll { seq }),
        kind::GET_IDENTITY => BootPacket::Unicast(Unicast::GetIdentity { seq }),
        kind::BLINK => BootPacket::Unicast(Unicast::Blink {
            seq,
            color: Rgb::new(b[2], b[3], b[4]),
            count: b[5],
            period_10ms: b[6],
        }),
        kind::REBOOT => BootPacket::Unicast(Unicast::Reboot { seq, into_bootloader: b[2] != 0 }),
        kind::OTA_QUERY => BootPacket::Unicast(Unicast::OtaQuery { seq, session: b[2], window: b[3] }),
        _ => BootPacket::Unicast(Unicast::OtaCommit { seq, session: b[2] }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumigift_proto::{Broadcast, MAX_PAYLOAD, MusicFrame, Packet};

    fn expected(p: &Packet) -> BootPacket {
        match *p {
            Packet::Broadcast(Broadcast::BootHold) => BootPacket::BootHold,
            Packet::Broadcast(Broadcast::OtaBegin(o)) => BootPacket::OtaBegin(o),
            Packet::Broadcast(Broadcast::OtaChunk(c)) => BootPacket::OtaChunk(c),
            Packet::Broadcast(Broadcast::SetNodeAddr { uid, node_addr }) => BootPacket::SetNodeAddr { uid, node_addr },
            Packet::Unicast(Unicast::SetSlot { .. } | Unicast::SetConfig { .. }) => BootPacket::Other,
            Packet::Unicast(u) => BootPacket::Unicast(u),
            _ => BootPacket::Other,
        }
    }

    #[test]
    fn agrees_with_proto_decoder() {
        let mut data = [0u8; OTA_CHUNK];
        for (i, d) in data.iter_mut().enumerate() {
            *d = i as u8 * 3;
        }
        let packets = [
            Packet::Broadcast(Broadcast::BootHold),
            Packet::Broadcast(Broadcast::OtaBegin(OtaBegin {
                session: 9,
                image_len: 0x1234_5678,
                image_crc: 0xDEAD_BEEF,
            })),
            Packet::Broadcast(Broadcast::OtaChunk(OtaChunk { session: 2, index: 938, data })),
            Packet::Broadcast(Broadcast::SetNodeAddr { uid: [7, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11], node_addr: 42 }),
            Packet::Broadcast(Broadcast::Sync { seq: 1, time_us: 5 }),
            Packet::Broadcast(Broadcast::Music { seq: 1, time_us: 5, frame: MusicFrame::default() }),
            Packet::Unicast(Unicast::Poll { seq: 3 }),
            Packet::Unicast(Unicast::GetIdentity { seq: 4 }),
            Packet::Unicast(Unicast::Blink { seq: 5, color: Rgb::new(1, 2, 3), count: 4, period_10ms: 50 }),
            Packet::Unicast(Unicast::SetSlot { seq: 6, slot: 7 }),
            Packet::Unicast(Unicast::Reboot { seq: 7, into_bootloader: true }),
            Packet::Unicast(Unicast::Reboot { seq: 8, into_bootloader: false }),
            Packet::Unicast(Unicast::SetConfig { seq: 9, key: 3, value: 0xBEEF }),
            Packet::Unicast(Unicast::OtaQuery { seq: 10, session: 11, window: 3 }),
            Packet::Broadcast(Broadcast::enter_bootloader(&[1, 2, 3])),
            Packet::Unicast(Unicast::OtaCommit { seq: 12, session: 13 }),
        ];
        for p in packets {
            let mut buf = [0u8; MAX_PAYLOAD];
            let n = p.encode(&mut buf);
            assert_eq!(decode(&buf[..n]), expected(&p), "{p:?}");
            // Every truncation that proto rejects is `Other` here too.
            for cut in 0..n {
                let proto = Packet::decode(&buf[..cut]);
                let mine = decode(&buf[..cut]);
                match proto {
                    Ok(q) => assert_eq!(mine, expected(&q), "cut {cut} of {p:?}"),
                    Err(_) => assert_eq!(mine, BootPacket::Other, "cut {cut} of {p:?}"),
                }
            }
        }
        assert_eq!(decode(&[0x99, 1, 2]), BootPacket::Other);
        assert_eq!(decode(&[kind::POLL; 33]), BootPacket::Other, "oversized");
    }
}
