//! Catalog and on-disk state of the on-device language models.
//!
//! Layout of a models directory (see the K-W2 contract): a verified model is
//! `<file_name>` plus a JSON sidecar `<file_name>.json`; the platform layer
//! downloads into `<file_name>.part` or copies imports into a staging file
//! `.harw-import-<16 hex>.gguf.part`. This module checks size, GGUF magic and
//! SHA-256 in one read pass (with progress) and promotes files atomically.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aws_lc_rs::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};

use crate::MobileError;

const HASH_BUFFER_BYTES: usize = 1024 * 1024;
const PROGRESS_STEP_BYTES: u64 = 32 * 1024 * 1024;
const PART_SUFFIX: &str = ".part";
const SIDECAR_SUFFIX: &str = ".json";
const SIDECAR_TMP_SUFFIX: &str = ".json.tmp";
const STAGING_PREFIX: &str = ".harw-import-";
const STAGING_SUFFIX: &str = ".gguf.part";
const STAGING_RANDOM_HEX: usize = 16;
const MODEL_SUFFIX: &str = ".gguf";
const MAX_FILE_NAME_BYTES: usize = 200;
const SIDECAR_SCHEMA: u32 = 1;
const SIDECAR_MAX_BYTES: u64 = 65536;
const OWN_ID_PREFIX: &str = "own-";
const GGUF_MAGIC: &[u8; 4] = b"GGUF";
const GIB: u64 = 1024 * 1024 * 1024;
const DEFAULT_N_CTX: u32 = 16384;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CatalogModel {
    pub id: String,
    pub label: String,
    pub repo: String,
    pub revision: String,
    pub file_name: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub min_ram_bytes: u64,
    pub recommended: bool,
    pub default_n_ctx: u32,
    /// Short German hint shown next to the entry; empty if none.
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ModelFileState {
    Missing,
    Partial,
    Complete,
    Invalid,
    /// The file is visible but cannot be opened (foreign owner after reinstall).
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ModelOrigin {
    Catalog,
    Imported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ModelVerification {
    /// Sidecar says verified and size plus hash match the catalog.
    Verified,
    /// Own model, hash known from the sidecar, no catalog match.
    Unverified,
    /// File present but sidecar missing or invalid.
    Unchecked,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LocalModel {
    pub id: String,
    pub file_name: String,
    pub state: ModelFileState,
    pub bytes_on_disk: u64,
    pub size_bytes: u64,
    pub label: String,
    pub origin: ModelOrigin,
    pub verification: ModelVerification,
    pub sha256: Option<String>,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ImportVerdict {
    Verified {
        catalog_id: String,
    },
    Unverified,
    AlreadyPresent {
        model_id: String,
    },
    Mismatch {
        catalog_id: String,
        label: String,
        expected_size: u64,
        actual_size: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ImportCheck {
    pub staged_name: String,
    pub final_name: String,
    pub model_id: String,
    pub label: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub verdict: ImportVerdict,
}

/// Progress of the SHA-256 pass. Called synchronously from the calling thread.
#[uniffi::export(with_foreign)]
pub trait ModelProgress: Send + Sync {
    /// Return `false` to cancel.
    fn on_progress(&self, done_bytes: u64, total_bytes: u64) -> bool;
}

struct CatalogRow {
    id: &'static str,
    label: &'static str,
    repo: &'static str,
    revision: &'static str,
    file_name: &'static str,
    size_bytes: u64,
    sha256: &'static str,
    min_ram_bytes: u64,
    recommended: bool,
    default_n_ctx: u32,
    note: &'static str,
}

/// Display order. Exactly one row is recommended.
const CATALOG: &[CatalogRow] = &[
    CatalogRow {
        id: "qwen3.5-9b-iq4xs",
        label: "Qwen3.5 9B \u{b7} IQ4_XS",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        revision: "3885219b6810b007914f3a7950a8d1b469d598a5",
        file_name: "Qwen3.5-9B-IQ4_XS.gguf",
        size_bytes: 5168653536,
        sha256: "7e918aeca06c52bcb528ea6b04b4ec957e75ee8c0a73138854c0dfcf371ea429",
        min_ram_bytes: 8 * GIB,
        recommended: true,
        default_n_ctx: DEFAULT_N_CTX,
        note: "Erste Wahl",
    },
    CatalogRow {
        id: "qwen3.5-9b-iq4nl",
        label: "Qwen3.5 9B \u{b7} IQ4_NL",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        revision: "3885219b6810b007914f3a7950a8d1b469d598a5",
        file_name: "Qwen3.5-9B-IQ4_NL.gguf",
        size_bytes: 5371028704,
        sha256: "12fd6b43e298ae4c8d374e64e8c2406c252d109ead47dffb46e75be3566ed0e5",
        min_ram_bytes: 8 * GIB,
        recommended: false,
        default_n_ctx: DEFAULT_N_CTX,
        note: "Interessant für ARM-CPU",
    },
    CatalogRow {
        id: "qwen3.5-9b-q4km",
        label: "Qwen3.5 9B \u{b7} Q4_K_M",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        revision: "3885219b6810b007914f3a7950a8d1b469d598a5",
        file_name: "Qwen3.5-9B-Q4_K_M.gguf",
        size_bytes: 5680522464,
        sha256: "03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8",
        min_ram_bytes: 8 * GIB,
        recommended: false,
        default_n_ctx: DEFAULT_N_CTX,
        note: "Klassische 4-Bit-Quantisierung",
    },
    CatalogRow {
        id: "qwen3.5-9b-ud-q4kxl",
        label: "Qwen3.5 9B \u{b7} UD-Q4_K_XL",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        revision: "3885219b6810b007914f3a7950a8d1b469d598a5",
        file_name: "Qwen3.5-9B-UD-Q4_K_XL.gguf",
        size_bytes: 5966095584,
        sha256: "6f5d30666c2d8ae16a306e616d95341dcf3cc46810df84d7e6f5a7d1e4c1b293",
        min_ram_bytes: 9 * GIB,
        recommended: false,
        default_n_ctx: DEFAULT_N_CTX,
        note: "Unsloth Dynamic, mehr RAM nötig",
    },
    CatalogRow {
        id: "qwen3.5-4b-q6k",
        label: "Qwen3.5 4B \u{b7} Q6_K",
        repo: "unsloth/Qwen3.5-4B-GGUF",
        revision: "e87f176479d0855a907a41277aca2f8ee7a09523",
        file_name: "Qwen3.5-4B-Q6_K.gguf",
        size_bytes: 3525956768,
        sha256: "fdedd781c9ce676ab66b018ca247ff78e8a33c98098a822c1e2d5075e7718f66",
        min_ram_bytes: 6 * GIB,
        recommended: false,
        default_n_ctx: DEFAULT_N_CTX,
        note: "Schneller, höhere Präzision",
    },
    CatalogRow {
        id: "qwen3.5-4b-q4km",
        label: "Qwen3.5 4B \u{b7} Q4_K_M",
        repo: "twoody/Qwen3.5-4B-GGUF",
        revision: "7e234459682a77fdae11b46591a6e6c37a45a495",
        file_name: "Qwen3.5-4B-Q4_K_M.gguf",
        size_bytes: 2_783_446_880,
        sha256: "b7508d8b4a652bc020a439aafdd4ce1527d49774865ce3334a43b16dbeaa90ca",
        min_ram_bytes: 6 * GIB,
        recommended: false,
        default_n_ctx: DEFAULT_N_CTX,
        note: "",
    },
    CatalogRow {
        id: "qwen3-4b-q4km",
        label: "Qwen3 4B (Q4_K_M)",
        repo: "Qwen/Qwen3-4B-GGUF",
        revision: "bc640142c66e1fdd12af0bd68f40445458f3869b",
        file_name: "Qwen3-4B-Q4_K_M.gguf",
        size_bytes: 2_497_280_256,
        sha256: "7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5",
        min_ram_bytes: 6 * GIB,
        recommended: false,
        default_n_ctx: DEFAULT_N_CTX,
        note: "",
    },
    CatalogRow {
        id: "qwen3-1.7b-q8",
        label: "Qwen3 1.7B (Q8_0)",
        repo: "Qwen/Qwen3-1.7B-GGUF",
        revision: "90862c4b9d2787eaed51d12237eafdfe7c5f6077",
        file_name: "Qwen3-1.7B-Q8_0.gguf",
        size_bytes: 1_834_426_016,
        sha256: "061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a",
        min_ram_bytes: 4 * GIB,
        recommended: false,
        default_n_ctx: DEFAULT_N_CTX,
        note: "",
    },
];

fn catalog_entries() -> Vec<CatalogModel> {
    CATALOG
        .iter()
        .map(|row| CatalogModel {
            id: row.id.to_owned(),
            label: row.label.to_owned(),
            repo: row.repo.to_owned(),
            revision: row.revision.to_owned(),
            file_name: row.file_name.to_owned(),
            url: format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                row.repo, row.revision, row.file_name
            ),
            size_bytes: row.size_bytes,
            sha256: row.sha256.to_owned(),
            min_ram_bytes: row.min_ram_bytes,
            recommended: row.recommended,
            default_n_ctx: row.default_n_ctx,
            note: row.note.to_owned(),
        })
        .collect()
}

// ---------------------------------------------------------------- sidecar

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SidecarOrigin {
    Catalog,
    Imported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Sidecar {
    schema: u32,
    id: String,
    #[serde(default)]
    label: String,
    origin: SidecarOrigin,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    url: Option<String>,
    file_name: String,
    size_bytes: u64,
    sha256: String,
    #[serde(default)]
    checked_at: String,
}

/// Reads a sidecar file; anything unusable counts as "no sidecar".
fn read_sidecar_file(path: &Path) -> Option<Sidecar> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(SIDECAR_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).ok()? > SIDECAR_MAX_BYTES {
        return None;
    }
    let sidecar: Sidecar = serde_json::from_slice(&bytes).ok()?;
    (sidecar.schema == SIDECAR_SCHEMA).then_some(sidecar)
}

fn sidecar_name(file_name: &str) -> String {
    format!("{file_name}{SIDECAR_SUFFIX}")
}

/// Sidecar of the final file `file_name`; its recorded name must match.
fn read_sidecar(dir: &Path, file_name: &str) -> Option<Sidecar> {
    read_sidecar_file(&dir.join(sidecar_name(file_name))).filter(|s| s.file_name == file_name)
}

/// Atomic write: `<name>.json.tmp`, sync, rename.
fn write_sidecar(dir: &Path, sidecar_file: &str, sidecar: &Sidecar) -> Result<(), MobileError> {
    let target = dir.join(sidecar_file);
    let stem = sidecar_file
        .strip_suffix(SIDECAR_SUFFIX)
        .unwrap_or(sidecar_file);
    let tmp = dir.join(format!("{stem}{SIDECAR_TMP_SUFFIX}"));
    let bytes = serde_json::to_vec_pretty(sidecar).map_err(internal)?;
    let written = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(&bytes)?;
            file.sync_all()
        });
    if let Err(error) = written {
        let _ = fs::remove_file(&tmp);
        return Err(internal(error));
    }
    fs::rename(&tmp, &target).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        internal(error)
    })
}

fn catalog_sidecar(model: &CatalogModel) -> Sidecar {
    Sidecar {
        schema: SIDECAR_SCHEMA,
        id: model.id.clone(),
        label: model.label.clone(),
        origin: SidecarOrigin::Catalog,
        verified: true,
        repo: Some(model.repo.clone()),
        revision: Some(model.revision.clone()),
        url: Some(model.url.clone()),
        file_name: model.file_name.clone(),
        size_bytes: model.size_bytes,
        sha256: model.sha256.clone(),
        checked_at: jiff::Timestamp::now().to_string(),
    }
}

// ------------------------------------------------------------------ names

fn ends_with_model_suffix(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(MODEL_SUFFIX)
}

fn strip_model_suffix(name: &str) -> &str {
    if ends_with_model_suffix(name) {
        name.get(..name.len().saturating_sub(MODEL_SUFFIX.len()))
            .unwrap_or(name)
    } else {
        name
    }
}

fn forbidden_char(c: char) -> bool {
    c == '/' || c == '\\' || c == '\0' || c.is_control()
}

/// File name rule for final model files (also applied to names from Kotlin).
fn valid_model_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_BYTES
        && !name.chars().any(forbidden_char)
        && !name.starts_with('.')
        && ends_with_model_suffix(name)
}

fn truncate_at(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// Turns a foreign display name into a name that passes `valid_model_name`.
fn sanitize_model_name(original: &str) -> String {
    let base = original.rsplit('/').next().unwrap_or_default();
    let mapped: String = base
        .chars()
        .map(|c| {
            if forbidden_char(c) || c.is_whitespace() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = mapped.trim_start_matches('.');
    if trimmed.is_empty() {
        return "model.gguf".to_owned();
    }
    let suffix = if ends_with_model_suffix(trimmed) {
        trimmed
            .get(trimmed.len().saturating_sub(MODEL_SUFFIX.len())..)
            .unwrap_or(MODEL_SUFFIX)
    } else {
        MODEL_SUFFIX
    };
    let stem = truncate_at(
        strip_model_suffix(trimmed),
        MAX_FILE_NAME_BYTES.saturating_sub(MODEL_SUFFIX.len()),
    );
    if stem.is_empty() || stem.chars().all(|c| c == '.') {
        return "model.gguf".to_owned();
    }
    format!("{stem}{suffix}")
}

fn valid_staging_name(name: &str) -> bool {
    name.strip_prefix(STAGING_PREFIX)
        .and_then(|rest| rest.strip_suffix(STAGING_SUFFIX))
        .is_some_and(|hex| {
            hex.len() == STAGING_RANDOM_HEX
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn own_id(sha256: &str) -> String {
    format!(
        "{OWN_ID_PREFIX}{}",
        sha256.get(..STAGING_RANDOM_HEX).unwrap_or(sha256)
    )
}

// ------------------------------------------------------------- fs helpers

fn internal(error: impl std::fmt::Display) -> MobileError {
    MobileError::Internal {
        detail: error.to_string(),
    }
}

fn session(detail: &str) -> MobileError {
    MobileError::Session {
        detail: detail.to_owned(),
    }
}

fn checked_dir(models_dir: &str) -> Result<PathBuf, MobileError> {
    let path = PathBuf::from(models_dir);
    if !path.is_absolute() {
        return Err(MobileError::Internal {
            detail: "models directory must be absolute".to_owned(),
        });
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .map_err(internal)?;
    Ok(path)
}

fn part_path(dir: &Path, model: &CatalogModel) -> PathBuf {
    dir.join(format!("{}{PART_SUFFIX}", model.file_name))
}

fn file_len(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn remove_if_present(path: &Path) -> Result<(), MobileError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(internal(error)),
    }
}

fn sync_dir(dir: &Path) {
    if let Ok(directory) = File::open(dir) {
        // Best effort: the rename is already visible, this only hardens it.
        let _ = directory.sync_all();
    }
}

/// State of a final file against its expected size; `None` when absent.
fn file_state(path: &Path, expected: u64) -> Option<(ModelFileState, u64)> {
    let len = file_len(path)?;
    match File::open(path) {
        Ok(_) if len == expected => Some((ModelFileState::Complete, len)),
        Ok(_) => Some((ModelFileState::Invalid, len)),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            Some((ModelFileState::Unreadable, len))
        }
        Err(_) => Some((ModelFileState::Invalid, len)),
    }
}

/// All valid sidecars next to existing model files, sorted by file name.
fn sidecars(dir: &Path) -> Vec<(String, Sidecar)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, Sidecar)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let file_name = name.strip_suffix(SIDECAR_SUFFIX)?.to_owned();
            if file_name.starts_with('.') || !ends_with_model_suffix(&file_name) {
                return None;
            }
            file_len(&dir.join(&file_name))?;
            let sidecar = read_sidecar(dir, &file_name)?;
            Some((file_name, sidecar))
        })
        .collect();
    found.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    found
}

// ------------------------------------------------------------- hash pass

struct Scan {
    len: u64,
    gguf: bool,
    sha256: String,
}

enum ScanFail {
    Cancelled,
    Io(std::io::Error),
}

impl ScanFail {
    fn into_error(self) -> MobileError {
        match self {
            Self::Cancelled => session("model check cancelled"),
            Self::Io(error) => internal(error),
        }
    }
}

/// One read pass: length, GGUF magic, SHA-256, progress.
fn scan_file(path: &Path, progress: &dyn ModelProgress) -> Result<Scan, ScanFail> {
    let mut file = File::open(path).map_err(ScanFail::Io)?;
    let total = file.metadata().map_err(ScanFail::Io)?.len();
    if !progress.on_progress(0, total) {
        return Err(ScanFail::Cancelled);
    }
    let mut context = Context::new(&SHA256);
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    let mut head: Vec<u8> = Vec::with_capacity(GGUF_MAGIC.len());
    let mut done = 0_u64;
    let mut reported = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(ScanFail::Io)?;
        if read == 0 {
            break;
        }
        let chunk = buffer.get(..read).unwrap_or_default();
        if head.len() < GGUF_MAGIC.len() {
            let need = GGUF_MAGIC.len().saturating_sub(head.len());
            head.extend_from_slice(chunk.get(..need.min(chunk.len())).unwrap_or_default());
        }
        context.update(chunk);
        done = done.saturating_add(u64::try_from(read).unwrap_or_default());
        if done.saturating_sub(reported) >= PROGRESS_STEP_BYTES {
            reported = done;
            if !progress.on_progress(done, total.max(done)) {
                return Err(ScanFail::Cancelled);
            }
        }
    }
    // The pass is finished; a cancel answer here no longer matters.
    let _ = progress.on_progress(done, done);
    Ok(Scan {
        len: done,
        gguf: head.as_slice() == GGUF_MAGIC.as_slice(),
        sha256: hex::encode(context.finish().as_ref()),
    })
}

// ------------------------------------------------------------ local state

fn catalog_local(dir: &Path, model: &CatalogModel, found: &[(String, Sidecar)]) -> LocalModel {
    let file_name = if exists(&dir.join(&model.file_name)) {
        model.file_name.clone()
    } else {
        found
            .iter()
            .find(|(_, s)| s.id == model.id && s.origin == SidecarOrigin::Catalog)
            .map_or_else(|| model.file_name.clone(), |(name, _)| name.clone())
    };
    let (state, bytes_on_disk) = match file_state(&dir.join(&file_name), model.size_bytes) {
        Some(found_state) => found_state,
        None => match file_len(&part_path(dir, model)) {
            Some(len) => (ModelFileState::Partial, len),
            None => (ModelFileState::Missing, 0),
        },
    };
    let sidecar = found
        .iter()
        .find(|(name, _)| *name == file_name)
        .map(|(_, s)| s);
    let present = matches!(state, ModelFileState::Complete | ModelFileState::Unreadable);
    let verification = match sidecar {
        Some(s)
            if present
                && s.verified
                && s.size_bytes == model.size_bytes
                && s.sha256 == model.sha256 =>
        {
            ModelVerification::Verified
        }
        _ => ModelVerification::Unchecked,
    };
    LocalModel {
        id: model.id.clone(),
        path: dir.join(&file_name).to_string_lossy().into_owned(),
        file_name,
        state,
        bytes_on_disk,
        size_bytes: model.size_bytes,
        label: model.label.clone(),
        origin: ModelOrigin::Catalog,
        verification,
        sha256: sidecar.map(|s| s.sha256.clone()),
    }
}

fn local_models_in(dir: &Path, catalog: &[CatalogModel]) -> Vec<LocalModel> {
    let found = sidecars(dir);
    let mut models: Vec<LocalModel> = catalog
        .iter()
        .map(|model| catalog_local(dir, model, &found))
        .collect();
    for (file_name, sidecar) in &found {
        if sidecar.origin != SidecarOrigin::Imported {
            continue;
        }
        let Some((state, bytes_on_disk)) = file_state(&dir.join(file_name), sidecar.size_bytes)
        else {
            continue;
        };
        models.push(LocalModel {
            id: sidecar.id.clone(),
            file_name: file_name.clone(),
            state,
            bytes_on_disk,
            size_bytes: sidecar.size_bytes,
            label: sidecar.label.clone(),
            origin: ModelOrigin::Imported,
            verification: ModelVerification::Unverified,
            sha256: Some(sidecar.sha256.clone()),
            path: dir.join(file_name).to_string_lossy().into_owned(),
        });
    }
    models
}

fn find_local(dir: &Path, catalog: &[CatalogModel], model_id: &str) -> Option<LocalModel> {
    local_models_in(dir, catalog)
        .into_iter()
        .find(|model| model.id == model_id)
}

// --------------------------------------------------------------- finalize

fn find_model(catalog: &[CatalogModel], model_id: &str) -> Result<CatalogModel, MobileError> {
    catalog
        .iter()
        .find(|model| model.id == model_id)
        .cloned()
        .ok_or_else(|| session("unknown model"))
}

fn finalize_in(
    dir: &Path,
    catalog: &[CatalogModel],
    model_id: &str,
    progress: &dyn ModelProgress,
) -> Result<LocalModel, MobileError> {
    let model = find_model(catalog, model_id)?;
    let part = part_path(dir, &model);
    if file_len(&part) != Some(model.size_bytes) {
        return Err(session("model download is incomplete"));
    }
    let scan = scan_file(&part, progress).map_err(ScanFail::into_error)?;
    if scan.len != model.size_bytes {
        return Err(session("model download is incomplete"));
    }
    if !scan.gguf || scan.sha256 != model.sha256 {
        remove_if_present(&part)?;
        return Err(session("model file failed verification"));
    }
    OpenOptions::new()
        .write(true)
        .open(&part)
        .and_then(|file| file.sync_all())
        .map_err(internal)?;
    let target = dir.join(&model.file_name);
    if exists(&target) {
        return Err(session("model name already in use"));
    }
    fs::rename(&part, &target).map_err(rename_error)?;
    write_sidecar(
        dir,
        &sidecar_name(&model.file_name),
        &catalog_sidecar(&model),
    )?;
    sync_dir(dir);
    find_local(dir, catalog, &model.id).ok_or_else(|| internal("model vanished after finalize"))
}

fn rename_error(error: std::io::Error) -> MobileError {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::AlreadyExists => {
            session("model name already in use")
        }
        _ => internal(error),
    }
}

