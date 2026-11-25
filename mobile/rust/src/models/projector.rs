//! Vision projectors (`mmproj` GGUF files) for any local model.
//!
//! A projector belongs to a model through the mapping file
//! `.harw-projectors.json` in the models directory (`model id -> file name`).
//! The GGUF headers of both files are read to check that the projector fits
//! the language model (architecture family and embedding size).

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::{
    GGUF_MAGIC, LocalModel, MODEL_SUFFIX, ModelFileState, VisionAddon, catalog_entries, checked_dir,
    ends_with_model_suffix, exists, file_len, find_local, internal, remove_if_present, remove_staging,
    rename_error, sanitize_model_name, sidecar_name, staging_path, strip_model_suffix, sync_dir,
    valid_model_name, vision_entries, vision_path_in,
};
use crate::MobileError;

const MAP_FILE: &str = ".harw-projectors.json";
const MAP_TMP_FILE: &str = ".harw-projectors.json.tmp";
const MAP_MAX_BYTES: u64 = 1024 * 1024;
const MAX_KV: u64 = 200_000;
const MAX_KEY_BYTES: u64 = 4096;
const MAX_STRING_BYTES: u64 = 1024 * 1024;

/// Facts from a GGUF header that matter for pairing a model with a projector.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GgufInfo {
    pub architecture: Option<String>,
    /// Text embedding size (`<arch>.embedding_length`), models only.
    pub embedding: Option<u64>,
    /// `clip.projector_type`, projectors only.
    pub projector_type: Option<String>,
    /// `clip.vision.projection_dim`: the text embedding size the projector feeds.
    pub projection_dim: Option<u64>,
}

impl GgufInfo {
    fn is_projector(&self) -> bool {
        self.architecture.as_deref() == Some("clip")
    }

    fn complete(&self) -> bool {
        match self.architecture.as_deref() {
            None => false,
            Some("clip") => self.projector_type.is_some() && self.projection_dim.is_some(),
            Some(_) => self.embedding.is_some(),
        }
    }
}

fn read_exact<const N: usize>(r: &mut impl Read) -> std::io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    Ok(u32::from_le_bytes(read_exact::<4>(r)?))
}

fn read_u64(r: &mut impl Read) -> std::io::Result<u64> {
    Ok(u64::from_le_bytes(read_exact::<8>(r)?))
}

fn bad(text: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, text.to_owned())
}

