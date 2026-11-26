//! The viewer's sound buffer: holds a little of the host's sound so short
//! network hiccups don't click, converts its sample rate to the output
//! device's, and keeps the delay bounded when the two clocks drift apart.

use std::collections::VecDeque;

/// Sound to hold before (re)starting playback.
const TARGET_MS: u32 = 60;
/// More than this waiting means we fell behind: skip ahead to the target.
const MAX_MS: u32 = 200;

pub struct Jitter {
    rate: u32,
    channels: usize,
    /// Stereo frames at the host's rate, as -1..=1.
    frames: VecDeque<[f32; 2]>,
    /// Position between `frames[0]` and `frames[1]` for resampling.
    frac: f64,
    /// Playing (false while filling up to the target).
    running: bool,
}

impl Jitter {
    /// For sound at `rate` Hz with `channels` interleaved 16-bit channels.
    pub fn new(rate: u32, channels: u16) -> Self {
        Jitter {
            rate: rate.max(1),
            channels: usize::from(channels.max(1)),
            frames: VecDeque::with_capacity(rate as usize / 2),
            frac: 0.0,
            running: false,
        }
    }

    fn ms(&self, ms: u32) -> usize {
        (self.rate as usize * ms as usize) / 1000
    }

    /// How much sound is waiting, in frames at the host's rate.
    pub fn buffered(&self) -> usize {
        self.frames.len()
    }

    /// Add a chunk of 16-bit little-endian interleaved PCM.
    pub fn push(&mut self, pcm: &[u8]) {
        let frame_bytes = self.channels * 2;
        for f in pcm.chunks_exact(frame_bytes) {
            let s = |i: usize| f32::from(i16::from_le_bytes([f[i * 2], f[i * 2 + 1]])) / 32768.0;
            let l = s(0);
            let r = if self.channels > 1 { s(1) } else { l };
            self.frames.push_back([l, r]);
        }
        let max = self.ms(MAX_MS);
        if self.frames.len() > max {
            let drop = self.frames.len() - self.ms(TARGET_MS);
            self.frames.drain(..drop);
        }
    }

    /// Drop everything waiting (e.g. when muted).
    pub fn clear(&mut self) {
        self.frames.clear();
        self.frac = 0.0;
        self.running = false;
    }

    /// Fill `out` (interleaved, `out_channels` channels at `out_rate` Hz).
    /// Plays silence while filling up after a gap.
    pub fn fill(&mut self, out: &mut [f32], out_channels: usize, out_rate: u32) {
        let out_channels = out_channels.max(1);
        if !self.running && self.frames.len() >= self.ms(TARGET_MS) {
            self.running = true;
        }
        let step = f64::from(self.rate) / f64::from(out_rate.max(1));
        for frame in out.chunks_mut(out_channels) {
            let (l, r) = if self.running && self.frames.len() >= 2 {
                let a = self.frames[0];
                let b = self.frames[1];
                let t = self.frac as f32;
                let v = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
                self.frac += step;
                while self.frac >= 1.0 && !self.frames.is_empty() {
                    self.frames.pop_front();
                    self.frac -= 1.0;
                }
                v
            } else {
                // Ran dry: wait for the target again rather than crackle.
                self.running = false;
                (0.0, 0.0)
            };
            match frame.len() {
                1 => frame[0] = (l + r) * 0.5,
                _ => {
                    frame[0] = l;
                    frame[1] = r;
                    for s in &mut frame[2..] {
                        *s = 0.0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(frames: usize, l: i16, r: i16) -> Vec<u8> {
        let mut v = Vec::new();
        for _ in 0..frames {
            v.extend_from_slice(&l.to_le_bytes());
            v.extend_from_slice(&r.to_le_bytes());
        }
        v
    }

    #[test]
    fn waits_for_the_target_then_plays() {
        let mut j = Jitter::new(48_000, 2);
        let mut out = vec![1.0f32; 2 * 480];
        j.push(&pcm(480, 16384, -16384)); // 10 ms: not enough yet
        j.fill(&mut out, 2, 48_000);
        assert!(out.iter().all(|s| *s == 0.0));
        j.push(&pcm(48 * 60, 16384, -16384));
        j.fill(&mut out, 2, 48_000);
        assert_eq!((out[0], out[1]), (0.5, -0.5));
        // Same rate: one frame out per frame in.
        assert_eq!(j.buffered(), 480 + 48 * 60 - 480);
    }

    #[test]
    fn converts_rate_and_channels() {
        let mut j = Jitter::new(48_000, 2);
        j.push(&pcm(48 * 100, 16384, 0));
        // 44.1 kHz mono device: 441 frames use about 480 input frames.
        let before = j.buffered();
        let mut out = vec![0f32; 441];
        j.fill(&mut out, 1, 44_100);
        assert_eq!(out[0], 0.25);
        let used = before - j.buffered();
        assert!((479..=481).contains(&used), "used {used}");
        // 4-channel device: extra channels stay silent.
        let mut out = vec![1f32; 8];
        j.fill(&mut out, 4, 48_000);
        assert_eq!(&out[..4], &[0.5, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn skips_ahead_when_too_far_behind() {
        let mut j = Jitter::new(1000, 1);
        let mono: Vec<u8> = (0..500i16).flat_map(|v| v.to_le_bytes()).collect();
        j.push(&mono);
        assert_eq!(j.buffered(), 60);
    }

    #[test]
    fn refills_after_running_dry() {
        let mut j = Jitter::new(1000, 2);
        j.push(&pcm(60, 1000, 1000));
        let mut out = vec![0f32; 2 * 100];
        j.fill(&mut out, 2, 1000);
        assert!(out[..2 * 50].iter().all(|s| *s != 0.0));
        assert!(out[2 * 60..].iter().all(|s| *s == 0.0));
        // A little more isn't enough to restart.
        j.push(&pcm(10, 1000, 1000));
        let mut out = vec![1f32; 2];
        j.fill(&mut out, 2, 1000);
        assert_eq!(out, [0.0, 0.0]);
    }
}
