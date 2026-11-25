//! Clipboard access for text and images. Images cross the wire as PNG.

use crate::wire::{Frame, MAX_CLIPBOARD_IMAGE};
use anyhow::{anyhow, bail, Result};
use std::borrow::Cow;

/// What the clipboard holds, in a form that can cross the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clip {
    Text(String),
    /// A PNG-encoded image.
    Png(Vec<u8>),
}

impl Clip {
    pub fn is_empty(&self) -> bool {
        match self {
            Clip::Text(t) => t.is_empty(),
            Clip::Png(b) => b.is_empty(),
        }
    }

    pub fn into_frame(self) -> Frame {
        match self {
            Clip::Text(t) => Frame::ClipboardText(t),
            Clip::Png(b) => Frame::ClipboardImage(b),
        }
    }
}

/// This machine's clipboard to send: its text if it has any, otherwise its
/// image. None if there is nothing we can send.
pub fn get() -> Option<Clip> {
    let mut c = arboard::Clipboard::new().ok()?;
    if let Ok(t) = c.get_text() {
        if !t.is_empty() {
            return Some(Clip::Text(t));
        }
    }
    let img = c.get_image().ok()?;
    match encode_png(img.width, img.height, &img.bytes) {
        Ok(png) if png.len() <= MAX_CLIPBOARD_IMAGE => Some(Clip::Png(png)),
        Ok(png) => {
            tracing::info!(bytes = png.len(), "clipboard image is too large to send");
            None
        }
        Err(e) => {
            tracing::debug!("could not encode the clipboard image: {e:#}");
            None
        }
    }
}

/// Put something received from the other computer on this clipboard.
pub fn set(clip: &Clip) {
    let mut c = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("clipboard unavailable: {e}");
            return;
        }
    };
    let result = match clip {
        Clip::Text(t) => c.set_text(t.clone()).map_err(anyhow::Error::from),
        Clip::Png(b) => decode_png(b).and_then(|img| c.set_image(img).map_err(anyhow::Error::from)),
    };
    if let Err(e) = result {
        tracing::debug!("could not set the clipboard: {e:#}");
    }
}

fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width as u32, height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        // Favour speed: this runs while the user is waiting for a switch.
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

fn decode_png(bytes: &[u8]) -> Result<arboard::ImageData<'static>> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info()?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| anyhow!("image is too large"))?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf)?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        bail!("unexpected image format");
    }
    buf.truncate(info.buffer_size());
    Ok(arboard::ImageData {
        width: info.width as usize,
        height: info.height as usize,
        bytes: Cow::Owned(buf),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trips_pixels() {
        let rgba: Vec<u8> = (0..2 * 3 * 4).map(|i| i as u8 * 7).collect();
        let png = encode_png(2, 3, &rgba).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let img = decode_png(&png).unwrap();
        assert_eq!((img.width, img.height), (2, 3));
        assert_eq!(img.bytes.as_ref(), rgba.as_slice());
    }

    #[test]
    fn a_non_png_is_rejected() {
        assert!(decode_png(b"not an image").is_err());
    }
}