// ----------------------------------------------------------------- import

fn staging_path(dir: &Path, staged_name: &str) -> Result<PathBuf, MobileError> {
    if valid_staging_name(staged_name) {
        Ok(dir.join(staged_name))
    } else {
        Err(session("invalid staging name"))
    }
}

fn remove_staging(dir: &Path, staged_name: &str) -> Result<(), MobileError> {
    remove_if_present(&dir.join(staged_name))?;
    remove_if_present(&dir.join(sidecar_name(staged_name)))
}

fn import_in(
    dir: &Path,
    catalog: &[CatalogModel],
    staged_name: &str,
    original_name: &str,
    progress: &dyn ModelProgress,
) -> Result<ImportCheck, MobileError> {
    let staged = staging_path(dir, staged_name)?;
    if !exists(&staged) {
        return Err(session("import file is missing"));
    }
    let scan = match scan_file(&staged, progress) {
        Ok(scan) => scan,
        Err(fail) => {
            if matches!(fail, ScanFail::Cancelled) {
                remove_staging(dir, staged_name)?;
            }
            return Err(fail.into_error());
        }
    };
    if !scan.gguf {
        remove_staging(dir, staged_name)?;
        return Err(session("not a GGUF file"));
    }
    let hit = catalog
        .iter()
        .find(|m| m.size_bytes == scan.len && m.sha256 == scan.sha256);
    let sanitized = sanitize_model_name(original_name);
    let mut check = ImportCheck {
        staged_name: staged_name.to_owned(),
        final_name: sanitized.clone(),
        model_id: own_id(&scan.sha256),
        label: strip_model_suffix(&sanitized).to_owned(),
        size_bytes: scan.len,
        sha256: scan.sha256.clone(),
        verdict: ImportVerdict::Unverified,
    };
    if let Some(model) = hit {
        check.final_name = model.file_name.clone();
        check.model_id = model.id.clone();
        check.label = model.label.clone();
        check.verdict = ImportVerdict::Verified {
            catalog_id: model.id.clone(),
        };
    } else if let Some(model) = catalog
        .iter()
        .find(|m| m.file_name.eq_ignore_ascii_case(&sanitized))
    {
        check.model_id = model.id.clone();
        check.label = model.label.clone();
        check.verdict = ImportVerdict::Mismatch {
            catalog_id: model.id.clone(),
            label: model.label.clone(),
            expected_size: model.size_bytes,
            actual_size: scan.len,
        };
        remove_staging(dir, staged_name)?;
        return Ok(check);
    }
    let present = find_local(dir, catalog, &check.model_id).is_some_and(|local| {
        matches!(
            local.state,
            ModelFileState::Complete | ModelFileState::Invalid
        ) && local.sha256.as_deref() == Some(scan.sha256.as_str())
    });
    if present {
        remove_staging(dir, staged_name)?;
        check.verdict = ImportVerdict::AlreadyPresent {
            model_id: check.model_id.clone(),
        };
        return Ok(check);
    }
    let sidecar = match hit {
        Some(model) => Sidecar {
            size_bytes: scan.len,
            sha256: scan.sha256,
            ..catalog_sidecar(model)
        },
        None => Sidecar {
            schema: SIDECAR_SCHEMA,
            id: check.model_id.clone(),
            label: check.label.clone(),
            origin: SidecarOrigin::Imported,
            verified: false,
            repo: None,
            revision: None,
            url: None,
            file_name: check.final_name.clone(),
            size_bytes: scan.len,
            sha256: scan.sha256,
            checked_at: jiff::Timestamp::now().to_string(),
        },
    };
    write_sidecar(dir, &sidecar_name(staged_name), &sidecar)?;
    Ok(check)
}

