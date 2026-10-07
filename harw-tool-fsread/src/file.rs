//! `fsread.file` — Magic-Erkennung wie `file`, in reinem Rust.
//!
//! Liest höchstens die ersten [`SNIFF_BYTES`] jeder Datei und bestimmt Art,
//! MIME-Typ und Beschreibung: ELF (Klasse, Endianness, Typ, Architektur), PNG
//! und GIF (Abmessungen), JPEG, PDF, ZIP, gzip, bzip2, xz, zstd, tar, 7z, RAR,
//! SQLite, WebAssembly, Mach-O, PE, Java-Klassen, Medien (WAV, WebP, MP4, OGG,
//! FLAC, MP3), Skripte (Shebang), XML/HTML/SVG/JSON sowie Text
//! (ASCII/UTF-8/UTF-16 mit BOM, CRLF) und „data“ für alles andere.
//!
//! Symlinks werden nicht gefolgt (Beschreibung `symbolic link to <ziel>`).
//! Geheimnis-Pfade (`.env`, Schlüsseldateien) werden nicht gelesen.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::ok;
use crate::io::SNIFF_BYTES;
use crate::meta::{Kind, Meta};
use crate::scope::{Scope, io_message, is_secret_path};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.file";

/// Höchstzahl Pfade je Aufruf.
pub const MAX_PATHS: usize = 32;

/// Argumente für `fsread.file`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct FileArgs {
    /// Paths relative to the workspace root (1-32).
    pub paths: Vec<String>,
}

/// Ergebnis der Erkennung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    /// Menschenlesbare Beschreibung.
    pub description: String,
    /// MIME-Typ.
    pub mime: &'static str,
    /// Grobe Art: `text`, `binary`, `empty`, `executable`, `archive`, `image`, `media`, `document`.
    pub kind: &'static str,
}

fn det(description: impl Into<String>, mime: &'static str, kind: &'static str) -> Detection {
    Detection {
        description: description.into(),
        mime,
        kind,
    }
}

fn u16_at(bytes: &[u8], at: usize, big: bool) -> Option<u16> {
    let raw: [u8; 2] = bytes.get(at..at + 2)?.try_into().ok()?;
    Some(if big {
        u16::from_be_bytes(raw)
    } else {
        u16::from_le_bytes(raw)
    })
}

fn u32_at(bytes: &[u8], at: usize, big: bool) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(if big {
        u32::from_be_bytes(raw)
    } else {
        u32::from_le_bytes(raw)
    })
}

fn elf(bytes: &[u8]) -> Detection {
    let class = match bytes.get(4) {
        Some(1) => "32-bit",
        Some(2) => "64-bit",
        _ => "unknown class",
    };
    let big = bytes.get(5) == Some(&2);
    let order = if big { "MSB" } else { "LSB" };
    let kind = match u16_at(bytes, 16, big) {
        Some(1) => "relocatable",
        Some(2) => "executable",
        Some(3) => "shared object",
        Some(4) => "core file",
        _ => "unknown type",
    };
    let machine = match u16_at(bytes, 18, big) {
        Some(0x03) => "Intel 80386",
        Some(0x3E) => "x86-64",
        Some(0x28) => "ARM",
        Some(0xB7) => "ARM aarch64",
        Some(0xF3) => "UCB RISC-V",
        Some(0x08) => "MIPS",
        Some(0x14) => "PowerPC",
        Some(0x15) => "64-bit PowerPC",
        Some(0x16) => "IBM S/390",
        Some(0x2A) => "SuperH",
        Some(0x5E) => "Xtensa",
        Some(0x102) => "LoongArch",
        _ => "unknown machine",
    };
    det(
        format!("ELF {class} {order} {kind}, {machine}"),
        "application/x-elf",
        "executable",
    )
}

fn macho(bytes: &[u8]) -> Option<Detection> {
    let magic = u32_at(bytes, 0, true)?;
    let (bits, order) = match magic {
        0xFEED_FACE => ("32-bit", "big-endian"),
        0xFEED_FACF => ("64-bit", "big-endian"),
        0xCEFA_EDFE => ("32-bit", "little-endian"),
        0xCFFA_EDFE => ("64-bit", "little-endian"),
        0xCAFE_BABE => {
            // Java-Klasse und Mach-O-Fat teilen die Magic; Java hat Hauptversion >= 45.
            let count = u32_at(bytes, 4, true)?;
            return Some(if count >= 45 {
                det(
                    format!("compiled Java class data, version {}", count),
                    "application/java-vm",
                    "executable",
                )
            } else {
                det(
                    "Mach-O universal binary",
                    "application/x-mach-binary",
                    "executable",
                )
            });
        }
        _ => return None,
    };
    Some(det(
        format!("Mach-O {bits} {order} executable"),
        "application/x-mach-binary",
        "executable",
    ))
}

