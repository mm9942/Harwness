//! Re-emitting an image without what a model does not need and a user did not
//! mean to share.
//!
//! Phone photos carry GPS position, device and time in EXIF/XMP; files can
//! carry comments, text chunks and whole payloads appended after the image.
//! The sanitizer parses the container structure, keeps only the parts that
//! affect rendering, and writes a fresh byte string. Pixel data is copied,
//! never decoded. Anything that does not parse as its format is an error,
//! not a pass-through.

use harw_protocol::ImageFormat;

use crate::error::MediaError;
use crate::inspect::{be16, be32, le32};

/// A sanitized copy of `bytes`, which must be `format` (see
/// [`crate::inspect::sniff`]).
///
/// # Errors
/// [`MediaError::Malformed`] when the container does not parse.
pub fn sanitize(bytes: &[u8], format: ImageFormat) -> Result<Vec<u8>, MediaError> {
    match format {
        ImageFormat::Png => png(bytes),
        ImageFormat::Jpeg => jpeg(bytes),
        ImageFormat::Gif => gif(bytes),
        ImageFormat::Webp => webp(bytes),
    }
}

fn bad(why: &'static str) -> MediaError {
    MediaError::Malformed(why)
}

// --- PNG ---------------------------------------------------------------

/// Chunks that affect rendering. Everything else ancillary (text, time,
/// EXIF, unknown) is dropped; an unknown *critical* chunk is an error.
const PNG_KEEP: &[&[u8; 4]] = &[
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT",
    b"bKGD", b"hIST", b"pHYs", b"sPLT", b"acTL", b"fcTL", b"fdAT", b"cICP", b"mDCV", b"cLLI",
];

fn png(bytes: &[u8]) -> Result<Vec<u8>, MediaError> {
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(bytes.get(..8).ok_or_else(|| bad("short png"))?);
    let mut at = 8usize;
    let mut first = true;
    let mut saw_data = false;
    loop {
        let length = usize::try_from(be32(bytes, at).ok_or_else(|| bad("png chunk header"))?)
            .map_err(|_| bad("png chunk length"))?;
        let kind: [u8; 4] = bytes
            .get(at + 4..at + 8)
            .and_then(|k| <[u8; 4]>::try_from(k).ok())
            .ok_or_else(|| bad("png chunk type"))?;
        let end = at
            .checked_add(12)
            .and_then(|n| n.checked_add(length))
            .ok_or_else(|| bad("png chunk length"))?;
        let chunk = bytes
            .get(at..end)
            .ok_or_else(|| bad("truncated png chunk"))?;
        if first && &kind != b"IHDR" {
            return Err(bad("png does not start with IHDR"));
        }
        first = false;
        let ancillary = kind.first().is_some_and(|b| b & 0x20 != 0);
        if PNG_KEEP.iter().any(|k| **k == kind) {
            if &kind == b"IDAT" {
                saw_data = true;
            }
            out.extend_from_slice(chunk);
            if &kind == b"IEND" {
                break; // anything after IEND is dropped
            }
        } else if !ancillary {
            return Err(bad("unknown critical png chunk"));
        }
        at = end;
    }
    if !saw_data {
        return Err(bad("png without image data"));
    }
    Ok(out)
}

// --- JPEG --------------------------------------------------------------

