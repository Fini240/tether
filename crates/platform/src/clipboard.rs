//! The system clipboard, via `arboard`.
//!
//! Images cross the wire as PNG rather than as raw RGBA: a 4K screenshot is
//! ~33 MB uncompressed and about 2 MB as PNG, and the clipboard path shares a
//! connection with keystrokes. Encoding costs a few milliseconds on the copy,
//! which nobody notices; a 33 MB write would stall the pointer, which everybody
//! does.

use tether_proto::{ClipboardContents, ClipboardStamp};

use crate::traits::{ClipboardAccess, PlatformError, Result};

pub struct SystemClipboard {
    inner: arboard::Clipboard,
    /// Bumped on every successful local read that differs from the last one.
    seq: u64,
    owner: u64,
    last: Option<ClipboardContents>,
}

impl SystemClipboard {
    pub fn new(owner: u64) -> Result<Self> {
        let inner = arboard::Clipboard::new().map_err(map_err)?;
        Ok(Self {
            inner,
            seq: 0,
            owner,
            last: None,
        })
    }

    /// Read the clipboard and report whether it changed since the last read.
    ///
    /// This is polled — no platform offers a portable change notification, and
    /// the two that offer *something* (NSPasteboard's changeCount, Windows'
    /// clipboard-format listener) do not agree on semantics. Comparing contents
    /// keeps the caller honest and costs little at a ~500 ms poll.
    pub fn poll(&mut self) -> Result<Option<ClipboardContents>> {
        let mut current = self.read_raw()?;
        let changed = match &self.last {
            Some(prev) => !same_contents(prev, &current),
            None => !current.is_empty(),
        };
        if !changed {
            return Ok(None);
        }
        self.seq += 1;
        current.stamp = ClipboardStamp {
            owner: self.owner,
            seq: self.seq,
        };
        self.last = Some(current.clone());
        Ok(Some(current))
    }

    fn read_raw(&mut self) -> Result<ClipboardContents> {
        let mut contents = ClipboardContents::empty(ClipboardStamp {
            owner: self.owner,
            seq: self.seq,
        });

        match self.inner.get_text() {
            Ok(text) if !text.is_empty() => contents.text = Some(text),
            Ok(_) => {}
            // An absent flavour is normal, not a failure.
            Err(arboard::Error::ContentNotAvailable) => {}
            Err(e) => return Err(map_err(e)),
        }

        match self.inner.get_image() {
            Ok(image) => {
                let png = encode_png(image.width, image.height, &image.bytes)?;
                contents.png = Some(png);
            }
            Err(arboard::Error::ContentNotAvailable) => {}
            Err(e) => tracing::debug!(%e, "clipboard image read failed; continuing without it"),
        }

        // TODO(rich-text): HTML needs `arboard`'s Get::html on the platforms
        // that expose it, plus an NSPasteboard `public.html` read on macOS.
        // Plain text is preserved meanwhile, so a rich-text copy degrades to
        // text rather than to nothing.
        Ok(contents)
    }
}

impl ClipboardAccess for SystemClipboard {
    fn read(&mut self) -> Result<ClipboardContents> {
        self.read_raw()
    }

    fn write(&mut self, contents: &ClipboardContents) -> Result<()> {
        if let Some(png) = &contents.png {
            let (width, height, rgba) = decode_png(png)?;
            self.inner
                .set_image(arboard::ImageData {
                    width,
                    height,
                    bytes: rgba.into(),
                })
                .map_err(map_err)?;
        }
        if let Some(text) = &contents.text {
            self.inner.set_text(text.clone()).map_err(map_err)?;
        }
        // Remember what we just wrote so the next poll does not see our own
        // paste as a local change and echo it back to the machine it came from.
        self.last = Some(contents.clone());
        Ok(())
    }

    fn poll_change(&mut self) -> Result<Option<ClipboardContents>> {
        self.poll()
    }
}

/// Compare the payloads, ignoring the stamp — two machines will never agree on
/// stamps, and a stamp difference is not a content change.
fn same_contents(a: &ClipboardContents, b: &ClipboardContents) -> bool {
    a.text == b.text && a.html == b.html && a.png == b.png && a.files == b.files
}

fn map_err(e: arboard::Error) -> PlatformError {
    match e {
        arboard::Error::ClipboardOccupied => {
            PlatformError::backend("clipboard is locked by another application")
        }
        other => PlatformError::backend(other.to_string()),
    }
}

pub fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>> {
    let expected = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| PlatformError::backend("clipboard image dimensions overflow"))?;
    if rgba.len() < expected {
        return Err(PlatformError::backend(format!(
            "clipboard image is {} bytes, expected {expected}",
            rgba.len()
        )));
    }

    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| PlatformError::backend(format!("png header: {e}")))?;
    writer
        .write_image_data(&rgba[..expected])
        .map_err(|e| PlatformError::backend(format!("png encode: {e}")))?;
    writer
        .finish()
        .map_err(|e| PlatformError::backend(format!("png finish: {e}")))?;
    Ok(out)
}

/// Decode to RGBA8. Returns `(width, height, rgba)`.
pub fn decode_png(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    let mut decoder = png::Decoder::new(bytes);
    // EXPAND turns palette and low-bit-depth greyscale into 8-bit channels;
    // STRIP_16 drops 16-bit images to 8. Between them every input becomes
    // Grayscale/Rgb/Rgba at 8 bits, which the match below covers.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);

    let mut reader = decoder
        .read_info()
        .map_err(|e| PlatformError::backend(format!("png header: {e}")))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| PlatformError::backend(format!("png decode: {e}")))?;

    let (w, h) = (info.width as usize, info.height as usize);
    let pixels = w * h;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..pixels * 4].to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(pixels * 4);
            for px in buf[..pixels * 3].chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(pixels * 4);
            for &g in &buf[..pixels] {
                out.extend_from_slice(&[g, g, g, 0xFF]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(pixels * 4);
            for px in buf[..pixels * 2].chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Indexed => {
            // EXPAND should have removed this; if it did not, refuse rather
            // than paste garbage.
            return Err(PlatformError::backend("unexpected indexed png"));
        }
    };

    Ok((w, h, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trips_rgba() {
        let rgba: Vec<u8> = (0..(4 * 3 * 4)).map(|i| i as u8).collect();
        let png = encode_png(4, 3, &rgba).unwrap();
        let (w, h, back) = decode_png(&png).unwrap();
        assert_eq!((w, h), (4, 3));
        assert_eq!(back, rgba);
    }

    #[test]
    fn encoding_rejects_a_short_buffer() {
        assert!(encode_png(4, 4, &[0u8; 8]).is_err());
    }

    #[test]
    fn decoding_rejects_garbage() {
        assert!(decode_png(b"not a png").is_err());
    }
}
