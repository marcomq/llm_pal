//! Microphone capture: the cpal callback downmixes to mono and pushes into a lock-free ring.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Stream, StreamConfig};

use crate::Error;

const RING_SECONDS: u32 = 2;

pub struct Capture {
    _stream: Stream,
    pub sample_rate: u32,
    pub device_name: String,
    /// Samples dropped because the worker fell behind.
    pub overruns: Arc<AtomicUsize>,
}

fn name_of(dev: &cpal::Device) -> String {
    dev.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "?".into())
}

pub fn input_device_names() -> Vec<String> {
    cpal::default_host().input_devices().map(|d| d.map(|d| name_of(&d)).collect()).unwrap_or_default()
}

/// The input whose name contains `wanted` (case-insensitive), else the default input.
fn input_device(host: &cpal::Host, wanted: Option<&str>) -> Option<cpal::Device> {
    if let Some(w) = wanted {
        let w = w.to_lowercase();
        return host.input_devices().ok()?.find(|d| name_of(d).to_lowercase().contains(&w));
    }
    host.default_input_device()
}

pub fn start(device: Option<&str>) -> Result<(Capture, rtrb::Consumer<f32>), Error> {
    #[cfg(target_os = "android")]
    crate::android::init_context();
    let host = cpal::default_host();
    let dev = input_device(&host, device).ok_or_else(|| Error::Audio(format!("no input device {device:?}")))?;
    let default = dev.default_input_config().map_err(|e| Error::Audio(format!("input config: {e}")))?;
    let channels = default.channels().max(1);
    let sample_rate = default.sample_rate();
    let config = StreamConfig { channels, sample_rate, buffer_size: BufferSize::Default };

    let (mut tx, rx) = rtrb::RingBuffer::<f32>::new((sample_rate * RING_SECONDS) as usize);
    let overruns = Arc::new(AtomicUsize::new(0));
    let overruns_cb = overruns.clone();
    let ch = channels as usize;
    let stream = dev
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                for frame in data.chunks(ch) {
                    let mono = frame.iter().sum::<f32>() / frame.len() as f32;
                    if tx.push(mono).is_err() {
                        overruns_cb.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            |e| eprintln!("llm_pal: input stream error: {e}"),
            None,
        )
        .map_err(|e| Error::Audio(format!("input stream: {e}")))?;
    stream.play().map_err(|e| Error::Audio(format!("input play: {e}")))?;
    Ok((Capture { _stream: stream, sample_rate, device_name: name_of(&dev), overruns }, rx))
}