fn commit_in(
    dir: &Path,
    catalog: &[CatalogModel],
    staged_name: &str,
    final_name: &str,
) -> Result<LocalModel, MobileError> {
    let staged = staging_path(dir, staged_name)?;
    let mut sidecar = read_sidecar_file(&dir.join(sidecar_name(staged_name)))
        .ok_or_else(|| session("import was not checked"))?;
    match file_len(&staged) {
        None => return Err(session("import file is missing")),
        Some(len) if len != sidecar.size_bytes => {
            return Err(session("import file changed after check"));
        }
        Some(_) => {}
    }
    if !valid_model_name(final_name) {
        return Err(session("invalid model file name"));
    }
    let target = dir.join(final_name);
    if exists(&target) || exists(&dir.join(sidecar_name(final_name))) {
        return Err(session("model name already in use"));
    }
    fs::rename(&staged, &target).map_err(rename_error)?;
    sidecar.file_name = final_name.to_owned();
    write_sidecar(dir, &sidecar_name(final_name), &sidecar)?;
    remove_if_present(&dir.join(sidecar_name(staged_name)))?;
    sync_dir(dir);
    find_local(dir, catalog, &sidecar.id).ok_or_else(|| internal("model vanished after import"))
}

fn discard_in(dir: &Path, staged_name: &str) -> Result<(), MobileError> {
    staging_path(dir, staged_name)?;
    remove_staging(dir, staged_name)
}

