//! Klassifikation einzelner Dateien in eine [`FileKind`].
//!
//! Vorgehen: zuerst bekannte Dateinamen (`Cargo.lock`, `Dockerfile`,
//! `README` …), dann die Endung (ohne Beachtung der Groß-/Kleinschreibung).
//! Ist beides unbekannt, entscheidet der Dateikopf: Magic-Bytes über
//! [`infer`], ansonsten gültiges UTF-8 ohne NUL-Bytes als Text, sonst binär.
//! Ein Kopf, der mit `%PDF` beginnt, gewinnt immer gegen die Endung.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::FileKind;

/// Anzahl Bytes, die [`classify_path`] vom Dateianfang liest.
const HEAD_LEN: usize = 512;

/// Klassifiziert eine Datei anhand von Name, Endung und Dateikopf.
///
/// Reine Funktion ohne Dateisystemzugriff; `head` sind die ersten (bis zu
/// 512) Bytes der Datei und darf leer sein. Liefert nie [`FileKind::Dir`].
#[must_use]
pub fn classify(path: &Path, head: &[u8]) -> FileKind {
    if head.starts_with(b"%PDF") {
        return FileKind::Pdf;
    }
    let by_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(by_file_name);
    if let Some(kind) = by_name {
        return kind;
    }
    let by_ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| by_extension(&ext.to_ascii_lowercase()));
    if let Some(kind) = by_ext {
        return kind;
    }
    by_content(head)
}

/// Klassifiziert einen (absoluten) Pfad; Verzeichnisse ergeben
/// [`FileKind::Dir`]. Liest höchstens 512 Bytes vom Dateianfang;
/// Lesefehler werden ignoriert (leerer Kopf).
#[must_use]
pub fn classify_path(path: &Path) -> FileKind {
    if path.is_dir() {
        return FileKind::Dir;
    }
    let head = read_head(path).unwrap_or_default();
    classify(path, &head)
}

/// Liest bis zu [`HEAD_LEN`] Bytes vom Anfang der Datei.
fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut head = Vec::with_capacity(HEAD_LEN);
    // `HEAD_LEN` passt immer in `u64`.
    file.take(HEAD_LEN as u64).read_to_end(&mut head)?;
    Ok(head)
}

fn source(lang: &str) -> FileKind {
    FileKind::Source {
        lang: lang.to_owned(),
    }
}

/// Bekannte Dateinamen, die ohne (oder trotz) Endung eindeutig sind.
fn by_file_name(name: &str) -> Option<FileKind> {
    let lower = name.to_ascii_lowercase();
    let kind = match lower.as_str() {
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "poetry.lock"
        | "uv.lock" | "go.sum" | "go.mod" | "gemfile.lock" | "composer.lock" | "flake.lock"
        | "dockerfile" | "containerfile" | "makefile" | "gnumakefile" | "justfile"
        | ".justfile" | "cmakelists.txt" | "procfile" | "vagrantfile" | "gemfile" | "rakefile"
        | "pipfile" | "pipfile.lock" | "requirements.txt" | ".gitignore" | ".gitattributes"
        | ".gitmodules" | ".dockerignore" | ".ignore" | ".editorconfig" | ".npmrc" | ".nvmrc"
        | ".prettierrc" | ".eslintrc" | ".babelrc" | ".envrc" | "rust-toolchain" | "codeowners" => {
            FileKind::Config
        }
        "readme" | "license" | "licence" | "copying" | "authors" | "contributors" | "changelog"
        | "notice" | "todo" | "install" => FileKind::Text,
        _ if lower == ".env" || lower.starts_with(".env.") => FileKind::Config,
        _ if lower.starts_with("dockerfile.") || lower.ends_with(".dockerfile") => FileKind::Config,
        _ => return None,
    };
    Some(kind)
}

