//! Host-side metadata persistence (`gifts.json`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::MetadataLocation;
use crate::types::{GiftMeta, Uid};

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct File {
    version: u32,
    gifts: BTreeMap<String, GiftMeta>,
}

pub(crate) struct MetaStore {
    path: Option<PathBuf>,
    map: BTreeMap<Uid, GiftMeta>,
}

impl MetaStore {
    pub fn open(loc: &MetadataLocation) -> (Self, Option<String>) {
        let path = match loc {
            MetadataLocation::Disabled => None,
            MetadataLocation::Path(p) => Some(p.clone()),
            MetadataLocation::Default => default_path(),
        };
        let mut store = Self {
            path,
            map: BTreeMap::new(),
        };
        let mut warning = None;
        if let Some(p) = &store.path {
            match std::fs::read_to_string(p) {
                Ok(s) => match serde_json::from_str::<File>(&s) {
                    Ok(f) => {
                        for (k, v) in f.gifts {
                            if let Ok(u) = k.parse::<Uid>() {
                                store.map.insert(u, v);
                            }
                        }
                    }
                    Err(e) => warning = Some(format!("ignoring unreadable {}: {e}", p.display())),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warning = Some(format!("cannot read {}: {e}", p.display())),
            }
        }
        (store, warning)
    }

    pub fn get(&self, uid: &Uid) -> GiftMeta {
        self.map.get(uid).cloned().unwrap_or_default()
    }

    pub fn all(&self) -> impl Iterator<Item = (&Uid, &GiftMeta)> {
        self.map.iter()
    }

    /// Apply `f` and save if anything changed.
    pub fn update(&mut self, uid: Uid, f: impl FnOnce(&mut GiftMeta)) -> Result<bool, String> {
        let e = self.map.entry(uid).or_default();
        let before = e.clone();
        f(e);
        if *e == before {
            return Ok(false);
        }
        self.save().map(|_| true)
    }

    fn save(&self) -> Result<(), String> {
        let Some(p) = &self.path else { return Ok(()) };
        let file = File {
            version: 1,
            gifts: self
                .map
                .iter()
                .map(|(k, v)| (k.to_hex(), v.clone()))
                .collect(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, p).map_err(|e| format!("{}: {e}", p.display()))
    }
}

pub fn default_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "lumigift").map(|d| d.config_dir().join("gifts.json"))
}
