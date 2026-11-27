//! Playback through cpal on the default output device.
//!
//! cpal streams aren't `Send` on every platform, so the stream lives on its
//! own thread for as long as the player does.

use super::Jitter;
use crate::wire::AudioFormat;
use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

pub(super) struct Player {
    format: AudioFormat,
    buf: Arc<Mutex<Jitter>>,
    /// Dropping this ends the playback thread.
    _stop: Sender<()>,
}

impl Player {
    pub(super) fn new(format: AudioFormat) -> Result<Self> {
        let buf = Arc::new(Mutex::new(Jitter::new(format.sample_rate, format.channels)));
        let (stop_tx, stop_rx) = channel::<()>();
        let (ready_tx, ready_rx) = channel::<Result<()>>();
        let b = buf.clone();
        std::thread::Builder::new()
            .name("da-audio-out".into())
            .spawn(move || {
                let stream = match open(b) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                // Keep playing until the player is dropped.
                let _ = stop_rx.recv();
                drop(stream);
            })?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("the sound output stopped while starting"))??;
        Ok(Player {
            format,
            buf,
            _stop: stop_tx,
        })
    }

    pub(super) fn push(&self, pcm: &[u8]) {
        self.buf.lock().unwrap().push(pcm);
    }

    pub(super) fn format(&self) -> AudioFormat {
        self.format
    }
}

fn open(buf: Arc<Mutex<Jitter>>) -> Result<cpal::Stream> {
    let device = cpal::default_host()
        .default_output_device()
        .context("this device has no sound output")?;
    let supported = device
        .default_output_config()
        .map_err(|e| anyhow!("could not read the sound output's format: {e}"))?;
    let config = supported.config();
    tracing::info!(
        rate = config.sample_rate,
        channels = config.channels,
        format = ?supported.sample_format(),
        "playing the other computer's sound"
    );
    let stream = match supported.sample_format() {
        SampleFormat::I16 => build::<i16>(&device, config, buf)?,
        SampleFormat::I32 => build::<i32>(&device, config, buf)?,
        SampleFormat::U16 => build::<u16>(&device, config, buf)?,
        // f32 is what CoreAudio and WASAPI normally use.
        _ => build::<f32>(&device, config, buf)?,
    };
    stream
        .play()
        .map_err(|e| anyhow!("could not start the sound output: {e}"))?;
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    buf: Arc<Mutex<Jitter>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream::<T, _, _>(
            config,
            move |out: &mut [T], _| {
                scratch.resize(out.len(), 0.0);
                match buf.lock() {
                    Ok(mut j) => j.fill(&mut scratch, channels, rate),
                    Err(_) => scratch.fill(0.0),
                }
                for (o, s) in out.iter_mut().zip(&scratch) {
                    *o = T::from_sample(*s);
                }
            },
            |e| tracing::warn!("sound output: {e}"),
            None,
        )
        .map_err(|e| anyhow!("could not open the sound output: {e}"))
}