// ----------------------------------------------------------------- delete

fn delete_in(dir: &Path, catalog: &[CatalogModel], model_id: &str) -> Result<(), MobileError> {
    if let Some(model) = catalog.iter().find(|m| m.id == model_id) {
        let local = catalog_local(dir, model, &sidecars(dir));
        for name in [local.file_name.as_str(), model.file_name.as_str()] {
            remove_if_present(&dir.join(name))?;
            remove_if_present(&dir.join(sidecar_name(name)))?;
        }
        return remove_if_present(&part_path(dir, model));
    }
    if model_id.starts_with(OWN_ID_PREFIX) {
        let found = sidecars(dir);
        let hit = found
            .iter()
            .find(|(_, s)| s.id == model_id && s.origin == SidecarOrigin::Imported);
        if let Some((name, _)) = hit {
            remove_if_present(&dir.join(name))?;
            return remove_if_present(&dir.join(sidecar_name(name)));
        }
    }
    Err(session("unknown model"))
}

fn model_path_in(
    dir: &Path,
    catalog: &[CatalogModel],
    model_id: &str,
) -> Result<String, MobileError> {
    find_local(dir, catalog, model_id)
        .filter(|local| local.state == ModelFileState::Complete)
        .map(|local| local.path)
        .ok_or_else(|| session("model is not available"))
}