/// Abbildung einer kleingeschriebenen Endung auf eine Art.
fn by_extension(ext: &str) -> Option<FileKind> {
    let lang = match ext {
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" | "pyi" | "pyw" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "m" | "mm" => "objc",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "cs" => "csharp",
        "fs" | "fsx" => "fsharp",
        "sh" | "bash" | "zsh" | "fish" => "shell",
        "ps1" | "psm1" => "powershell",
        "bat" | "cmd" => "batch",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" | "less" => "css",
        "vue" => "vue",
        "svelte" => "svelte",
        "lua" => "lua",
        "zig" => "zig",
        "hs" | "lhs" => "haskell",
        "ex" | "exs" => "elixir",
        "erl" | "hrl" => "erlang",
        "dart" => "dart",
        "scala" | "sc" => "scala",
        "clj" | "cljs" | "cljc" | "edn" => "clojure",
        "ml" | "mli" => "ocaml",
        "r" => "r",
        "jl" => "julia",
        "pl" | "pm" => "perl",
        "nix" => "nix",
        "proto" => "proto",
        "graphql" | "gql" => "graphql",
        "tf" | "hcl" => "hcl",
        "wgsl" | "glsl" | "hlsl" | "vert" | "frag" => "shader",
        "asm" | "s" => "asm",
        "v" | "sv" => "verilog",
        "vhd" | "vhdl" => "vhdl",
        "el" => "elisp",
        "vim" => "vim",
        "cmake" => "cmake",
        "gradle" => "gradle",
        "bzl" | "star" => "starlark",
        "wat" => "wasm",
        _ => return other_extension(ext),
    };
    Some(source(lang))
}

/// Nicht-Quelltext-Endungen.
fn other_extension(ext: &str) -> Option<FileKind> {
    let kind = match ext {
        "md" | "markdown" | "mdx" | "mdown" | "mkd" => FileKind::Markdown,
        "pdf" => FileKind::Pdf,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tif" | "tiff"
        | "avif" | "heic" => FileKind::Image,
        "zip" | "tar" | "gz" | "tgz" | "xz" | "txz" | "bz2" | "tbz2" | "7z" | "zst" | "rar"
        | "crate" | "jar" | "war" | "whl" | "lz4" | "lzma" => FileKind::Archive,
        "toml" | "yaml" | "yml" | "json" | "jsonc" | "json5" | "ini" | "cfg" | "conf"
        | "config" | "env" | "lock" | "properties" | "plist" | "xml" | "editorconfig"
        | "gitignore" => FileKind::Config,
        "csv" | "tsv" | "parquet" | "sqlite" | "sqlite3" | "db" | "ndjson" | "jsonl" | "arrow"
        | "feather" | "avro" | "orc" => FileKind::Data,
        "txt" | "text" | "log" | "rst" | "adoc" | "asciidoc" | "org" | "tex" => FileKind::Text,
        _ => return None,
    };
    Some(kind)
}

/// Inhaltsbasierte Klassifikation für unbekannte Endungen.
fn by_content(head: &[u8]) -> FileKind {
    if let Some(kind) = infer::get(head) {
        match (kind.mime_type(), kind.matcher_type()) {
            ("application/pdf", _) => return FileKind::Pdf,
            ("application/vnd.sqlite3" | "application/x-sqlite3", _) => return FileKind::Data,
            (_, infer::MatcherType::Image) => return FileKind::Image,
            (_, infer::MatcherType::Archive) => return FileKind::Archive,
            // Textuelle Treffer (HTML, XML, Shellskript) fallen auf die
            // Textprüfung zurück.
            (_, infer::MatcherType::Text) => {}
            _ => return FileKind::Binary,
        }
    }
    if looks_textual(head) {
        FileKind::Text
    } else {
        FileKind::Binary
    }
}