fn jpeg(bytes: &[u8]) -> Result<Vec<u8>, MediaError> {
    let mut out = vec![0xFF, 0xD8];
    let mut at = 2usize;
    let mut saw_frame = false;
    loop {
        if bytes.get(at) != Some(&0xFF) {
            return Err(bad("jpeg marker expected"));
        }
        while bytes.get(at) == Some(&0xFF) {
            at += 1;
        }
        let code = *bytes.get(at).ok_or_else(|| bad("truncated jpeg"))?;
        at += 1;
        match code {
            0xD9 => {
                if !saw_frame {
                    return Err(bad("jpeg without frame"));
                }
                out.extend_from_slice(&[0xFF, 0xD9]);
                return Ok(out); // anything after EOI is dropped
            }
            0x00 => return Err(bad("jpeg stray stuffing")),
            0x01 | 0xD0..=0xD8 => {} // no payload, not expected here, harmless
            _ => {
                let length =
                    usize::try_from(be16(bytes, at).ok_or_else(|| bad("jpeg segment length"))?)
                        .map_err(|_| bad("jpeg segment length"))?;
                if length < 2 {
                    return Err(bad("jpeg segment length"));
                }
                let end = at.checked_add(length).ok_or_else(|| bad("jpeg segment"))?;
                let segment = bytes
                    .get(at..end)
                    .ok_or_else(|| bad("truncated jpeg segment"))?;
                at = end;
                let keep = match code {
                    // JFIF, ICC profile, Adobe colour transform, tables, frame
                    // headers, restart interval, scan header.
                    0xE0 | 0xE2 | 0xEE | 0xDB | 0xC4 | 0xDD | 0xDC | 0xDA => true,
                    0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                        saw_frame = true;
                        true
                    }
                    // EXIF/XMP (E1), other APPn, comments (FE), reserved.
                    _ => false,
                };
                if keep {
                    out.extend_from_slice(&[0xFF, code]);
                    out.extend_from_slice(segment);
                }
                if code == 0xDA {
                    // Entropy-coded data runs to the next real marker: 0xFF
                    // followed by anything but 0x00 (stuffing) or RSTn.
                    let start = at;
                    loop {
                        let byte = *bytes.get(at).ok_or_else(|| bad("truncated jpeg scan"))?;
                        if byte == 0xFF {
                            match bytes.get(at + 1) {
                                Some(0x00 | 0xD0..=0xD7) => at += 2,
                                Some(_) => break,
                                None => return Err(bad("truncated jpeg scan")),
                            }
                        } else {
                            at += 1;
                        }
                    }
                    out.extend_from_slice(bytes.get(start..at).ok_or_else(|| bad("jpeg scan"))?);
                }
            }
        }
    }
}

// --- GIF ---------------------------------------------------------------

fn gif_sub_blocks(bytes: &[u8], mut at: usize) -> Result<usize, MediaError> {
    loop {
        let size = usize::from(*bytes.get(at).ok_or_else(|| bad("truncated gif block"))?);
        at += 1;
        if size == 0 {
            return Ok(at);
        }
        at = at.checked_add(size).ok_or_else(|| bad("gif block"))?;
        if at > bytes.len() {
            return Err(bad("truncated gif block"));
        }
    }
}

fn gif_color_table(flags: u8) -> usize {
    if flags & 0x80 != 0 {
        3usize << (usize::from(flags & 0x07) + 1)
    } else {
        0
    }
}

fn gif(bytes: &[u8]) -> Result<Vec<u8>, MediaError> {
    let flags = *bytes.get(10).ok_or_else(|| bad("short gif"))?;
    let mut at = 13usize
        .checked_add(gif_color_table(flags))
        .ok_or_else(|| bad("gif"))?;
    let mut out = bytes.get(..at).ok_or_else(|| bad("short gif"))?.to_vec();
    let mut saw_image = false;
    loop {
        match *bytes.get(at).ok_or_else(|| bad("gif without trailer"))? {
            0x3B => {
                if !saw_image {
                    return Err(bad("gif without image"));
                }
                out.push(0x3B);
                return Ok(out); // anything after the trailer is dropped
            }
            0x21 => {
                let label = *bytes.get(at + 1).ok_or_else(|| bad("gif extension"))?;
                let end = if label == 0xFF {
                    // Application extension: keep only the animation ones.
                    let id = bytes
                        .get(at + 3..at + 14)
                        .ok_or_else(|| bad("gif extension"))?;
                    let end = gif_sub_blocks(bytes, at + 2)?;
                    if id == b"NETSCAPE2.0" || id == b"ANIMEXTS1.0" {
                        out.extend_from_slice(
                            bytes.get(at..end).ok_or_else(|| bad("gif extension"))?,
                        );
                    }
                    end
                } else {
                    let end = gif_sub_blocks(bytes, at + 2)?;
                    // Graphic control (0xF9) affects rendering; comment (0xFE),
                    // plain text (0x01) and unknown extensions are dropped.
                    if label == 0xF9 {
                        out.extend_from_slice(
                            bytes.get(at..end).ok_or_else(|| bad("gif extension"))?,
                        );
                    }
                    end
                };
                at = end;
            }
            0x2C => {
                let flags = *bytes.get(at + 9).ok_or_else(|| bad("gif image"))?;
                let table = gif_color_table(flags);
                // Descriptor (10), local table, LZW code size (1), data blocks.
                let data = at
                    .checked_add(10)
                    .and_then(|n| n.checked_add(table))
                    .and_then(|n| n.checked_add(1))
                    .ok_or_else(|| bad("gif image"))?;
                let end = gif_sub_blocks(bytes, data)?;
                out.extend_from_slice(bytes.get(at..end).ok_or_else(|| bad("gif image"))?);
                saw_image = true;
                at = end;
            }
            _ => return Err(bad("unexpected gif block")),
        }
    }
}