// ------------------------------------------------------------ public API

/// The models the app offers, in display order.
#[uniffi::export]
pub fn model_catalog() -> Vec<CatalogModel> {
    catalog_entries()
}

/// Catalog models first, then imported own models, as found in `models_dir`.
#[uniffi::export]
pub fn local_models(models_dir: String) -> Result<Vec<LocalModel>, MobileError> {
    let dir = checked_dir(&models_dir)?;
    Ok(local_models_in(&dir, &catalog_entries()))
}

/// Verifies the finished `.part` download (size, GGUF magic, SHA-256) and
/// moves it to its final name, writing the sidecar.
#[uniffi::export]
pub fn finalize_model(
    models_dir: String,
    model_id: String,
    progress: Arc<dyn ModelProgress>,
) -> Result<LocalModel, MobileError> {
    let dir = checked_dir(&models_dir)?;
    finalize_in(&dir, &catalog_entries(), &model_id, progress.as_ref())
}

/// Removes the files of a catalog or own model; missing files are fine.
#[uniffi::export]
pub fn delete_model(models_dir: String, model_id: String) -> Result<(), MobileError> {
    let dir = checked_dir(&models_dir)?;
    delete_in(&dir, &catalog_entries(), &model_id)
}

/// Checks a staged import copy in one pass and writes its staging sidecar.
#[uniffi::export]
pub fn import_model(
    models_dir: String,
    staged_name: String,
    original_name: String,
    progress: Arc<dyn ModelProgress>,
) -> Result<ImportCheck, MobileError> {
    let dir = checked_dir(&models_dir)?;
    import_in(
        &dir,
        &catalog_entries(),
        &staged_name,
        &original_name,
        progress.as_ref(),
    )
}

/// Moves a checked staging file to `final_name` and moves its sidecar along.
#[uniffi::export]
pub fn commit_import(
    models_dir: String,
    staged_name: String,
    final_name: String,
) -> Result<LocalModel, MobileError> {
    let dir = checked_dir(&models_dir)?;
    commit_in(&dir, &catalog_entries(), &staged_name, &final_name)
}

/// Drops a staging file and its sidecar.
#[uniffi::export]
pub fn discard_import(models_dir: String, staged_name: String) -> Result<(), MobileError> {
    let dir = checked_dir(&models_dir)?;
    discard_in(&dir, &staged_name)
}

