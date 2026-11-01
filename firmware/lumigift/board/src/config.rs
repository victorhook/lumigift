//! `ConfigRecord` log in the config page (see `lumigift_logic::config_log`).

use lumigift_logic::config_log::{next_slot, sanitize, scan};
use lumigift_proto::layout::{CONFIG_BASE, ConfigRecord, page_of};

use crate::flash;

pub struct ConfigStore {
    pub current: ConfigRecord,
    next_free: usize,
}

impl ConfigStore {
    /// Load the current record, or `ConfigRecord::defaults(uid)`.
    pub fn load(uid: &[u8; 12]) -> Self {
        let d = ConfigRecord::defaults(uid);
        let s = scan(|i| {
            let mut b = [0u8; 16];
            flash::read(CONFIG_BASE + (i * ConfigRecord::SIZE) as u32, &mut b);
            b
        });
        Self { current: sanitize(s.current.unwrap_or(d), &d), next_free: s.next_free }
    }

    /// Append `rec` (erasing the page first when full) and make it current.
    /// Stalls the CPU ~0.2 ms, or ~25 ms when the page is erased.
    pub fn store(&mut self, mut rec: ConfigRecord) -> bool {
        rec.seal();
        let (erase, slot) = next_slot(self.next_free);
        if erase && !flash::erase_page(page_of(CONFIG_BASE)) {
            return false;
        }
        let b = rec.to_bytes();
        let addr = CONFIG_BASE + (slot * ConfigRecord::SIZE) as u32;
        let ok =
            flash::program(addr, b[..8].try_into().unwrap()) && flash::program(addr + 8, b[8..].try_into().unwrap());
        self.next_free = slot + 1;
        if ok {
            self.current = rec;
        }
        ok
    }
}
