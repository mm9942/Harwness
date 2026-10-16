//! Format and dimensions of an image, read from its headers.
//!
//! No decoder: the harness never renders an image, it only has to know what
//! it is forwarding. Every read is bounds-checked, so any byte string,
//! however hostile or truncated, ends in an error and never in a panic.

use harw_protocol::ImageFormat;

use crate::error::MediaError;

/// What the headers say about an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inspected {
    /// The format, from the magic bytes (never from a file name).
    pub format: ImageFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

pub(crate) fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    let pair = bytes.get(at..at.checked_add(2)?)?;
    Some(u32::from(u16::from_be_bytes([
        *pair.first()?,
        *pair.get(1)?,
    ])))
}

pub(crate) fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let quad = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([
        *quad.first()?,
        *quad.get(1)?,
        *quad.get(2)?,
        *quad.get(3)?,
    ]))
}

pub(crate) fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    let pair = bytes.get(at..at.checked_add(2)?)?;
    Some(u32::from(u16::from_le_bytes([
        *pair.first()?,
        *pair.get(1)?,
    ])))
}

pub(crate) fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    let quad = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([
        *quad.first()?,
        *quad.get(1)?,
        *quad.get(2)?,
        *quad.get(3)?,
    ]))
}

/// The format named by the magic bytes.
///
/// # Errors
/// [`MediaError::UnknownFormat`] for anything else (including SVG, which can
/// carry script, and formats we do not forward).
pub fn sniff(bytes: &[u8]) -> Result<ImageFormat, MediaError> {
    if bytes.starts_with(PNG_MAGIC) {
        Ok(ImageFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Ok(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok(ImageFormat::Gif)
    } else if bytes.get(0..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Ok(ImageFormat::Webp)
    } else {
        Err(MediaError::UnknownFormat)
    }
}

/// Format and dimensions.
///
/// # Errors
/// [`MediaError::Empty`], [`MediaError::UnknownFormat`] or
/// [`MediaError::Malformed`] when the header does not hold the dimensions.
pub fn inspect(bytes: &[u8]) -> Result<Inspected, MediaError> {
    if bytes.is_empty() {
        return Err(MediaError::Empty);
    }
    let format = sniff(bytes)?;
    let (width, height) = match format {
        ImageFormat::Png => png_dimensions(bytes),
        ImageFormat::Jpeg => jpeg_dimensions(bytes),
        ImageFormat::Gif => gif_dimensions(bytes),
        ImageFormat::Webp => webp_dimensions(bytes),
    }
    .ok_or(MediaError::Malformed("no readable dimensions"))?;
    if width == 0 || height == 0 {
        return Err(MediaError::Malformed("zero dimension"));
    }
    Ok(Inspected {
        format,
        width,
        height,
    })
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // Signature, then the first chunk must be IHDR with 13 bytes of data.
    if be32(bytes, 8)? != 13 || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    Some((be32(bytes, 16)?, be32(bytes, 20)?))
}

fn gif_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    Some((le16(bytes, 6)?, le16(bytes, 8)?))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2usize; // after SOI
    loop {
        // Find the next marker: 0xFF, optionally repeated as fill, then a code.
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        while *bytes.get(at)? == 0xFF {
            at = at.checked_add(1)?;
        }
        let code = *bytes.get(at)?;
        at = at.checked_add(1)?;
        match code {
            // Markers without a length: TEM, RSTn, SOI. EOI or SOS before a
            // frame header means there is no frame.
            0x01 | 0xD0..=0xD8 => {}
            0xD9 | 0xDA | 0x00 => return None,
            // SOFn (not DHT 0xC4, JPG 0xC8, DAC 0xCC): precision, height, width.
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                let height = be16(bytes, at.checked_add(3)?)?;
                let width = be16(bytes, at.checked_add(5)?)?;
                return Some((width, height));
            }
            _ => {
                let length = usize::try_from(be16(bytes, at)?).ok()?;
                if length < 2 {
                    return None;
                }
                at = at.checked_add(length)?;
            }
        }
    }
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match bytes.get(12..16)? {
        // Lossy: 3-byte frame tag, start code 9D 01 2A, then 14-bit sizes.
        b"VP8 " => {
            if bytes.get(23..26)? != [0x9D, 0x01, 0x2A] {
                return None;
            }
            Some((le16(bytes, 26)? & 0x3FFF, le16(bytes, 28)? & 0x3FFF))
        }
        // Lossless: signature 0x2F, then 14 bits of (width - 1), 14 of (height - 1).
        b"VP8L" => {
            if *bytes.get(20)? != 0x2F {
                return None;
            }
            let bits = le32(bytes, 21)?;
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        // Extended: 24-bit (width - 1) and (height - 1) after the flags.
        b"VP8X" => {
            let width = u32::from(*bytes.get(24)?)
                | (u32::from(*bytes.get(25)?) << 8)
                | (u32::from(*bytes.get(26)?) << 16);
            let height = u32::from(*bytes.get(27)?)
                | (u32::from(*bytes.get(28)?) << 8)
                | (u32::from(*bytes.get(29)?) << 16);
            Some((width + 1, height + 1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{TestError, TestResult, gif, jpeg, png, webp_extended, webp_lossy};

    fn dims(bytes: &[u8]) -> TestResult<(ImageFormat, u32, u32)> {
        let seen = inspect(bytes).map_err(|e| TestError::Unexpected(e.to_string()))?;
        Ok((seen.format, seen.width, seen.height))
    }

    #[test]
    fn every_format_reports_its_dimensions() -> TestResult {
        assert_eq!(
            dims(&png(640, 480, &[], &[]))?,
            (ImageFormat::Png, 640, 480)
        );
        assert_eq!(dims(&jpeg(1024, 768, &[]))?, (ImageFormat::Jpeg, 1024, 768));
        assert_eq!(dims(&gif(300, 200, &[]))?, (ImageFormat::Gif, 300, 200));
        assert_eq!(
            dims(&webp_lossy(500, 400, &[]))?,
            (ImageFormat::Webp, 500, 400)
        );
        assert_eq!(
            dims(&webp_extended(1920, 1080))?,
            (ImageFormat::Webp, 1920, 1080)
        );
        Ok(())
    }

    #[test]
    fn what_is_not_an_accepted_image_is_refused() {
        for bytes in [
            &b""[..],
            b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>",
            b"BM\x00\x00\x00\x00", // bmp
            b"%PDF-1.7",
            b"plain text",
            b"\x00\x00\x00\x00",
        ] {
            assert!(inspect(bytes).is_err(), "{bytes:?}");
        }
    }

    #[test]
    fn a_file_name_cannot_vouch_for_the_content() {
        // The format comes from the magic bytes only; there is no file name.
        assert_eq!(sniff(&png(1, 1, &[], &[])).ok(), Some(ImageFormat::Png));
        assert!(sniff(b"GIF8").is_err());
    }

    #[test]
    fn zero_dimensions_are_malformed() {
        assert!(inspect(&png(0, 10, &[], &[])).is_err());
        assert!(inspect(&jpeg(10, 0, &[])).is_err());
    }

    #[test]
    fn no_prefix_of_any_image_panics() {
        let samples = [
            png(64, 64, &[], &[]),
            jpeg(64, 64, &[]),
            gif(64, 64, &[]),
            webp_lossy(64, 64, &[]),
            webp_extended(64, 64),
        ];
        for sample in &samples {
            for len in 0..sample.len() {
                let _ = inspect(sample.get(..len).unwrap_or(&[]));
            }
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // Deterministic xorshift, seeded with each magic so the parsers go deep.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for magic in [
            &b"\x89PNG\r\n\x1a\n"[..],
            b"\xFF\xD8\xFF",
            b"GIF89a",
            b"RIFF\x20\x00\x00\x00WEBP",
        ] {
            for _ in 0..2000 {
                let mut bytes = magic.to_vec();
                let extra = usize::try_from(next() % 80).unwrap_or(0);
                for _ in 0..extra {
                    bytes.push(next().to_le_bytes()[0]);
                }
                let _ = inspect(&bytes);
                if let Ok(format) = sniff(&bytes) {
                    let _ = crate::sanitize::sanitize(&bytes, format);
                }
            }
        }
    }
}