/// Absolute path of a complete model for the inference engine.
#[uniffi::export]
pub fn model_path(models_dir: String, model_id: String) -> Result<String, MobileError> {
    let dir = checked_dir(&models_dir)?;
    model_path_in(&dir, &catalog_entries(), &model_id)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    const STAGED: &str = ".harw-import-0123456789abcdef.gguf.part";

    struct Quiet;
    impl ModelProgress for Quiet {
        fn on_progress(&self, _done: u64, _total: u64) -> bool {
            true
        }
    }

    struct Cancel;
    impl ModelProgress for Cancel {
        fn on_progress(&self, _done: u64, _total: u64) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<(u64, u64)>>,
    }
    impl ModelProgress for Recorder {
        fn on_progress(&self, done: u64, total: u64) -> bool {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push((done, total));
            }
            true
        }
    }

    fn sha_hex(bytes: &[u8]) -> String {
        let mut context = Context::new(&SHA256);
        context.update(bytes);
        hex::encode(context.finish().as_ref())
    }

    fn fake_data() -> Vec<u8> {
        b"GGUFabc".to_vec()
    }

    fn fake_with(size: u64, sha: String) -> CatalogModel {
        CatalogModel {
            id: "fake".to_owned(),
            label: "Fake".to_owned(),
            repo: "x/y".to_owned(),
            revision: "r".to_owned(),
            file_name: "fake.gguf".to_owned(),
            url: "https://huggingface.co/x/y/resolve/r/fake.gguf".to_owned(),
            size_bytes: size,
            sha256: sha,
            min_ram_bytes: 0,
            recommended: false,
            default_n_ctx: DEFAULT_N_CTX,
            note: String::new(),
        }
    }

    fn fake() -> CatalogModel {
        let data = fake_data();
        fake_with(u64::try_from(data.len()).unwrap_or(0), sha_hex(&data))
    }

    fn fake_catalog() -> Vec<CatalogModel> {
        vec![fake()]
    }

    fn put_part(dir: &Path, model: &CatalogModel, data: &[u8]) -> std::io::Result<PathBuf> {
        let part = part_path(dir, model);
        fs::write(&part, data)?;
        Ok(part)
    }

    fn stage(dir: &Path, data: &[u8]) -> std::io::Result<()> {
        fs::write(dir.join(STAGED), data)
    }

    fn row(id: &str) -> Option<CatalogModel> {
        model_catalog().into_iter().find(|m| m.id == id)
    }

    #[test]
    fn catalog_has_pinned_qwen35_row() -> TestResult {
        let model = row("qwen3.5-4b-q4km").ok_or("row missing")?;
        assert_eq!(model.label, "Qwen3.5 4B \u{b7} Q4_K_M");
        assert_eq!(model.repo, "twoody/Qwen3.5-4B-GGUF");
        assert_eq!(model.revision, "7e234459682a77fdae11b46591a6e6c37a45a495");
        assert_eq!(model.file_name, "Qwen3.5-4B-Q4_K_M.gguf");
        assert_eq!(model.size_bytes, 2_783_446_880);
        assert_eq!(
            model.sha256,
            "b7508d8b4a652bc020a439aafdd4ce1527d49774865ce3334a43b16dbeaa90ca"
        );
        assert_eq!(
            model.url,
            "https://huggingface.co/twoody/Qwen3.5-4B-GGUF/resolve/7e234459682a77fdae11b46591a6e6c37a45a495/Qwen3.5-4B-Q4_K_M.gguf"
        );
        assert_eq!(model.default_n_ctx, 16384);
        assert_eq!(model.min_ram_bytes, 6 * GIB);
        assert!(!model.recommended);
        Ok(())
    }

    #[test]
    fn exactly_one_recommended() {
        assert_eq!(model_catalog().iter().filter(|m| m.recommended).count(), 1);
    }

    #[test]
    fn catalog_ids_and_file_names_unique() {
        let catalog = model_catalog();
        for (index, model) in catalog.iter().enumerate() {
            for other in catalog.iter().skip(index.saturating_add(1)) {
                assert_ne!(model.id, other.id);
                assert_ne!(model.file_name, other.file_name);
            }
            assert!(valid_model_name(&model.file_name));
        }
    }

    #[test]
    fn catalog_urls_pin_the_revision() {
        let catalog = model_catalog();
        assert_eq!(catalog.len(), 3);
        for model in &catalog {
            assert_eq!(
                model.url,
                format!(
                    "https://huggingface.co/{}/resolve/{}/{}",
                    model.repo, model.revision, model.file_name
                )
            );
            assert_eq!(model.revision.len(), 40);
        }
    }

    #[test]
    fn catalog_hashes_are_lowercase_hex_64() {
        for model in model_catalog() {
            assert_eq!(model.sha256.len(), 64);
            assert!(
                model
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
        }
    }

    #[test]
    fn relative_models_dir_is_rejected() {
        assert!(matches!(
            local_models("models".to_owned()),
            Err(MobileError::Internal { ref detail }) if detail == "models directory must be absolute"
        ));
    }

    #[test]
    fn unknown_model_is_a_session_error() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().to_string_lossy().into_owned();
        assert!(matches!(
            finalize_model(dir.clone(), "nope".to_owned(), Arc::new(Quiet)),
            Err(MobileError::Session { ref detail }) if detail == "unknown model"
        ));
        assert!(matches!(
            delete_model(dir, "nope".to_owned()),
            Err(MobileError::Session { ref detail }) if detail == "unknown model"
        ));
        Ok(())
    }

    #[test]
    fn local_models_reports_missing_partial_complete_invalid() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let catalog = fake_catalog();
        let model = fake();
        let state = || {
            local_models_in(dir, &catalog)
                .into_iter()
                .next()
                .map(|m| (m.state, m.bytes_on_disk, m.verification))
        };
        assert_eq!(state().map(|s| s.0), Some(ModelFileState::Missing));
        put_part(dir, &model, b"ab")?;
        assert_eq!(
            state().map(|s| (s.0, s.1)),
            Some((ModelFileState::Partial, 2))
        );
        fs::write(dir.join(&model.file_name), b"abcdefgh")?;
        assert_eq!(state().map(|s| s.0), Some(ModelFileState::Invalid));
        fs::write(dir.join(&model.file_name), fake_data())?;
        assert_eq!(
            state().map(|s| (s.0, s.2)),
            Some((ModelFileState::Complete, ModelVerification::Unchecked))
        );
        Ok(())
    }

    #[test]
    fn finalize_writes_sidecar_and_reports_verified() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let model = fake();
        put_part(dir, &model, &fake_data())?;
        let done = finalize_in(dir, &fake_catalog(), "fake", &Quiet)?;
        assert_eq!(done.state, ModelFileState::Complete);
        assert_eq!(done.verification, ModelVerification::Verified);
        assert_eq!(done.origin, ModelOrigin::Catalog);
        assert_eq!(done.sha256.as_deref(), Some(model.sha256.as_str()));
        assert!(!part_path(dir, &model).exists());
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("fake.gguf.json"))?)?;
        assert_eq!(json["schema"], 1);
        assert_eq!(json["origin"], "catalog");
        assert_eq!(json["verified"], true);
        assert_eq!(json["id"], "fake");
        assert_eq!(json["size_bytes"], model.size_bytes);
        assert_eq!(json["sha256"], model.sha256.as_str());
        assert_eq!(json["file_name"], "fake.gguf");
        Ok(())
    }

    #[test]
    fn finalize_progress_is_monotonic_and_ends_at_total() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let total = 40 * 1024 * 1024_u64;
        let probe = dir.join("probe");
        {
            let mut file = File::create(&probe)?;
            file.write_all(GGUF_MAGIC)?;
            file.set_len(total)?;
        }
        let sha = sha256_of(&probe)?;
        let mut model = fake_with(total, sha);
        model.file_name = "big.gguf".to_owned();
        fs::rename(&probe, part_path(dir, &model))?;
        let recorder = Recorder::default();
        finalize_in(dir, &[model], "fake", &recorder)?;
        let calls = recorder.calls.lock().map_err(|_| "poisoned")?.clone();
        assert_eq!(calls.first(), Some(&(0, total)));
        assert_eq!(calls.last(), Some(&(total, total)));
        assert!(calls.len() >= 3);
        assert!(calls.windows(2).all(|w| matches!(w, [a, b] if a.0 <= b.0)));
        Ok(())
    }

    fn sha256_of(path: &Path) -> std::io::Result<String> {
        let mut file = File::open(path)?;
        let mut context = Context::new(&SHA256);
        let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            context.update(buffer.get(..read).unwrap_or_default());
        }
        Ok(hex::encode(context.finish().as_ref()))
    }

    #[test]
    fn finalize_cancel_keeps_part() -> TestResult {
        let temp = tempfile::tempdir()?;
        let model = fake();
        let part = put_part(temp.path(), &model, &fake_data())?;
        let result = finalize_in(temp.path(), &fake_catalog(), "fake", &Cancel);
        assert!(matches!(
            result,
            Err(MobileError::Session { ref detail }) if detail == "model check cancelled"
        ));
        assert!(part.exists());
        Ok(())
    }

    #[test]
    fn finalize_rejects_wrong_hash_and_removes_part() -> TestResult {
        let temp = tempfile::tempdir()?;
        let model = fake();
        let part = put_part(temp.path(), &model, b"GGUFabd")?;
        let result = finalize_in(temp.path(), &fake_catalog(), "fake", &Quiet);
        assert!(matches!(
            result,
            Err(MobileError::Session { ref detail }) if detail == "model file failed verification"
        ));
        assert!(!part.exists());
        Ok(())
    }

    #[test]
    fn finalize_rejects_non_gguf_magic_and_removes_part() -> TestResult {
        let temp = tempfile::tempdir()?;
        let data = b"XXXXabc";
        let model = fake_with(7, sha_hex(data));
        let part = put_part(temp.path(), &model, data)?;
        let result = finalize_in(temp.path(), &[model], "fake", &Quiet);
        assert!(matches!(
            result,
            Err(MobileError::Session { ref detail }) if detail == "model file failed verification"
        ));
        assert!(!part.exists());
        Ok(())
    }

    #[test]
    fn finalize_incomplete_keeps_part() -> TestResult {
        let temp = tempfile::tempdir()?;
        let model = fake();
        let missing = finalize_in(temp.path(), &fake_catalog(), "fake", &Quiet);
        assert!(matches!(
            missing,
            Err(MobileError::Session { ref detail }) if detail == "model download is incomplete"
        ));
        let part = put_part(temp.path(), &model, b"GG")?;
        let short = finalize_in(temp.path(), &fake_catalog(), "fake", &Quiet);
        assert!(matches!(
            short,
            Err(MobileError::Session { ref detail }) if detail == "model download is incomplete"
        ));
        assert!(part.exists());
        Ok(())
    }

    #[test]
    fn finalize_refuses_existing_target() -> TestResult {
        let temp = tempfile::tempdir()?;
        let model = fake();
        let part = put_part(temp.path(), &model, &fake_data())?;
        fs::write(temp.path().join(&model.file_name), b"other")?;
        let result = finalize_in(temp.path(), &fake_catalog(), "fake", &Quiet);
        assert!(matches!(
            result,
            Err(MobileError::Session { ref detail }) if detail == "model name already in use"
        ));
        assert!(part.exists());
        Ok(())
    }

    #[test]
    fn import_verified_uses_catalog_name() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        stage(dir, &fake_data())?;
        let check = import_in(dir, &fake_catalog(), STAGED, "whatever.gguf", &Quiet)?;
        assert_eq!(
            check.verdict,
            ImportVerdict::Verified {
                catalog_id: "fake".to_owned()
            }
        );
        assert_eq!(check.final_name, "fake.gguf");
        assert_eq!(check.model_id, "fake");
        assert!(dir.join(format!("{STAGED}.json")).exists());
        Ok(())
    }

    #[test]
    fn import_unverified_gets_own_id_and_sanitized_name() -> TestResult {
        let temp = tempfile::tempdir()?;
        stage(temp.path(), b"GGUFzzzz")?;
        let check = import_in(temp.path(), &fake_catalog(), STAGED, "../My Model", &Quiet)?;
        assert_eq!(check.verdict, ImportVerdict::Unverified);
        assert_eq!(check.final_name, "My_Model.gguf");
        assert_eq!(check.label, "My_Model");
        assert!(check.model_id.starts_with("own-"));
        assert_eq!(check.model_id.len(), 4 + 16);
        Ok(())
    }

    #[test]
    fn import_name_clash_with_catalog_is_mismatch() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        stage(dir, b"GGUFlonger")?;
        let check = import_in(dir, &fake_catalog(), STAGED, "FAKE.gguf", &Quiet)?;
        assert!(matches!(
            check.verdict,
            ImportVerdict::Mismatch {
                expected_size: 7,
                actual_size: 10,
                ..
            }
        ));
        assert!(!dir.join(STAGED).exists());
        assert!(!dir.join(format!("{STAGED}.json")).exists());
        Ok(())
    }

    #[test]
    fn import_not_gguf_is_rejected_and_staging_removed() -> TestResult {
        let temp = tempfile::tempdir()?;
        stage(temp.path(), b"nope-not-gguf")?;
        let result = import_in(temp.path(), &fake_catalog(), STAGED, "a.gguf", &Quiet);
        assert!(matches!(
            result,
            Err(MobileError::Session { ref detail }) if detail == "not a GGUF file"
        ));
        assert!(!temp.path().join(STAGED).exists());
        Ok(())
    }

    #[test]
    fn import_cancel_removes_staging() -> TestResult {
        let temp = tempfile::tempdir()?;
        stage(temp.path(), &fake_data())?;
        let result = import_in(temp.path(), &fake_catalog(), STAGED, "a.gguf", &Cancel);
        assert!(matches!(
            result,
            Err(MobileError::Session { ref detail }) if detail == "model check cancelled"
        ));
        assert!(!temp.path().join(STAGED).exists());
        Ok(())
    }

    #[test]
    fn import_missing_staging_file_is_reported() -> TestResult {
        let temp = tempfile::tempdir()?;
        let result = import_in(temp.path(), &fake_catalog(), STAGED, "a.gguf", &Quiet);
        assert!(matches!(
            result,
            Err(MobileError::Session { ref detail }) if detail == "import file is missing"
        ));
        Ok(())
    }

    #[test]
    fn import_already_present_removes_staging() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        put_part(dir, &fake(), &fake_data())?;
        finalize_in(dir, &fake_catalog(), "fake", &Quiet)?;
        stage(dir, &fake_data())?;
        let check = import_in(dir, &fake_catalog(), STAGED, "x.gguf", &Quiet)?;
        assert_eq!(
            check.verdict,
            ImportVerdict::AlreadyPresent {
                model_id: "fake".to_owned()
            }
        );
        assert!(!dir.join(STAGED).exists());
        Ok(())
    }

    #[test]
    fn import_rejects_bad_staging_names() -> TestResult {
        let temp = tempfile::tempdir()?;
        for bad in [
            "x.gguf",
            ".harw-import-zz.gguf.part",
            "../.harw-import-0123456789abcdef.gguf.part",
        ] {
            assert!(matches!(
                import_in(temp.path(), &fake_catalog(), bad, "a.gguf", &Quiet),
                Err(MobileError::Session { ref detail }) if detail == "invalid staging name"
            ));
            assert!(matches!(
                discard_in(temp.path(), bad),
                Err(MobileError::Session { ref detail }) if detail == "invalid staging name"
            ));
        }
        Ok(())
    }

    #[test]
    fn commit_import_renames_and_moves_sidecar() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        stage(dir, b"GGUFzzzz")?;
        let check = import_in(dir, &fake_catalog(), STAGED, "mine.gguf", &Quiet)?;
        let local = commit_in(dir, &fake_catalog(), STAGED, &check.final_name)?;
        assert_eq!(local.id, check.model_id);
        assert_eq!(local.origin, ModelOrigin::Imported);
        assert_eq!(local.verification, ModelVerification::Unverified);
        assert_eq!(local.state, ModelFileState::Complete);
        assert!(dir.join("mine.gguf").exists());
        assert!(dir.join("mine.gguf.json").exists());
        assert!(!dir.join(STAGED).exists());
        assert!(!dir.join(format!("{STAGED}.json")).exists());
        Ok(())
    }

    #[test]
    fn commit_import_refuses_existing_name() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        stage(dir, b"GGUFzzzz")?;
        import_in(dir, &fake_catalog(), STAGED, "mine.gguf", &Quiet)?;
        fs::write(dir.join("mine.gguf"), b"x")?;
        let result = commit_in(dir, &fake_catalog(), STAGED, "mine.gguf");
        assert!(matches!(
            result,
            Err(MobileError::Session { ref detail }) if detail == "model name already in use"
        ));
        assert!(dir.join(STAGED).exists());
        Ok(())
    }

    #[test]
    fn commit_import_with_fallback_name_keeps_catalog_id() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        stage(dir, &fake_data())?;
        import_in(dir, &fake_catalog(), STAGED, "a.gguf", &Quiet)?;
        let local = commit_in(dir, &fake_catalog(), STAGED, "fake-2.gguf")?;
        assert_eq!(local.id, "fake");
        let listed = local_models_in(dir, &fake_catalog());
        let first = listed.first().ok_or("empty")?;
        assert_eq!(first.state, ModelFileState::Complete);
        assert_eq!(first.file_name, "fake-2.gguf");
        assert_eq!(first.verification, ModelVerification::Verified);
        assert_eq!(listed.len(), 1);
        Ok(())
    }

    #[test]
    fn commit_import_requires_check_and_unchanged_file() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        stage(dir, b"GGUFzzzz")?;
        assert!(matches!(
            commit_in(dir, &fake_catalog(), STAGED, "a.gguf"),
            Err(MobileError::Session { ref detail }) if detail == "import was not checked"
        ));
        import_in(dir, &fake_catalog(), STAGED, "a.gguf", &Quiet)?;
        stage(dir, b"GGUFzzzzzz")?;
        assert!(matches!(
            commit_in(dir, &fake_catalog(), STAGED, "a.gguf"),
            Err(MobileError::Session { ref detail }) if detail == "import file changed after check"
        ));
        assert!(dir.join(STAGED).exists());
        Ok(())
    }

    #[test]
    fn commit_import_rejects_invalid_names() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        stage(dir, b"GGUFzzzz")?;
        import_in(dir, &fake_catalog(), STAGED, "a.gguf", &Quiet)?;
        let long = format!("{}.gguf", "a".repeat(196));
        assert_eq!(long.len(), 201);
        for bad in ["", ".hidden.gguf", "a/b.gguf", "x.bin", long.as_str()] {
            assert!(matches!(
                commit_in(dir, &fake_catalog(), STAGED, bad),
                Err(MobileError::Session { ref detail }) if detail == "invalid model file name"
            ));
        }
        assert!(dir.join(STAGED).exists());
        Ok(())
    }

    #[test]
    fn sanitize_always_yields_a_valid_name() {
        for original in [
            "",
            ".",
            "..",
            "a/",
            "x y.GGUF",
            "\u{7}\u{7}",
            "\u{e4}".repeat(300).as_str(),
        ] {
            let name = sanitize_model_name(original);
            assert!(valid_model_name(&name), "{original:?} -> {name:?}");
        }
        assert_eq!(sanitize_model_name("../My Model"), "My_Model.gguf");
    }

    #[test]
    fn local_models_lists_imported_after_catalog_and_ignores_staging() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        for (staged, original) in [
            (".harw-import-aaaaaaaaaaaaaaaa.gguf.part", "zeta.gguf"),
            (".harw-import-bbbbbbbbbbbbbbbb.gguf.part", "alpha.gguf"),
        ] {
            fs::write(dir.join(staged), format!("GGUF{original}"))?;
            let check = import_in(dir, &fake_catalog(), staged, original, &Quiet)?;
            commit_in(dir, &fake_catalog(), staged, &check.final_name)?;
        }
        stage(dir, b"GGUFpending")?;
        fs::write(dir.join("loose.gguf"), b"GGUF")?;
        fs::write(dir.join("fake.gguf.part"), b"GG")?;
        let listed = local_models_in(dir, &fake_catalog());
        let names: Vec<&str> = listed.iter().map(|m| m.file_name.as_str()).collect();
        assert_eq!(names, ["fake.gguf", "alpha.gguf", "zeta.gguf"]);
        assert_eq!(
            listed.first().map(|m| m.state),
            Some(ModelFileState::Partial)
        );
        Ok(())
    }

    #[test]
    fn broken_or_future_sidecar_is_ignored() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        fs::write(dir.join("fake.gguf"), fake_data())?;
        let sidecar = dir.join("fake.gguf.json");
        let valid = catalog_sidecar(&fake());
        let mut future = serde_json::to_value(&valid)?;
        future["schema"] = serde_json::json!(2);
        let huge = vec![b' '; 70_000];
        for bytes in [b"garbage".to_vec(), serde_json::to_vec(&future)?, huge] {
            fs::write(&sidecar, bytes)?;
            let first = local_models_in(dir, &fake_catalog())
                .into_iter()
                .next()
                .ok_or("empty")?;
            assert_eq!(first.state, ModelFileState::Complete);
            assert_eq!(first.verification, ModelVerification::Unchecked);
            assert_eq!(first.sha256, None);
        }
        Ok(())
    }

    #[test]
    fn delete_own_model_removes_file_and_sidecar() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        stage(dir, b"GGUFzzzz")?;
        let check = import_in(dir, &fake_catalog(), STAGED, "mine.gguf", &Quiet)?;
        commit_in(dir, &fake_catalog(), STAGED, &check.final_name)?;
        delete_in(dir, &fake_catalog(), &check.model_id)?;
        assert!(!dir.join("mine.gguf").exists());
        assert!(!dir.join("mine.gguf.json").exists());
        assert!(matches!(
            delete_in(dir, &fake_catalog(), &check.model_id),
            Err(MobileError::Session { ref detail }) if detail == "unknown model"
        ));
        Ok(())
    }

    #[test]
    fn delete_catalog_model_removes_file_sidecar_and_part() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        put_part(dir, &fake(), &fake_data())?;
        finalize_in(dir, &fake_catalog(), "fake", &Quiet)?;
        fs::write(dir.join("fake.gguf.part"), b"GG")?;
        delete_in(dir, &fake_catalog(), "fake")?;
        delete_in(dir, &fake_catalog(), "fake")?;
        for name in ["fake.gguf", "fake.gguf.json", "fake.gguf.part"] {
            assert!(!dir.join(name).exists());
        }
        Ok(())
    }

    #[test]
    fn model_path_needs_a_complete_model() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        assert!(matches!(
            model_path_in(dir, &fake_catalog(), "fake"),
            Err(MobileError::Session { ref detail }) if detail == "model is not available"
        ));
        put_part(dir, &fake(), &fake_data())?;
        finalize_in(dir, &fake_catalog(), "fake", &Quiet)?;
        assert_eq!(
            model_path_in(dir, &fake_catalog(), "fake")?,
            dir.join("fake.gguf").to_string_lossy()
        );
        Ok(())
    }

    #[test]
    fn unreadable_file_reports_unreadable() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let path = dir.join("fake.gguf");
        fs::write(&path, fake_data())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000))?;
        if File::open(&path).is_ok() {
            // Running as root: permissions are not enforced, device-only check.
            return Ok(());
        }
        let first = local_models_in(dir, &fake_catalog())
            .into_iter()
            .next()
            .ok_or("empty")?;
        assert_eq!(first.state, ModelFileState::Unreadable);
        Ok(())
    }
}