fn text_description(bytes: &[u8]) -> Option<(String, &'static str)> {
    if bytes.is_empty() {
        return None;
    }
    if bytes.contains(&0) {
        return None;
    }
    let valid = match std::str::from_utf8(bytes) {
        Ok(_) => true,
        // Ein am Ende abgeschnittenes Zeichen ist kein Fehler.
        Err(error) => error.error_len().is_none() && error.valid_up_to() + 4 > bytes.len(),
    };
    if !valid {
        return None;
    }
    let ascii = bytes.is_ascii();
    let crlf = bytes.windows(2).any(|w| w == b"\r\n");
    let mut text = if ascii {
        "ASCII text".to_owned()
    } else {
        "Unicode text, UTF-8".to_owned()
    };
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        text = "Unicode text, UTF-8 (with BOM)".to_owned();
    }
    if crlf {
        text.push_str(", with CRLF line terminators");
    }
    Some((
        text,
        if ascii {
            "text/plain"
        } else {
            "text/plain; charset=utf-8"
        },
    ))
}

fn shebang(bytes: &[u8], text: &str) -> Option<Detection> {
    let first = bytes.strip_prefix(b"#!")?;
    let line_end = first
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(first.len());
    let line = String::from_utf8_lossy(&first[..line_end])
        .trim()
        .to_owned();
    let mut parts = line.split_whitespace();
    let program = parts.next().unwrap_or("");
    let base = program.rsplit('/').next().unwrap_or(program);
    let interpreter = if base == "env" {
        parts.find(|part| !part.starts_with('-')).unwrap_or("env")
    } else {
        base
    };
    let name = interpreter.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    let (label, mime) = match name {
        "sh" | "bash" | "dash" | "zsh" | "ksh" | "ash" => {
            ("POSIX shell script", "text/x-shellscript")
        }
        "python" => ("Python script", "text/x-script.python"),
        "perl" => ("Perl script", "text/x-perl"),
        "ruby" => ("Ruby script", "text/x-ruby"),
        "node" | "nodejs" => ("Node.js script", "application/javascript"),
        "php" => ("PHP script", "text/x-php"),
        "lua" => ("Lua script", "text/x-lua"),
        other => {
            return Some(det(
                format!("script, {other} interpreter, {text} executable"),
                "text/x-script",
                "text",
            ));
        }
    };
    Some(det(format!("{label}, {text} executable"), mime, "text"))
}

