//! Resolving user-typed gift references (slot, address, UID prefix, name).

use crate::types::{LinkError, Snapshot, Uid};

pub(crate) fn resolve(snap: &Snapshot, sel: &str) -> Result<Vec<Uid>, LinkError> {
    let s = sel.trim();
    let none = || LinkError::NoSuchGift(s.to_owned());
    if s.is_empty() {
        return Err(none());
    }
    let lower = s.to_ascii_lowercase();
    if lower == "all" || lower == "*" {
        return Ok(snap.gifts.iter().map(|g| g.uid).collect());
    }
    let one = |v: Vec<Uid>| if v.is_empty() { Err(none()) } else { Ok(v) };
    let by_slot = |n: u8| {
        snap.gifts
            .iter()
            .filter(|g| g.slot == Some(n))
            .map(|g| g.uid)
            .collect::<Vec<_>>()
    };
    let by_addr = |n: u8| {
        snap.gifts
            .iter()
            .filter(|g| g.node_addr == n)
            .map(|g| g.uid)
            .collect::<Vec<_>>()
    };

    if let Some(n) = lower
        .strip_prefix("slot:")
        .or_else(|| lower.strip_prefix('s'))
        .and_then(|r| r.parse::<u8>().ok())
    {
        return one(by_slot(n));
    }
    if let Some(n) = lower
        .strip_prefix("addr:")
        .or_else(|| lower.strip_prefix('@'))
        .or_else(|| lower.strip_prefix('a'))
        .and_then(|r| r.parse::<u8>().ok())
    {
        return one(by_addr(n));
    }
    if let Ok(n) = lower.parse::<u8>() {
        let v = by_slot(n);
        return if v.is_empty() { one(by_addr(n)) } else { Ok(v) };
    }
    let hex = lower.strip_prefix("uid:");
    let is_hex = |h: &str| h.len() >= 4 && h.chars().all(|c| c.is_ascii_hexdigit());
    if let Some(h) = hex.or_else(|| is_hex(&lower).then_some(lower.as_str())) {
        let v: Vec<Uid> = snap
            .gifts
            .iter()
            .filter(|g| {
                let x = g.uid.to_hex();
                x.starts_with(h) || x.ends_with(h)
            })
            .map(|g| g.uid)
            .collect();
        if !v.is_empty() || hex.is_some() {
            return match v.len() {
                0 => Err(none()),
                1 => Ok(v),
                _ => Err(LinkError::Other(format!(
                    "UID {h:?} is ambiguous ({} gifts)",
                    v.len()
                ))),
            };
        }
    }
    let exact: Vec<Uid> = snap
        .gifts
        .iter()
        .filter(|g| {
            g.meta
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(s))
        })
        .map(|g| g.uid)
        .collect();
    if !exact.is_empty() {
        return Ok(exact);
    }
    let prefix: Vec<Uid> = snap
        .gifts
        .iter()
        .filter(|g| {
            g.meta
                .name
                .as_deref()
                .is_some_and(|n| n.to_ascii_lowercase().starts_with(&lower))
        })
        .map(|g| g.uid)
        .collect();
    match prefix.len() {
        0 => Err(none()),
        1 => Ok(prefix),
        _ => Err(LinkError::Other(format!(
            "name {s:?} is ambiguous ({} gifts)",
            prefix.len()
        ))),
    }
}