// --- WebP --------------------------------------------------------------

fn webp(bytes: &[u8]) -> Result<Vec<u8>, MediaError> {
    let declared = usize::try_from(le32(bytes, 4).ok_or_else(|| bad("short webp"))?)
        .map_err(|_| bad("webp size"))?;
    let end = declared.checked_add(8).ok_or_else(|| bad("webp size"))?;
    let body = bytes.get(12..end).ok_or_else(|| bad("truncated webp"))?;
    let mut chunks = Vec::with_capacity(body.len());
    let mut at = 0usize;
    let mut saw_image = false;
    while at < body.len() {
        let kind: [u8; 4] = body
            .get(at..at + 4)
            .and_then(|k| <[u8; 4]>::try_from(k).ok())
            .ok_or_else(|| bad("webp chunk header"))?;
        let size = usize::try_from(le32(body, at + 4).ok_or_else(|| bad("webp chunk header"))?)
            .map_err(|_| bad("webp chunk size"))?;
        let padded = size
            .checked_add(size & 1)
            .ok_or_else(|| bad("webp chunk size"))?;
        let chunk_end = at
            .checked_add(8)
            .and_then(|n| n.checked_add(padded))
            .ok_or_else(|| bad("webp chunk size"))?;
        let chunk = body
            .get(at..chunk_end)
            .ok_or_else(|| bad("truncated webp chunk"))?;
        match &kind {
            b"EXIF" | b"XMP " => {}
            b"VP8X" => {
                // Clear the EXIF (0x08) and XMP (0x04) flags: the chunks are gone.
                let mut copy = chunk.to_vec();
                if let Some(flags) = copy.get_mut(8) {
                    *flags &= !0x0C;
                }
                chunks.extend_from_slice(&copy);
            }
            _ => {
                if matches!(&kind, b"VP8 " | b"VP8L" | b"ANMF") {
                    saw_image = true;
                }
                chunks.extend_from_slice(chunk);
            }
        }
        at = chunk_end;
    }
    if !saw_image {
        return Err(bad("webp without image data"));
    }
    let riff = u32::try_from(
        chunks
            .len()
            .checked_add(4)
            .ok_or_else(|| bad("webp size"))?,
    )
    .map_err(|_| bad("webp size"))?;
    let mut out = Vec::with_capacity(chunks.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff.to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&chunks);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{
        SECRET, TestError, TestResult, contains, gif, jpeg, png, webp_extended, webp_lossy,
    };
    use crate::inspect::inspect;

    fn clean(bytes: &[u8]) -> TestResult<Vec<u8>> {
        let format =
            crate::inspect::sniff(bytes).map_err(|e| TestError::Unexpected(e.to_string()))?;
        sanitize(bytes, format).map_err(|e| TestError::Unexpected(e.to_string()))
    }

    fn same_image(before: &[u8], after: &[u8]) -> TestResult {
        let a = inspect(before).map_err(|e| TestError::Unexpected(e.to_string()))?;
        let b = inspect(after).map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert_eq!(a, b);
        Ok(())
    }

    #[test]
    fn png_text_exif_time_and_trailing_data_are_removed() -> TestResult {
        let input = png(
            32,
            32,
            &[
                (b"tEXt", SECRET),
                (b"eXIf", SECRET),
                (b"tIME", &[0; 7]),
                (b"iCCP", b"profile"),
                (b"zzZz", b"unknown ancillary"),
            ],
            b"appended payload",
        );
        let out = clean(&input)?;
        assert!(!contains(&out, SECRET));
        assert!(!contains(&out, b"appended payload"));
        assert!(!contains(&out, b"unknown ancillary"));
        assert!(contains(&out, b"profile"), "the colour profile stays");
        assert!(contains(&out, b"pixel-data"));
        same_image(&input, &out)
    }

    #[test]
    fn a_png_with_an_unknown_critical_chunk_or_no_pixels_is_refused() {
        let critical = png(8, 8, &[(b"ZZZZ", b"x")], &[]);
        assert!(sanitize(&critical, ImageFormat::Png).is_err());
        let mut truncated = png(8, 8, &[], &[]);
        truncated.truncate(truncated.len() - 20);
        assert!(sanitize(&truncated, ImageFormat::Png).is_err());
    }

    #[test]
    fn jpeg_exif_comment_and_trailing_data_are_removed_but_both_scans_stay() -> TestResult {
        let input = jpeg(64, 48, b"appended payload");
        let out = clean(&input)?;
        assert!(!contains(&out, SECRET));
        assert!(!contains(&out, b"secret camera"));
        assert!(!contains(&out, b"appended payload"));
        assert!(contains(&out, b"JFIF"), "JFIF stays");
        // Stuffed byte and restart marker inside the first scan survive.
        assert!(contains(&out, &[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]));
        assert!(contains(&out, &[0x78, 0x9A]), "the second scan stays");
        assert!(out.ends_with(&[0xFF, 0xD9]));
        same_image(&input, &out)
    }

    #[test]
    fn a_truncated_jpeg_is_refused() {
        let input = jpeg(64, 48, &[]);
        for cut in [10, input.len() / 2, input.len() - 1] {
            let slice = input.get(..cut).unwrap_or(&[]);
            assert!(sanitize(slice, ImageFormat::Jpeg).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn gif_comment_xmp_and_trailing_data_are_removed_but_animation_stays() -> TestResult {
        let input = gif(40, 30, b"appended payload");
        let out = clean(&input)?;
        assert!(!contains(&out, b"secret"));
        assert!(!contains(&out, b"XMP DataXMP"));
        assert!(!contains(&out, b"appended payload"));
        assert!(contains(&out, b"NETSCAPE2.0"));
        assert!(out.ends_with(&[0x3B]));
        same_image(&input, &out)
    }

    #[test]
    fn webp_exif_xmp_and_trailing_data_are_removed_and_the_riff_stays_consistent() -> TestResult {
        let extended = webp_extended(100, 80);
        let out = clean(&extended)?;
        assert!(!contains(&out, SECRET));
        let declared = usize::try_from(crate::inspect::le32(&out, 4).unwrap_or(0))
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert_eq!(declared + 8, out.len(), "the RIFF size matches");
        let flags = out.get(20).copied().unwrap_or(0xFF);
        assert_eq!(flags & 0x0C, 0, "the EXIF and XMP flags are cleared");
        same_image(&extended, &out)?;

        let lossy = webp_lossy(50, 40, b"appended payload");
        let out = clean(&lossy)?;
        assert!(!contains(&out, b"appended payload"));
        same_image(&lossy, &out)
    }

    #[test]
    fn sanitizing_is_idempotent() -> TestResult {
        for input in [
            png(16, 16, &[(b"tEXt", SECRET)], b"x"),
            jpeg(16, 16, b"x"),
            gif(16, 16, b"x"),
            webp_extended(16, 16),
        ] {
            let once = clean(&input)?;
            let twice = clean(&once)?;
            assert_eq!(once, twice);
        }
        Ok(())
    }
}
