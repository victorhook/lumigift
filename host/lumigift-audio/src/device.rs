//! cpal device lookup shared by [`crate::Player`] and
//! [`crate::live::CpalInputSource`].

use cpal::traits::{DeviceTrait, HostTrait};

use crate::{Error, Result};

pub(crate) fn device_error(e: impl std::fmt::Display) -> Error {
    Error::Device(e.to_string())
}

/// The device's human-readable name, if it has one.
pub(crate) fn device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_string())
}

/// `(id, name, is_default)` for each of `devices` that has an id; the name
/// falls back to the id.
pub(crate) fn describe(
    devices: impl Iterator<Item = cpal::Device>,
    default: Option<cpal::Device>,
) -> Vec<(String, String, bool)> {
    let default_id = default.and_then(|d| d.id().ok()).map(|i| i.to_string());
    devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = device_name(&d).unwrap_or_else(|| id.clone());
            let is_default = Some(&id) == default_id.as_ref();
            Some((id, name, is_default))
        })
        .collect()
}

/// The device with the given id on the default host, or `default(host)` for
/// `None`. `kind` ("input" / "output") is used in the error message.
pub(crate) fn find(
    id: Option<&str>,
    default: impl FnOnce(&cpal::Host) -> Option<cpal::Device>,
    kind: &str,
) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match id {
        None => default(&host),
        Some(id) => {
            let parsed: cpal::DeviceId = id.parse().map_err(|e: cpal::Error| device_error(e))?;
            host.device_by_id(&parsed)
        }
    }
    .ok_or_else(|| Error::Device(format!("{kind} device not found")))
}
