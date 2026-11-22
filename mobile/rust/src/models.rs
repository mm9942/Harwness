//! Catalog and on-disk state of the on-device language models.
//!
//! The platform layer downloads into `<file_name>.part`; this module verifies
//! the single SHA-256 and promotes the file to its final name.

use std::fs::{self, DirBuilder, File};
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use aws_lc_rs::digest::{Context, SHA256};

use crate::MobileError;

const HASH_BUFFER_BYTES: usize = 1024 * 1024;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ModelFileState {
    Missing,
    Partial,
    Complete,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LocalModel {
    pub id: String,
    pub file_name: String,
    pub state: ModelFileState,
    pub bytes_on_disk: u64,
    pub size_bytes: u64,
}

#[expect(clippy::too_many_arguments, reason = "flat literal catalog row")]
fn entry(
    id: &str,
    label: &str,
    repo: &str,
    revision: &str,
    file_name: &str,
    size_bytes: u64,
    sha256: &str,
    min_ram_bytes: u64,
    recommended: bool,
) -> CatalogModel {
    CatalogModel {
        id: id.to_owned(),
        label: label.to_owned(),
        repo: repo.to_owned(),
        revision: revision.to_owned(),
        file_name: file_name.to_owned(),
        url: format!("https://huggingface.co/{repo}/resolve/{revision}/{file_name}"),
        size_bytes,
        sha256: sha256.to_owned(),
        min_ram_bytes,
        recommended,
    }
}

fn catalog_entries() -> Vec<CatalogModel> {
    vec![
        entry(
            "qwen3-4b-q4km",
            "Qwen3 4B (Q4_K_M)",
            "Qwen/Qwen3-4B-GGUF",
            "bc640142c66e1fdd12af0bd68f40445458f3869b",
            "Qwen3-4B-Q4_K_M.gguf",
            2_497_280_256,
            "7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5",
            6_442_450_944,
            true,
        ),
        entry(
            "qwen3-1.7b-q8",
            "Qwen3 1.7B (Q8_0)",
            "Qwen/Qwen3-1.7B-GGUF",
            "90862c4b9d2787eaed51d12237eafdfe7c5f6077",
            "Qwen3-1.7B-Q8_0.gguf",
            1_834_426_016,
            "061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a",
            4_294_967_296,
            false,
        ),
    ]
}

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

fn find_model(model_id: &str) -> Result<CatalogModel, MobileError> {
    catalog_entries()
        .into_iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| session("unknown model"))
}

fn part_path(dir: &Path, model: &CatalogModel) -> PathBuf {
    dir.join(format!("{}.part", model.file_name))
}

fn file_len(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
}

fn local_in(dir: &Path, model: &CatalogModel) -> LocalModel {
    let (state, bytes_on_disk) = match file_len(&dir.join(&model.file_name)) {
        Some(len) if len == model.size_bytes => (ModelFileState::Complete, len),
        Some(len) => (ModelFileState::Invalid, len),
        None => match file_len(&part_path(dir, model)) {
            Some(len) => (ModelFileState::Partial, len),
            None => (ModelFileState::Missing, 0),
        },
    };
    LocalModel {
        id: model.id.clone(),
        file_name: model.file_name.clone(),
        state,
        bytes_on_disk,
        size_bytes: model.size_bytes,
    }
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
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

fn finalize_in(dir: &Path, model: &CatalogModel) -> Result<LocalModel, MobileError> {
    let part = part_path(dir, model);
    if file_len(&part) != Some(model.size_bytes) {
        return Err(session("model download is incomplete"));
    }
    let digest = sha256_file(&part).map_err(internal)?;
    if digest != model.sha256 {
        match fs::remove_file(&part) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(internal(error)),
        }
        return Err(session("model file failed verification"));
    }
    fs::OpenOptions::new()
        .write(true)
        .open(&part)
        .and_then(|file| file.sync_all())
        .map_err(internal)?;
    fs::rename(&part, dir.join(&model.file_name)).map_err(internal)?;
    if let Ok(directory) = File::open(dir) {
        // Best effort: the rename is already visible, this only hardens it.
        let _ = directory.sync_all();
    }
    Ok(local_in(dir, model))
}

fn remove_if_present(path: &Path) -> Result<(), MobileError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(internal(error)),
    }
}

/// The models the app offers, in display order.
#[uniffi::export]
pub fn model_catalog() -> Vec<CatalogModel> {
    catalog_entries()
}

/// One entry per catalog model describing what is on disk in `models_dir`.
#[uniffi::export]
pub fn local_models(models_dir: String) -> Result<Vec<LocalModel>, MobileError> {
    let dir = checked_dir(&models_dir)?;
    Ok(catalog_entries()
        .iter()
        .map(|model| local_in(&dir, model))
        .collect())
}

