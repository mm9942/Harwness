//! Synthetic images for the tests: valid containers around placeholder pixel
//! data (nothing here is decoded, so the data bytes only have to be bytes).

harw_test_support::define_test_error!(pub(crate));

pub const SECRET: &[u8] = b"GPS-SECRET-48.1372N-11.5755E";

fn be32(n: u32) -> [u8; 4] {
    n.to_be_bytes()
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&be32(u32::try_from(data.len()).unwrap_or(0)));
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0, 0, 0, 0]); // CRC is not checked by the sanitizer
    out
}

/// A PNG with the given extra chunks between IHDR and IDAT, and `tail` after IEND.
pub fn png(width: u32, height: u32, extra: &[(&[u8; 4], &[u8])], tail: &[u8]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(width));
    ihdr.extend_from_slice(&be32(height));
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    out.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    for (kind, data) in extra {
        out.extend_from_slice(&png_chunk(kind, data));
    }
    out.extend_from_slice(&png_chunk(b"IDAT", b"pixel-data"));
    out.extend_from_slice(&png_chunk(b"IEND", b""));
    out.extend_from_slice(tail);
    out
}

fn jpeg_segment(code: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, code];
    out.extend_from_slice(&u16::try_from(data.len() + 2).unwrap_or(2).to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// A JPEG with an EXIF segment holding [`SECRET`], a comment, a JFIF header,
/// two scans (the first with byte stuffing and a restart marker) and `tail`
/// after EOI.
pub fn jpeg(width: u16, height: u16, tail: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    out.extend_from_slice(&jpeg_segment(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend_from_slice(SECRET);
    out.extend_from_slice(&jpeg_segment(0xE1, &exif));
    out.extend_from_slice(&jpeg_segment(0xFE, b"made with a secret camera"));
    out.extend_from_slice(&jpeg_segment(0xDB, &[0; 65]));
    let mut frame = vec![8];
    frame.extend_from_slice(&height.to_be_bytes());
    frame.extend_from_slice(&width.to_be_bytes());
    frame.extend_from_slice(&[1, 1, 0x11, 0]);
    out.extend_from_slice(&jpeg_segment(0xC2, &frame)); // progressive
    out.extend_from_slice(&jpeg_segment(0xC4, &[0; 20]));
    out.extend_from_slice(&jpeg_segment(0xDA, &[1, 1, 0, 0, 0x3F, 0]));
    out.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]); // stuffing + RST0
    out.extend_from_slice(&jpeg_segment(0xDA, &[1, 1, 0, 0, 0x3F, 0]));
    out.extend_from_slice(&[0x78, 0x9A]);
    out.extend_from_slice(&[0xFF, 0xD9]);
    out.extend_from_slice(tail);
    out
}

/// A GIF with a comment, an XMP application extension, a NETSCAPE loop
/// extension, a graphic control extension, one image and `tail` after the trailer.
pub fn gif(width: u16, height: u16, tail: &[u8]) -> Vec<u8> {
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&[0x80, 0, 0]); // global table of 2 colours
    out.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    out.extend_from_slice(&[0x21, 0xFE, 6]);
    out.extend_from_slice(b"secret");
    out.push(0);
    out.extend_from_slice(&[0x21, 0xFF, 11]);
    out.extend_from_slice(b"XMP DataXMP");
    out.extend_from_slice(&[4]);
    out.extend_from_slice(SECRET.get(..4).unwrap_or(b"GPS-"));
    out.push(0);
    out.extend_from_slice(&[0x21, 0xFF, 11]);
    out.extend_from_slice(b"NETSCAPE2.0");
    out.extend_from_slice(&[3, 1, 0, 0, 0]);
    out.extend_from_slice(&[0x21, 0xF9, 4, 0, 10, 0, 0, 0]);
    out.extend_from_slice(&[0x2C, 0, 0, 0, 0]);
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&[0, 2, 3, 1, 2, 3, 0]); // LZW code size 2, one 3-byte block
    out.push(0x3B);
    out.extend_from_slice(tail);
    out
}

fn riff(chunks: &[u8]) -> Vec<u8> {
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&u32::try_from(chunks.len() + 4).unwrap_or(4).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(chunks);
    out
}

fn webp_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = kind.to_vec();
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A lossy WebP (plain `VP8 `) with `tail` after the RIFF payload.
pub fn webp_lossy(width: u16, height: u16, tail: &[u8]) -> Vec<u8> {
    let mut data = vec![0x10, 0x02, 0x00, 0x9D, 0x01, 0x2A];
    data.extend_from_slice(&width.to_le_bytes());
    data.extend_from_slice(&height.to_le_bytes());
    data.extend_from_slice(b"pixel-data");
    let mut out = riff(&webp_chunk(b"VP8 ", &data));
    out.extend_from_slice(tail);
    out
}

/// An extended WebP with EXIF and XMP chunks holding [`SECRET`].
pub fn webp_extended(width: u32, height: u32) -> Vec<u8> {
    let mut vp8x = vec![0x0C, 0, 0, 0]; // EXIF and XMP flags set
    let w = (width - 1).to_le_bytes();
    let h = (height - 1).to_le_bytes();
    vp8x.extend_from_slice(&w[..3]);
    vp8x.extend_from_slice(&h[..3]);
    let mut chunks = webp_chunk(b"VP8X", &vp8x);
    let mut lossless = vec![0x2F];
    let bits = (width - 1) | ((height - 1) << 14);
    lossless.extend_from_slice(&bits.to_le_bytes());
    lossless.extend_from_slice(b"pixel-data");
    chunks.extend_from_slice(&webp_chunk(b"VP8L", &lossless));
    chunks.extend_from_slice(&webp_chunk(b"EXIF", SECRET));
    chunks.extend_from_slice(&webp_chunk(b"XMP ", SECRET));
    riff(&chunks)
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