/// Erkennt den Inhalt anhand seiner ersten Bytes. `size` ist die Dateigröße.
#[must_use]
pub fn detect(bytes: &[u8], size: u64) -> Detection {
    if size == 0 || bytes.is_empty() {
        return det("empty", "inode/x-empty", "empty");
    }
    if bytes.starts_with(&[0x7F, b'E', b'L', b'F']) {
        return elf(bytes);
    }
    if let Some(found) = macho(bytes) {
        return found;
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let dims = u32_at(bytes, 16, true).zip(u32_at(bytes, 20, true));
        let text = dims.map_or_else(
            || "PNG image data".to_owned(),
            |(w, h)| format!("PNG image data, {w} x {h}"),
        );
        return det(text, "image/png", "image");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let version = if bytes.starts_with(b"GIF87a") {
            "87a"
        } else {
            "89a"
        };
        let dims = u16_at(bytes, 6, false).zip(u16_at(bytes, 8, false));
        let text = dims.map_or_else(
            || format!("GIF image data, version {version}"),
            |(w, h)| format!("GIF image data, version {version}, {w} x {h}"),
        );
        return det(text, "image/gif", "image");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return det("JPEG image data", "image/jpeg", "image");
    }
    if bytes.starts_with(b"BM") && bytes.len() > 14 && u32_at(bytes, 6, false) == Some(0) {
        return det("PC bitmap", "image/bmp", "image");
    }
    if bytes.starts_with(&[0, 0, 1, 0]) {
        return det(
            "MS Windows icon resource",
            "image/vnd.microsoft.icon",
            "image",
        );
    }
    if bytes.starts_with(b"%PDF-") {
        let version: String = bytes[5..]
            .iter()
            .take_while(|b| b.is_ascii_digit() || **b == b'.')
            .map(|b| char::from(*b))
            .collect();
        return det(
            format!("PDF document, version {version}"),
            "application/pdf",
            "document",
        );
    }
    if bytes.starts_with(b"%!PS") {
        return det(
            "PostScript document text",
            "application/postscript",
            "document",
        );
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return det("Zip archive data", "application/zip", "archive");
    }
    if bytes.starts_with(&[0x1F, 0x8B]) {
        return det("gzip compressed data", "application/gzip", "archive");
    }
    if bytes.starts_with(b"BZh") {
        return det("bzip2 compressed data", "application/x-bzip2", "archive");
    }
    if bytes.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return det("XZ compressed data", "application/x-xz", "archive");
    }
    if bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return det("Zstandard compressed data", "application/zstd", "archive");
    }
    if bytes.starts_with(&[0x04, 0x22, 0x4D, 0x18]) {
        return det("LZ4 compressed data", "application/x-lz4", "archive");
    }
    if bytes.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return det(
            "7-zip archive data",
            "application/x-7z-compressed",
            "archive",
        );
    }
    if bytes.starts_with(b"Rar!\x1a\x07") {
        return det("RAR archive data", "application/vnd.rar", "archive");
    }
    if bytes.starts_with(b"!<arch>\n") {
        return det("current ar archive", "application/x-archive", "archive");
    }
    if bytes.get(257..262) == Some(b"ustar") {
        return det("POSIX tar archive", "application/x-tar", "archive");
    }
    if bytes.starts_with(b"SQLite format 3\0") {
        return det("SQLite 3.x database", "application/vnd.sqlite3", "binary");
    }
    if bytes.starts_with(b"\0asm") {
        return det(
            "WebAssembly (wasm) binary module",
            "application/wasm",
            "executable",
        );
    }
    if bytes.starts_with(b"MZ") && bytes.len() > 64 {
        return det(
            "PE32 executable (MS-DOS/Windows)",
            "application/vnd.microsoft.portable-executable",
            "executable",
        );
    }
    if bytes.starts_with(b"RIFF") && bytes.len() >= 12 {
        return match &bytes[8..12] {
            b"WAVE" => det(
                "RIFF (little-endian) data, WAVE audio",
                "audio/wav",
                "media",
            ),
            b"WEBP" => det(
                "RIFF (little-endian) data, Web/P image",
                "image/webp",
                "image",
            ),
            b"AVI " => det("RIFF (little-endian) data, AVI", "video/x-msvideo", "media"),
            _ => det(
                "RIFF (little-endian) data",
                "application/octet-stream",
                "binary",
            ),
        };
    }
    if bytes.get(4..8) == Some(b"ftyp") {
        return det("ISO Media (MP4/QuickTime)", "video/mp4", "media");
    }
    if bytes.starts_with(b"OggS") {
        return det("Ogg data", "application/ogg", "media");
    }
    if bytes.starts_with(b"fLaC") {
        return det("FLAC audio bitstream data", "audio/flac", "media");
    }
    if bytes.starts_with(b"ID3") || bytes.starts_with(&[0xFF, 0xFB]) {
        return det("MPEG audio (MP3)", "audio/mpeg", "media");
    }
    let sample = &bytes[..bytes.len().min(SNIFF_BYTES)];
    if sample.starts_with(&[0xFF, 0xFE]) || sample.starts_with(&[0xFE, 0xFF]) {
        return det(
            "Unicode text, UTF-16 (with BOM)",
            "text/plain; charset=utf-16",
            "text",
        );
    }
    if let Some((text, mime)) = text_description(sample) {
        if let Some(found) = shebang(sample, &text) {
            return found;
        }
        let trimmed = String::from_utf8_lossy(sample);
        let trimmed = trimmed.trim_start_matches('\u{feff}').trim_start();
        let lower: String = trimmed
            .chars()
            .take(64)
            .collect::<String>()
            .to_ascii_lowercase();
        if lower.starts_with("<?xml") {
            return det(format!("XML 1.0 document, {text}"), "text/xml", "text");
        }
        if lower.starts_with("<svg") || (lower.starts_with("<?xml") && trimmed.contains("<svg")) {
            return det(
                format!("SVG Scalable Vector Graphics image, {text}"),
                "image/svg+xml",
                "text",
            );
        }
        if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
            return det(format!("HTML document, {text}"), "text/html", "text");
        }
        if size <= SNIFF_BYTES as u64
            && (trimmed.starts_with('{') || trimmed.starts_with('['))
            && serde_json::from_str::<Value>(trimmed).is_ok()
        {
            return det(
                format!("JSON text data, {text}"),
                "application/json",
                "text",
            );
        }
        return det(text, mime, "text");
    }
    det("data", "application/octet-stream", "binary")
}