/// `true`, wenn `head` gültiges UTF-8 ohne NUL-Bytes ist. Ein am Ende
/// abgeschnittenes Mehrbyte-Zeichen wird toleriert.
fn looks_textual(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        // `error_len() == None`: unvollständige Sequenz ganz am Ende.
        Err(error) => error.error_len().is_none() && head.len() - error.valid_up_to() < 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn kind(name: &str) -> FileKind {
        classify(&PathBuf::from(name), b"")
    }

    #[test]
    fn source_extensions_case_insensitive() {
        assert_eq!(kind("main.rs"), source("rust"));
        assert_eq!(kind("MAIN.RS"), source("rust"));
        assert_eq!(kind("app.tsx"), source("typescript"));
        assert_eq!(kind("x.mjs"), source("javascript"));
        assert_eq!(kind("a/b/tool.py"), source("python"));
        assert_eq!(kind("lib.h"), source("c"));
        assert_eq!(kind("lib.hpp"), source("cpp"));
        assert_eq!(kind("run.zsh"), source("shell"));
        assert_eq!(kind("Main.hs"), source("haskell"));
        assert_eq!(kind("mix.exs"), source("elixir"));
        assert_eq!(kind("styles.scss"), source("css"));
        assert_eq!(kind("api.proto"), source("proto"));
        assert_eq!(kind("App.cs"), source("csharp"));
    }

    #[test]
    fn document_media_and_data_extensions() {
        assert_eq!(kind("README.md"), FileKind::Markdown);
        assert_eq!(kind("page.MDX"), FileKind::Markdown);
        assert_eq!(kind("paper.pdf"), FileKind::Pdf);
        assert_eq!(kind("logo.SVG"), FileKind::Image);
        assert_eq!(kind("photo.jpeg"), FileKind::Image);
        assert_eq!(kind("dist.tar.gz"), FileKind::Archive);
        assert_eq!(kind("serde-1.0.crate"), FileKind::Archive);
        assert_eq!(kind("table.csv"), FileKind::Data);
        assert_eq!(kind("events.jsonl"), FileKind::Data);
        assert_eq!(kind("app.sqlite"), FileKind::Data);
    }

    #[test]
    fn config_extensions_and_well_known_names() {
        for name in [
            "Cargo.toml",
            "config.yml",
            "tsconfig.json",
            "settings.ini",
            "Cargo.lock",
            "package-lock.json",
            "Dockerfile",
            "Makefile",
            "justfile",
            ".gitignore",
            ".editorconfig",
            ".env",
            ".env.local",
            "sub/dir/pnpm-lock.yaml",
        ] {
            assert_eq!(kind(name), FileKind::Config, "{name}");
        }
    }

    #[test]
    fn plain_text_names() {
        assert_eq!(kind("README"), FileKind::Text);
        assert_eq!(kind("LICENSE"), FileKind::Text);
        assert_eq!(kind("notes.txt"), FileKind::Text);
    }

    #[test]
    fn unknown_extension_uses_content() {
        let png = [
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D,
        ];
        assert_eq!(classify(Path::new("blob.xyz"), &png), FileKind::Image);
        let zip = [b'P', b'K', 3, 4, 20, 0, 0, 0];
        assert_eq!(classify(Path::new("blob.xyz"), &zip), FileKind::Archive);
        assert_eq!(classify(Path::new("doc.bin"), b"%PDF-1.7\n"), FileKind::Pdf);
        assert_eq!(
            classify(Path::new("notes.xyz"), "hällo\nwelt".as_bytes()),
            FileKind::Text
        );
        assert_eq!(
            classify(Path::new("data.xyz"), &[1, 0, 2, 3]),
            FileKind::Binary
        );
        assert_eq!(
            classify(Path::new("data.xyz"), &[0xff, 0xfe, 0x41]),
            FileKind::Binary
        );
        assert_eq!(classify(Path::new("noext"), b""), FileKind::Text);
    }

    #[test]
    fn truncated_utf8_at_end_is_text() {
        let mut head = b"abc ".to_vec();
        head.extend_from_slice(&"ä".as_bytes()[..1]);
        assert_eq!(classify(Path::new("x.unknown"), &head), FileKind::Text);
    }

    #[test]
    fn pdf_magic_overrides_text_extension() {
        assert_eq!(classify(Path::new("fake.txt"), b"%PDF-1.4"), FileKind::Pdf);
        assert_eq!(classify(Path::new("fake.md"), b"%PDF-1.4"), FileKind::Pdf);
    }

    #[test]
    fn classify_path_reads_head_and_detects_dirs() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        assert_eq!(classify_path(dir.path()), FileKind::Dir);

        let pdf = dir.path().join("scan");
        let mut bytes = b"%PDF-1.5\n".to_vec();
        bytes.resize(4096, b'x');
        std::fs::write(&pdf, &bytes)?;
        assert_eq!(classify_path(&pdf), FileKind::Pdf);

        let rs = dir.path().join("lib.rs");
        std::fs::write(&rs, "fn main() {}\n")?;
        assert_eq!(classify_path(&rs), source("rust"));

        let bin = dir.path().join("blob");
        std::fs::write(&bin, [0u8, 1, 2, 3, 255])?;
        assert_eq!(classify_path(&bin), FileKind::Binary);

        let missing = dir.path().join("missing.xyz");
        assert_eq!(classify_path(&missing), FileKind::Text);
        Ok(())
    }

    #[test]
    fn read_head_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("big.txt");
        std::fs::write(&file, vec![b'a'; 10_000])?;
        assert_eq!(read_head(&file)?.len(), HEAD_LEN);
        Ok(())
    }
}
