//! Windows: record what the default output device plays (WASAPI loopback,
//! which cpal uses when asked for an input stream on an output device).
//! Needs no permission. Windows delivers nothing while nothing plays.

use super::{AudioEvent, AudioSink, Chunker};
use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use std::sync::mpsc::{channel, Sender};

pub(super) struct Capture {
    /// Dropping this ends the capture thread (and the stream with it).
    _stop: Sender<()>,
}

impl Capture {
    pub(super) fn start(sink: AudioSink) -> Result<Self> {
        let (stop_tx, stop_rx) = channel::<()>();
        let (ready_tx, ready_rx) = channel::<Result<()>>();
        std::thread::Builder::new()
            .name("da-audio-in".into())
            .spawn(move || match open(sink) {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stop_rx.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("sound capture stopped while starting"))??;
        Ok(Capture { _stop: stop_tx })
    }
}

fn open(mut sink: AudioSink) -> Result<cpal::Stream> {
    let device = cpal::default_host()
        .default_output_device()
        .context("this PC has no sound output to share")?;
    let supported = device
        .default_output_config()
        .map_err(|e| anyhow!("could not read the sound output's format: {e}"))?;
    let config = supported.config();
    let format = supported.sample_format();
    let channels = usize::from(config.channels);
    tracing::info!(
        rate = config.sample_rate,
        channels,
        ?format,
        "sharing this PC's sound (loopback)"
    );
    let mut chunker = Chunker::new(config.sample_rate);
    sink(AudioEvent::Format(chunker.format()));
    let stream = device
        .build_input_stream_raw(
            config,
            format,
            move |data: &cpal::Data, _| {
                let emit = |c| sink(AudioEvent::Chunk(c));
                match data.sample_format() {
                    SampleFormat::F32 => {
                        if let Some(s) = data.as_slice::<f32>() {
                            chunker.push_interleaved(s.iter().copied(), channels, emit);
                        }
                    }
                    SampleFormat::I16 => {
                        if let Some(s) = data.as_slice::<i16>() {
                            let it = s.iter().map(|v| f32::from(*v) / 32768.0);
                            chunker.push_interleaved(it, channels, emit);
                        }
                    }
                    SampleFormat::I32 => {
                        if let Some(s) = data.as_slice::<i32>() {
                            let it = s.iter().map(|v| *v as f32 / 2_147_483_648.0);
                            chunker.push_interleaved(it, channels, emit);
                        }
                    }
                    _ => {}
                }
            },
            |e| tracing::warn!("sound capture: {e}"),
            None,
        )
        .map_err(|e| anyhow!("could not record this PC's sound: {e}"))?;
    stream
        .play()
        .map_err(|e| anyhow!("could not start recording this PC's sound: {e}"))?;
    Ok(stream)
}
