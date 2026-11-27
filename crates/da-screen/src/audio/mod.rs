//! The host's sound, sent to the viewer alongside the picture.
//!
//! - Host: [`AudioCapture`] records what this computer is playing
//!   (Windows: WASAPI loopback on the default output device, through cpal;
//!   macOS: ScreenCaptureKit's system audio, which needs the same Screen
//!   Recording permission as the picture).
//! - Viewer: [`AudioPlayer`] plays it on the default output device
//!   (through cpal on Windows, macOS and iOS), with a small jitter buffer
//!   that also absorbs clock drift between the two machines.
//!
//! On the wire the sound is plain 16-bit PCM in 10 ms chunks (see
//! [`crate::wire::AudioFormat`]): about 1.5 Mbit/s for 48 kHz stereo, which
//! is small next to the video on a LAN and needs no codec.

use crate::wire::{AudioChunk, AudioFormat};
use anyhow::Result;

mod jitter;
pub use jitter::Jitter;

#[cfg(any(windows, target_os = "macos", target_os = "ios"))]
mod player;

#[cfg(target_os = "macos")]
mod capture_macos;
#[cfg(target_os = "macos")]
use capture_macos as capture_platform;
#[cfg(windows)]
mod capture_windows;
#[cfg(windows)]
use capture_windows as capture_platform;

/// What the capture reports.
#[derive(Debug)]
pub enum AudioEvent {
    /// The format of the chunks that follow.
    Format(AudioFormat),
    Chunk(AudioChunk),
    /// Capture stopped; the text is for the person.
    Error(String),
}

pub type AudioSink = Box<dyn FnMut(AudioEvent) + Send>;

/// True when this build can record the computer's sound.
pub fn capture_supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

/// True when this build can play a host's sound.
pub fn playback_supported() -> bool {
    cfg!(any(windows, target_os = "macos", target_os = "ios"))
}

/// Records what this computer plays. Dropping it stops recording.
pub struct AudioCapture {
    #[cfg(any(windows, target_os = "macos"))]
    _inner: capture_platform::Capture,
}

impl AudioCapture {
    /// Start recording. `sink` gets the format, then a chunk every 10 ms or
    /// so while anything plays (Windows sends nothing during silence).
    pub fn start(sink: AudioSink) -> Result<Self> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            Ok(AudioCapture {
                _inner: capture_platform::Capture::start(sink)?,
            })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = sink;
            anyhow::bail!("this computer can't share its sound yet")
        }
    }
}

/// Plays a host's sound on this device's default output.
pub struct AudioPlayer {
    #[cfg(any(windows, target_os = "macos", target_os = "ios"))]
    inner: player::Player,
}

impl AudioPlayer {
    /// Open the default output device for sound in `format`.
    pub fn new(format: AudioFormat) -> Result<Self> {
        #[cfg(any(windows, target_os = "macos", target_os = "ios"))]
        {
            Ok(AudioPlayer {
                inner: player::Player::new(format)?,
            })
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
        {
            let _ = format;
            anyhow::bail!("this device can't play sound yet")
        }
    }

    /// Queue a chunk (16-bit PCM in the format given to [`AudioPlayer::new`]).
    pub fn push(&self, pcm: &[u8]) {
        #[cfg(any(windows, target_os = "macos", target_os = "ios"))]
        self.inner.push(pcm);
        #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
        let _ = pcm;
    }

    /// The format this player was opened for.
    pub fn format(&self) -> AudioFormat {
        #[cfg(any(windows, target_os = "macos", target_os = "ios"))]
        {
            self.inner.format()
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
        unreachable!("no player on this platform")
    }
}

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
/// Turns captured samples (any channel count, as f32) into 10 ms chunks of
/// 16-bit stereo for the wire.
pub(crate) struct Chunker {
    format: AudioFormat,
    frames_per_chunk: usize,
    buf: Vec<u8>,
    /// Frames sent so far, for timestamps.
    sent: u64,
}

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
impl Chunker {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let format = AudioFormat {
            sample_rate,
            channels: 2,
        };
        let frames_per_chunk = (sample_rate as usize / 100).max(1);
        Chunker {
            format,
            frames_per_chunk,
            buf: Vec::with_capacity(frames_per_chunk * 4),
            sent: 0,
        }
    }

    pub(crate) fn format(&self) -> AudioFormat {
        self.format
    }

    /// Add one frame (left, right) and hand back any full chunk.
    pub(crate) fn push(&mut self, l: f32, r: f32, mut emit: impl FnMut(AudioChunk)) {
        self.buf.extend_from_slice(&to_i16(l).to_le_bytes());
        self.buf.extend_from_slice(&to_i16(r).to_le_bytes());
        if self.buf.len() >= self.frames_per_chunk * 4 {
            let pts_us = self.sent * 1_000_000 / u64::from(self.format.sample_rate.max(1));
            self.sent += self.frames_per_chunk as u64;
            let data =
                std::mem::replace(&mut self.buf, Vec::with_capacity(self.frames_per_chunk * 4));
            emit(AudioChunk { pts_us, data });
        }
    }

    /// Add interleaved frames of `channels` channels. Mono is copied to
    /// both sides; more than two channels keep front left and right.
    pub(crate) fn push_interleaved(
        &mut self,
        samples: impl Iterator<Item = f32>,
        channels: usize,
        mut emit: impl FnMut(AudioChunk),
    ) {
        let channels = channels.max(1);
        let mut frame = [0f32; 2];
        for (i, s) in samples.enumerate() {
            let c = i % channels;
            if c < 2 {
                frame[c] = s;
            }
            if c == channels - 1 {
                let r = if channels == 1 { frame[0] } else { frame[1] };
                self.push(frame[0], r, &mut emit);
            }
        }
    }
}

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_ten_ms_of_stereo() {
        let mut c = Chunker::new(48_000);
        let mut out = Vec::new();
        // 25 ms of 6-channel sound: two whole chunks, the rest waits.
        let samples = (0..1200 * 6).map(|i| if i % 6 == 1 { -1.0 } else { 0.5 });
        c.push_interleaved(samples, 6, |ch| out.push(ch));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].data.len(), 480 * 4);
        assert_eq!((out[0].pts_us, out[1].pts_us), (0, 10_000));
        let l = i16::from_le_bytes([out[0].data[0], out[0].data[1]]);
        let r = i16::from_le_bytes([out[0].data[2], out[0].data[3]]);
        assert_eq!((l, r), (16384, -32767));
    }

    #[test]
    fn mono_goes_to_both_sides() {
        let mut c = Chunker::new(100);
        let mut out = Vec::new();
        c.push_interleaved([1.0f32].into_iter(), 1, |ch| out.push(ch));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, [0xff, 0x7f, 0xff, 0x7f]);
    }
}