fn describe(scope: &Scope, input: &str) -> Value {
    let rel = match scope.rel(input) {
        Ok(rel) => rel,
        Err(error) => return json!({"path": input, "ok": false, "error": error.to_string()}),
    };
    let shown = rel.display();
    let stat = match scope.lstat(&rel) {
        Ok(stat) => stat,
        Err(error) => return json!({"path": shown, "ok": false, "error": io_message(&error)}),
    };
    let meta = Meta::from_stat(&stat);
    let simple = |description: &str, mime: &'static str, kind: &'static str| json!({"path": shown, "ok": true, "description": description, "mime": mime, "kind": kind});
    match meta.kind {
        Kind::Dir => simple("directory", "inode/directory", "directory"),
        Kind::Symlink => {
            let target = scope
                .read_link(&rel)
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_default();
            json!({
                "path": shown, "ok": true, "kind": "symlink",
                "description": format!("symbolic link to {target}"),
                "mime": "inode/symlink", "target": target,
            })
        }
        Kind::Fifo => simple("fifo (named pipe)", "inode/fifo", "special"),
        Kind::Socket => simple("socket", "inode/socket", "special"),
        Kind::CharDevice => simple("character special", "inode/chardevice", "special"),
        Kind::BlockDevice => simple("block special", "inode/blockdevice", "special"),
        Kind::Unknown => simple("unknown file type", "application/octet-stream", "special"),
        Kind::File => {
            if is_secret_path(rel.as_path()) {
                return json!({"path": shown, "ok": false, "error": format!("path '{shown}' is protected (secret material)")});
            }
            let mut file = match scope.open_read(&rel) {
                Ok(file) => file,
                Err(error) => {
                    return json!({"path": shown, "ok": false, "error": error.to_string()});
                }
            };
            let mut buf = Vec::new();
            if let Err(error) = file.by_ref().take(SNIFF_BYTES as u64).read_to_end(&mut buf) {
                return json!({"path": shown, "ok": false, "error": io_message(&error)});
            }
            let found = detect(&buf, meta.size);
            json!({
                "path": shown, "ok": true, "description": found.description,
                "mime": found.mime, "kind": found.kind, "size": meta.size,
            })
        }
    }
}

/// Führt `fsread.file` aus.
#[must_use]
pub fn run(root: &Path, args: &FileArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        if args.paths.is_empty() {
            return Err("paths must contain at least one path".to_owned());
        }
        if args.paths.len() > MAX_PATHS {
            return Err(format!("too many paths (max {MAX_PATHS})"));
        }
        let results: Vec<Value> = args.paths.iter().map(|p| describe(scope, p)).collect();
        let failed = results.iter().filter(|r| r["ok"] == false).count();
        Ok(ok(
            TOOL,
            format!(
                "{} paths identified, {failed} failed",
                results.len() - failed
            ),
            json!({"results": results, "failed": failed, "truncated": false}),
        ))
    })
}