/// Verifies the finished `.part` download and moves it to its final name.
#[uniffi::export]
pub fn finalize_model(models_dir: String, model_id: String) -> Result<LocalModel, MobileError> {
    let dir = checked_dir(&models_dir)?;
    let model = find_model(&model_id)?;
    finalize_in(&dir, &model)
}

/// Removes the final and partial files of a model; missing files are fine.
#[uniffi::export]
pub fn delete_model(models_dir: String, model_id: String) -> Result<(), MobileError> {
    let dir = checked_dir(&models_dir)?;
    let model = find_model(&model_id)?;
    remove_if_present(&dir.join(&model.file_name))?;
    remove_if_present(&part_path(&dir, &model))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn fake() -> CatalogModel {
        CatalogModel {
            id: "fake".to_owned(),
            label: "Fake".to_owned(),
            repo: "x/y".to_owned(),
            revision: "r".to_owned(),
            file_name: "fake.gguf".to_owned(),
            url: "https://huggingface.co/x/y/resolve/r/fake.gguf".to_owned(),
            size_bytes: 3,
            sha256: ABC_SHA256.to_owned(),
            min_ram_bytes: 0,
            recommended: false,
        }
    }

    #[test]
    fn catalog_urls_pin_the_revision() {
        let catalog = model_catalog();
        assert_eq!(catalog.len(), 2);
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
    fn local_models_reports_missing_partial_complete_invalid() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let model = fake();
        assert_eq!(local_in(dir, &model).state, ModelFileState::Missing);

        fs::write(part_path(dir, &model), b"ab")?;
        let partial = local_in(dir, &model);
        assert_eq!(partial.state, ModelFileState::Partial);
        assert_eq!(partial.bytes_on_disk, 2);

        fs::write(dir.join(&model.file_name), b"abcd")?;
        assert_eq!(local_in(dir, &model).state, ModelFileState::Invalid);

        fs::write(dir.join(&model.file_name), b"abc")?;
        let complete = local_in(dir, &model);
        assert_eq!(complete.state, ModelFileState::Complete);
        assert_eq!(complete.bytes_on_disk, 3);

        let listed = local_models(dir.to_string_lossy().into_owned())?;
        assert_eq!(listed.len(), 2);
        assert!(
            listed
                .iter()
                .all(|entry| entry.state == ModelFileState::Missing)
        );
        Ok(())
    }

    #[test]
    fn finalize_rejects_wrong_hash_and_removes_part() -> TestResult {
        let temp = tempfile::tempdir()?;
        let model = fake();
        let part = part_path(temp.path(), &model);
        fs::write(&part, b"abd")?;
        let result = finalize_in(temp.path(), &model);
        assert!(matches!(
            result,
            Err(MobileError::Session { ref detail }) if detail == "model file failed verification"
        ));
        assert!(!part.exists());
        Ok(())
    }

    #[test]
    fn finalize_accepts_matching_hash_and_renames() -> TestResult {
        let temp = tempfile::tempdir()?;
        let model = fake();
        let part = part_path(temp.path(), &model);
        fs::write(&part, b"abc")?;
        let done = finalize_in(temp.path(), &model)?;
        assert_eq!(done.state, ModelFileState::Complete);
        assert!(!part.exists());
        assert_eq!(fs::read(temp.path().join(&model.file_name))?, b"abc");
        Ok(())
    }

    #[test]
    fn finalize_incomplete_keeps_part() -> TestResult {
        let temp = tempfile::tempdir()?;
        let model = fake();
        let part = part_path(temp.path(), &model);
        let missing = finalize_in(temp.path(), &model);
        assert!(matches!(
            missing,
            Err(MobileError::Session { ref detail }) if detail == "model download is incomplete"
        ));
        fs::write(&part, b"ab")?;
        let short = finalize_in(temp.path(), &model);
        assert!(matches!(
            short,
            Err(MobileError::Session { ref detail }) if detail == "model download is incomplete"
        ));
        assert!(part.exists());
        Ok(())
    }

    #[test]
    fn unknown_model_is_a_session_error() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().to_string_lossy().into_owned();
        assert!(matches!(
            finalize_model(dir.clone(), "nope".to_owned()),
            Err(MobileError::Session { ref detail }) if detail == "unknown model"
        ));
        assert!(matches!(
            delete_model(dir, "nope".to_owned()),
            Err(MobileError::Session { ref detail }) if detail == "unknown model"
        ));
        Ok(())
    }

    #[test]
    fn relative_models_dir_is_rejected() {
        assert!(matches!(
            local_models("models".to_owned()),
            Err(MobileError::Internal { ref detail }) if detail == "models directory must be absolute"
        ));
    }
}
