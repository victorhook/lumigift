//! Capture from any cpal input device. On PipeWire/PulseAudio systems the
//! ALSA `pipewire`/`pulse` devices can record monitor sources (what another
//! application plays) when the default source is set to a monitor, e.g. with
//! `pactl set-default-source <sink>.monitor` or `pavucontrol`.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use super::{BlockSink, LiveSource};
use crate::device::{describe, device_error, device_name, find};
use crate::{Error, Result};

/// An audio input device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    /// Pass to [`CpalInputSource::new`].
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Live source reading from a cpal input device.
pub struct CpalInputSource {
    device: cpal::Device,
    config: cpal::StreamConfig,
    format: SampleFormat,
    name: String,
    stream: Option<cpal::Stream>,
}

impl CpalInputSource {
    /// Input devices of the default host.
    pub fn input_devices() -> Result<Vec<InputDevice>> {
        let host = cpal::default_host();
        let devices = host.input_devices().map_err(device_error)?;
        Ok(describe(devices, host.default_input_device())
            .into_iter()
            .map(|(id, name, is_default)| InputDevice {
                id,
                name,
                is_default,
            })
            .collect())
    }

    /// Open the device with the given [`InputDevice::id`] (`None` = default input).
    pub fn new(id: Option<&str>) -> Result<Self> {
        let device = find(id, |h| h.default_input_device(), "input")?;
        let cfg = device.default_input_config().map_err(device_error)?;
        Ok(Self {
            format: cfg.sample_format(),
            config: cfg.into(),
            name: device_name(&device).unwrap_or_else(|| "input".into()),
            device,
            stream: None,
        })
    }

    fn build<T>(&self, mut sink: BlockSink) -> Result<cpal::Stream>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let mut buf: Vec<f32> = Vec::new();
        self.device
            .build_input_stream(
                self.config,
                move |data: &[T], _info: &cpal::InputCallbackInfo| {
                    buf.clear();
                    buf.extend(data.iter().map(|s| s.to_sample::<f32>()));
                    sink(&buf);
                },
                |e| log::warn!("audio input error: {e}"),
                None,
            )
            .map_err(device_error)
    }
}

impl LiveSource for CpalInputSource {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    fn channels(&self) -> u16 {
        self.config.channels
    }

    fn start(&mut self, sink: BlockSink) -> Result<()> {
        let stream = match self.format {
            SampleFormat::F32 => self.build::<f32>(sink),
            SampleFormat::I16 => self.build::<i16>(sink),
            SampleFormat::I32 => self.build::<i32>(sink),
            SampleFormat::U16 => self.build::<u16>(sink),
            SampleFormat::U8 => self.build::<u8>(sink),
            SampleFormat::I8 => self.build::<i8>(sink),
            SampleFormat::F64 => self.build::<f64>(sink),
            other => return Err(Error::Device(format!("unsupported sample format {other}"))),
        }?;
        stream.play().map_err(device_error)?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) {
        self.stream = None;
    }
}