fn read_string(r: &mut impl Read, max: u64) -> std::io::Result<String> {
    let len = read_u64(r)?;
    if len > max {
        return Err(bad("string too long"));
    }
    let mut bytes = Vec::new();
    r.take(len).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).ok() != Some(len) {
        return Err(bad("truncated string"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn fixed_size(kind: u32) -> Option<u64> {
    match kind {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

fn skip_bytes(r: &mut (impl Read + Seek), count: u64) -> std::io::Result<()> {
    let count = i64::try_from(count).map_err(|_| bad("value too large"))?;
    r.seek(SeekFrom::Current(count)).map(|_| ())
}

fn skip_value(r: &mut (impl Read + Seek), kind: u32) -> std::io::Result<()> {
    if let Some(size) = fixed_size(kind) {
        return skip_bytes(r, size);
    }
    match kind {
        8 => {
            let len = read_u64(r)?;
            skip_bytes(r, len)
        }
        9 => {
            let elem = read_u32(r)?;
            let len = read_u64(r)?;
            if let Some(size) = fixed_size(elem) {
                return skip_bytes(r, size.checked_mul(len).ok_or_else(|| bad("array too large"))?);
            }
            for _ in 0..len {
                skip_value(r, elem)?;
            }
            Ok(())
        }
        _ => Err(bad("unknown value type")),
    }
}

fn read_uint(r: &mut impl Read, kind: u32) -> std::io::Result<Option<u64>> {
    Ok(match kind {
        0 => Some(u64::from(read_exact::<1>(r)?[0])),
        2 => Some(u64::from(u16::from_le_bytes(read_exact::<2>(r)?))),
        4 => Some(u64::from(read_u32(r)?)),
        10 => Some(read_u64(r)?),
        5 => u64::try_from(i32::from_le_bytes(read_exact::<4>(r)?)).ok(),
        11 => u64::try_from(i64::from_le_bytes(read_exact::<8>(r)?)).ok(),
        _ => None,
    })
}

/// Reads the metadata keys needed for pairing; stops as soon as they are known.
pub(crate) fn read_gguf_info(path: &Path) -> std::io::Result<GgufInfo> {
    let mut r = BufReader::new(File::open(path)?);
    if &read_exact::<4>(&mut r)? != GGUF_MAGIC {
        return Err(bad("not a GGUF file"));
    }
    let version = read_u32(&mut r)?;
    if !(2..=3).contains(&version) {
        return Err(bad("unsupported GGUF version"));
    }
    let _tensors = read_u64(&mut r)?;
    let kv_count = read_u64(&mut r)?.min(MAX_KV);
    let mut info = GgufInfo::default();
    for _ in 0..kv_count {
        let key = read_string(&mut r, MAX_KEY_BYTES)?;
        let kind = read_u32(&mut r)?;
        let embedding_key = info
            .architecture
            .as_ref()
            .map(|arch| format!("{arch}.embedding_length"));
        match key.as_str() {
            "general.architecture" if kind == 8 => {
                info.architecture = Some(read_string(&mut r, MAX_STRING_BYTES)?);
            }
            "clip.projector_type" if kind == 8 => {
                info.projector_type = Some(read_string(&mut r, MAX_STRING_BYTES)?);
            }
            "clip.vision.projection_dim" => info.projection_dim = read_uint(&mut r, kind)?,
            other if embedding_key.as_deref() == Some(other) => {
                info.embedding = read_uint(&mut r, kind)?;
            }
            _ => skip_value(&mut r, kind)?,
        }
        if info.complete() {
            break;
        }
    }
    Ok(info)
}

/// `Ok` when `projector` can serve `model`, else a German explanation.
pub(crate) fn compatibility(model: &GgufInfo, projector: &GgufInfo) -> Result<(), String> {
    if !projector.is_projector() {
        return Err(
            "Das ist keine Projektor-Datei (mmproj), sondern ein normales Modell. Bitte eine Datei wie \u{201e}mmproj-F16.gguf\u{201c} w\u{e4}hlen."
                .to_owned(),
        );
    }
    let arch = model.architecture.clone().unwrap_or_default();
    if arch.is_empty() || arch == "clip" {
        return Err("Die Architektur des Modells ist nicht lesbar.".to_owned());
    }
    if arch.starts_with("qwen")
        && projector
            .projector_type
            .as_deref()
            .is_some_and(|t| !t.starts_with("qwen"))
    {
        return Err(format!(
            "Der Projektor ist f\u{fc}r eine andere Modellfamilie gebaut (Typ \u{201e}{}\u{201c}); das Modell ist ein Qwen-Modell.",
            projector.projector_type.as_deref().unwrap_or_default()
        ));
    }
    match (model.embedding, projector.projection_dim) {
        (Some(have), Some(want)) if have == want => Ok(()),
        (Some(have), Some(want)) => Err(format!(
            "Der Projektor passt nicht zum Modell: Er erwartet die Einbettungsgr\u{f6}\u{df}e {want}, das Modell hat {have}. Gemeint ist meist eine andere Modellgr\u{f6}\u{df}e (z. B. 4B statt 9B)."
        )),
        _ => Err(
            "Die Gr\u{f6}\u{df}e des Projektors oder des Modells ist im Dateikopf nicht lesbar; Kompatibilit\u{e4}t nicht pr\u{fc}fbar."
                .to_owned(),
        ),
    }
}

// ------------------------------------------------------------ mapping

fn read_map(dir: &Path) -> BTreeMap<String, String> {
    let Ok(file) = File::open(dir.join(MAP_FILE)) else {
        return BTreeMap::new();
    };
    let mut bytes = Vec::new();
    if file
        .take(MAP_MAX_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return BTreeMap::new();
    }
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn write_map(dir: &Path, map: &BTreeMap<String, String>) -> Result<(), MobileError> {
    let tmp = dir.join(MAP_TMP_FILE);
    let bytes = serde_json::to_vec_pretty(map).map_err(internal)?;
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
    fs::rename(&tmp, dir.join(MAP_FILE)).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        internal(error)
    })
}

/// Complete mapped projector file of `model_id`, if mapped and on disk.
pub(crate) fn mapped_projector_in(dir: &Path, model_id: &str) -> Option<PathBuf> {
    let name = read_map(dir).remove(model_id)?;
    if !valid_model_name(&name) {
        return None;
    }
    let path = dir.join(name);
    file_len(&path)?;
    File::open(&path).ok()?;
    Some(path)
}

/// Drops the mapping of a deleted model (and its projector file when unused).
pub(crate) fn forget_in(dir: &Path, model_id: &str) -> Result<(), MobileError> {
    let mut map = read_map(dir);
    let Some(name) = map.remove(model_id) else {
        return Ok(());
    };
    write_map(dir, &map)?;
    remove_unused_file(dir, &map, &name)
}

fn is_catalog_file(name: &str) -> bool {
    vision_entries().iter().any(|a| a.file.file_name == name)
}

fn remove_unused_file(
    dir: &Path,
    map: &BTreeMap<String, String>,
    name: &str,
) -> Result<(), MobileError> {
    if map.values().any(|other| other == name) || !valid_model_name(name) {
        return Ok(());
    }
    if is_catalog_file(name) {
        // Keep a downloaded add-on while a catalog model of it is on the phone.
        let in_use = vision_entries()
            .into_iter()
            .filter(|a| a.file.file_name == name)
            .flat_map(|a| a.model_ids)
            .any(|id| {
                find_local(dir, &catalog_entries(), &id)
                    .is_some_and(|m| m.state == ModelFileState::Complete)
            });
        if in_use {
            return Ok(());
        }
    }
    remove_if_present(&dir.join(name))?;
    remove_if_present(&dir.join(sidecar_name(name)))
}

// ------------------------------------------------------------ public API

/// A projector file on the phone.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ProjectorInfo {
    pub file_name: String,
    pub path: String,
    pub size_bytes: u64,
    /// Header check against the model passed to the call.
    pub compatible: bool,
    /// German reason when `compatible` is false; empty otherwise.
    pub detail: String,
}

/// Result of attaching a projector: `info` is set when it worked.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ProjectorAttach {
    pub info: Option<ProjectorInfo>,
    /// German message for the owner, empty on success.
    pub message: String,
}

fn failure(message: impl Into<String>) -> ProjectorAttach {
    ProjectorAttach {
        info: None,
        message: message.into(),
    }
}

fn local_model_info(dir: &Path, model_id: &str) -> Result<(LocalModel, GgufInfo), String> {
    let local = find_local(dir, &catalog_entries(), model_id)
        .filter(|m| m.state == ModelFileState::Complete)
        .ok_or_else(|| "Das Modell ist nicht vollst\u{e4}ndig auf dem Ger\u{e4}t.".to_owned())?;
    let info = read_gguf_info(Path::new(&local.path))
        .map_err(|_| "Der Dateikopf des Modells ist nicht lesbar.".to_owned())?;
    Ok((local, info))
}

fn projector_info(dir: &Path, name: &str, model: &GgufInfo) -> Option<ProjectorInfo> {
    let path = dir.join(name);
    let size = file_len(&path)?;
    let verdict = read_gguf_info(&path)
        .map_err(|_| "Die Datei ist keine lesbare GGUF-Datei.".to_owned())
        .and_then(|info| compatibility(model, &info).map(|()| info));
    Some(ProjectorInfo {
        file_name: name.to_owned(),
        path: path.to_string_lossy().into_owned(),
        size_bytes: size,
        compatible: verdict.is_ok(),
        detail: verdict.err().unwrap_or_default(),
    })
}

/// Projector in use for `model_id`: the mapped one, else (catalog models) the
/// catalog add-on. `None` when the model reads text only.
#[uniffi::export]
pub fn model_projector(models_dir: String, model_id: String) -> Result<Option<ProjectorInfo>, MobileError> {
    let dir = checked_dir(&models_dir)?;
    let Some(path) = effective_projector_in(&dir, &model_id) else {
        return Ok(None);
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let model = local_model_info(&dir, &model_id)
        .map(|(_, info)| info)
        .unwrap_or_default();
    Ok(projector_info(&dir, &name, &model))
}

/// Path of the projector the engine should load for `model_id`.
pub(crate) fn effective_projector_in(dir: &Path, model_id: &str) -> Option<PathBuf> {
    vision_path_in(dir, model_id).map(PathBuf::from)
}

/// Compatible projector files already in the models folder, best name match first.
#[uniffi::export]
pub fn suggest_projectors(models_dir: String, model_id: String) -> Result<Vec<ProjectorInfo>, MobileError> {
    let dir = checked_dir(&models_dir)?;
    let Ok((local, model)) = local_model_info(&dir, &model_id) else {
        return Ok(Vec::new());
    };
    let stem = strip_model_suffix(&local.file_name).to_lowercase();
    let mapped = read_map(&dir).remove(&model_id);
    let mut found: Vec<(bool, ProjectorInfo)> = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| {
                    let lower = name.to_lowercase();
                    !name.starts_with('.')
                        && ends_with_model_suffix(name)
                        && lower.contains("mmproj")
                        && *name != local.file_name
                        && mapped.as_deref() != Some(name.as_str())
                })
                .filter_map(|name| {
                    let info = projector_info(&dir, &name, &model)?;
                    let related = name.to_lowercase().starts_with(&stem);
                    info.compatible.then_some((related, info))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.file_name.as_bytes().cmp(b.1.file_name.as_bytes()))
    });
    Ok(found.into_iter().map(|(_, info)| info).collect())
}

/// Maps an existing file of the models folder to the model after the header check.
#[uniffi::export]
pub fn attach_projector(
    models_dir: String,
    model_id: String,
    file_name: String,
) -> Result<ProjectorAttach, MobileError> {
    let dir = checked_dir(&models_dir)?;
    Ok(attach_existing(&dir, &model_id, &file_name))
}

fn attach_existing(dir: &Path, model_id: &str, file_name: &str) -> ProjectorAttach {
    if !valid_model_name(file_name) {
        return failure("Ung\u{fc}ltiger Dateiname.");
    }
    let (_, model) = match local_model_info(dir, model_id) {
        Ok(found) => found,
        Err(message) => return failure(message),
    };
    let Some(info) = projector_info(dir, file_name, &model) else {
        return failure("Die Projektor-Datei fehlt.");
    };
    if !info.compatible {
        return failure(info.detail);
    }
    let mut map = read_map(dir);
    let previous = map.insert(model_id.to_owned(), file_name.to_owned());
    if let Err(error) = write_map(dir, &map) {
        return failure(format!("Zuordnung nicht gespeichert: {error}"));
    }
    if let Some(old) = previous.filter(|old| old != file_name) {
        let _ = remove_unused_file(dir, &map, &old);
    }
    ProjectorAttach {
        info: Some(info),
        message: String::new(),
    }
}

/// Takes a staged copy (`.harw-import-<hex>.gguf.part`) of a picked projector,
/// checks it against the model, moves it to a final name and maps it.
#[uniffi::export]
pub fn attach_projector_import(
    models_dir: String,
    model_id: String,
    staged_name: String,
    original_name: String,
) -> Result<ProjectorAttach, MobileError> {
    let dir = checked_dir(&models_dir)?;
    let staged = staging_path(&dir, &staged_name)?;
    let result = stage_check(&dir, &model_id, &staged, &original_name);
    match result {
        Ok(final_name) => {
            let target = dir.join(&final_name);
            fs::rename(&staged, &target).map_err(rename_error)?;
            sync_dir(&dir);
            let attached = attach_existing(&dir, &model_id, &final_name);
            if attached.info.is_none() {
                remove_if_present(&target)?;
            }
            Ok(attached)
        }
        Err(message) => {
            remove_staging(&dir, &staged_name)?;
            Ok(failure(message))
        }
    }
}

/// Validates the staged file; returns the free final name or a German error.
fn stage_check(dir: &Path, model_id: &str, staged: &Path, original: &str) -> Result<String, String> {
    let (_, model) = local_model_info(dir, model_id)?;
    let info = read_gguf_info(staged).map_err(|_| {
        "Die gew\u{e4}hlte Datei ist keine g\u{fc}ltige GGUF-Datei.".to_owned()
    })?;
    compatibility(&model, &info)?;
    let base = sanitize_model_name(original);
    let stem = strip_model_suffix(&base).to_owned();
    let mut name = base;
    let mut n = 1u32;
    while exists(&dir.join(&name)) {
        // The same name on disk is only reused when it is this exact file content size.
        n += 1;
        if n > 99 {
            return Err("Kein freier Dateiname im Modellordner.".to_owned());
        }
        name = format!("{stem}-{n}{MODEL_SUFFIX}");
    }
    Ok(name)
}

/// Removes the mapping of `model_id` (and the file when nothing else uses it).
#[uniffi::export]
pub fn detach_projector(models_dir: String, model_id: String) -> Result<(), MobileError> {
    let dir = checked_dir(&models_dir)?;
    forget_in(&dir, &model_id)
}

/// Catalog add-on that fits the model's family and size class, if any.
#[uniffi::export]
pub fn default_projector_for(models_dir: String, model_id: String) -> Result<Option<VisionAddon>, MobileError> {
    let dir = checked_dir(&models_dir)?;
    let Ok((local, info)) = local_model_info(&dir, &model_id) else {
        return Ok(None);
    };
    if !info
        .architecture
        .as_deref()
        .is_some_and(|a| a.starts_with("qwen35"))
    {
        return Ok(None);
    }
    let name = format!("{} {}", local.file_name, local.label).to_lowercase();
    let by_name = |size: &str| has_token(&name, size);
    let by_dim = |size: &str| match info.embedding {
        Some(4096) => size == "9b",
        Some(2560) => size == "4b",
        _ => false,
    };
    let addons = vision_entries();
    let size_of = |addon: &VisionAddon| addon.file.id.split('-').nth(1).unwrap_or_default().to_owned();
    let hit = addons
        .iter()
        .find(|a| by_name(&size_of(a)))
        .or_else(|| addons.iter().find(|a| by_dim(&size_of(a))));
    Ok(hit.cloned())
}

/// `token` standing alone between non-alphanumeric characters (e.g. "9b" in "qwen-3.5-9b-q4").
fn has_token(text: &str, token: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(token).any(|(start, _)| {
        let before = start.checked_sub(1).and_then(|i| bytes.get(i));
        let after = bytes.get(start + token.len());
        !before.is_some_and(u8::is_ascii_alphanumeric) && !after.is_some_and(u8::is_ascii_alphanumeric)
    })
}