/// Erkennt Dateitypen wie `file`.
#[harw_macros::tool(
    name = "fsread.file",
    description = "Identifies file types by magic bytes like file(1): ELF (class/arch), PNG/GIF dimensions, JPEG, PDF, archives, SQLite, WebAssembly, scripts via shebang, XML/HTML/JSON and text encodings. Use when you need to know what a file is before reading or hashing it instead of running file. Returns JSON {results:[{path, description, mime, kind}], failed}. Reads at most 8 KiB per file, never follows symlinks, never reads secret files.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_file(
    context: &ToolExecutionContext,
    args: FileArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, error_of, json_of};

    fn d(bytes: &[u8]) -> Detection {
        detect(bytes, bytes.len() as u64)
    }

    #[test]
    fn empty_and_text_kinds() -> TestResult {
        assert_eq!(detect(b"", 0).description, "empty");
        assert_eq!(d(b"hello\n").description, "ASCII text");
        assert_eq!(d("grüße\n".as_bytes()).description, "Unicode text, UTF-8");
        assert_eq!(
            d(b"a\r\nb\r\n").description,
            "ASCII text, with CRLF line terminators"
        );
        assert_eq!(
            d(&[0xEF, 0xBB, 0xBF, b'a']).description,
            "Unicode text, UTF-8 (with BOM)"
        );
        assert_eq!(
            d(&[0xFF, 0xFE, b'a', 0]).description,
            "Unicode text, UTF-16 (with BOM)"
        );
        assert_eq!(d(&[0xC3, 0x28, b'x']).description, "data");
        assert_eq!(d(&[1, 2, 0, 4]).kind, "binary");
        Ok(())
    }

    #[test]
    fn utf8_cut_at_sniff_boundary_is_still_text() -> TestResult {
        let mut bytes = "ü".repeat(10).into_bytes();
        bytes.pop(); // schneidet das letzte Zeichen mittendrin ab
        assert_eq!(d(&bytes).kind, "text");
        Ok(())
    }

    #[test]
    fn scripts_and_markup() -> TestResult {
        assert_eq!(
            d(b"#!/bin/bash\necho hi\n").description,
            "POSIX shell script, ASCII text executable"
        );
        assert_eq!(
            d(b"#!/usr/bin/env python3\nprint(1)\n").description,
            "Python script, ASCII text executable"
        );
        assert!(d(b"#!/usr/bin/awk -f\n{}\n").description.contains("awk"));
        assert!(
            d(b"<?xml version=\"1.0\"?><a/>")
                .description
                .starts_with("XML 1.0 document")
        );
        assert!(
            d(b"<!DOCTYPE html><html></html>")
                .description
                .starts_with("HTML document")
        );
        assert!(
            d(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
                .description
                .starts_with("SVG")
        );
        assert!(
            d(b"{\"a\": [1, 2]}")
                .description
                .starts_with("JSON text data")
        );
        assert_eq!(d(b"{not json").description, "ASCII text");
        Ok(())
    }

    #[test]
    fn binary_magics() -> TestResult {
        let mut elf = vec![0x7F, b'E', b'L', b'F', 2, 1, 1, 0];
        elf.resize(16, 0);
        elf.extend_from_slice(&[3, 0, 0x3E, 0]);
        assert_eq!(d(&elf).description, "ELF 64-bit LSB shared object, x86-64");
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(d(&png).description, "PNG image data, 640 x 480");
        assert_eq!(
            d(b"GIF89a\x0a\x00\x14\x00").description,
            "GIF image data, version 89a, 10 x 20"
        );
        assert_eq!(d(&[0xFF, 0xD8, 0xFF, 0xE0]).mime, "image/jpeg");
        assert_eq!(d(b"%PDF-1.7\n").description, "PDF document, version 1.7");
        assert_eq!(d(b"PK\x03\x04rest").kind, "archive");
        assert_eq!(d(&[0x1F, 0x8B, 8, 0]).mime, "application/gzip");
        assert_eq!(d(b"BZh91AY").mime, "application/x-bzip2");
        assert_eq!(
            d(&[0xFD, b'7', b'z', b'X', b'Z', 0, 1]).mime,
            "application/x-xz"
        );
        assert_eq!(d(&[0x28, 0xB5, 0x2F, 0xFD, 0]).mime, "application/zstd");
        assert_eq!(d(b"SQLite format 3\0abc").mime, "application/vnd.sqlite3");
        assert_eq!(d(b"\0asm\x01\0\0\0").mime, "application/wasm");
        let mut tar = vec![0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(d(&tar).mime, "application/x-tar");
        let mut mp4 = vec![0, 0, 0, 0x18];
        mp4.extend_from_slice(b"ftypisom");
        assert_eq!(d(&mp4).mime, "video/mp4");
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.resize(20, 0);
        assert_eq!(d(&wav).mime, "audio/wav");
        assert_eq!(
            d(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52]).mime,
            "application/java-vm"
        );
        assert_eq!(
            d(&[0xCF, 0xFA, 0xED, 0xFE, 7, 0, 0, 1]).mime,
            "application/x-mach-binary"
        );
        Ok(())
    }

    #[test]
    fn tool_describes_dirs_links_files_and_refuses_secrets() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("src/main.rs", b"fn main() {}\n")?;
        fx.write(".env", b"A=1")?;
        fx.write("empty", b"")?;
        let value = json_of(run(
            &fx.ws,
            &serde_json::from_value(
                json!({"paths": ["src", "src/main.rs", "empty", "link_file", "../outside/secret.txt", ".env", "missing"]}),
            )?,
        ))?;
        let r = value["results"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        assert_eq!(r[0]["description"], "directory");
        assert_eq!(r[1]["description"], "ASCII text");
        assert_eq!(r[2]["description"], "empty");
        assert_eq!(r[3]["kind"], "symlink");
        assert!(
            r[3]["description"]
                .as_str()
                .is_some_and(|d| d.starts_with("symbolic link to "))
        );
        for entry in &r[4..7] {
            assert_eq!(entry["ok"], false, "{entry}");
        }
        assert_eq!(value["failed"], 3);
        error_of(run(&fx.ws, &serde_json::from_value(json!({"paths": []}))?))?;
        Ok(())
    }
}
