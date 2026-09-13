use crate::error::{AdapterError, ProfileArchiveOperation};
use base64::Engine;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

pub(crate) fn encode_firefox_profile(directory: &Path) -> Result<String, AdapterError> {
    let root = fs::canonicalize(directory)
        .map_err(|error| archive_error(directory, ProfileArchiveOperation::ReadDirectory, error))?;
    if !root.is_dir() {
        return Err(AdapterError::ProfileArchive {
            directory: directory.to_path_buf(),
            operation: ProfileArchiveOperation::ReadDirectory,
            detail: "configured profile path is not a directory".to_owned(),
        });
    }

    let mut entries = Vec::new();
    collect_entries(directory, &root, directory, &mut entries)?;
    entries.sort();

    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    for path in entries {
        let relative = path
            .strip_prefix(directory)
            .map_err(|error| archive_error(directory, ProfileArchiveOperation::ReadEntry, error))?;
        let name = relative.to_string_lossy().replace('\\', "/");
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| archive_error(directory, ProfileArchiveOperation::ReadEntry, error))?;
        if metadata.is_dir() {
            archive
                .add_directory(format!("{name}/"), options)
                .map_err(|error| {
                    archive_error(directory, ProfileArchiveOperation::WriteArchive, error)
                })?;
        } else {
            archive.start_file(name, options).map_err(|error| {
                archive_error(directory, ProfileArchiveOperation::WriteArchive, error)
            })?;
            let bytes = fs::read(&path).map_err(|error| {
                archive_error(directory, ProfileArchiveOperation::ReadEntry, error)
            })?;
            archive.write_all(&bytes).map_err(|error| {
                archive_error(directory, ProfileArchiveOperation::WriteArchive, error)
            })?;
        }
    }
    let bytes = archive
        .finish()
        .map_err(|error| archive_error(directory, ProfileArchiveOperation::WriteArchive, error))?
        .into_inner();
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn collect_entries(
    configured: &Path,
    root: &Path,
    directory: &Path,
    entries: &mut Vec<PathBuf>,
) -> Result<(), AdapterError> {
    let reader = fs::read_dir(directory).map_err(|error| {
        archive_error(configured, ProfileArchiveOperation::ReadDirectory, error)
    })?;
    for entry in reader {
        let entry = entry.map_err(|error| {
            archive_error(configured, ProfileArchiveOperation::ReadEntry, error)
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            archive_error(configured, ProfileArchiveOperation::ReadEntry, error)
        })?;
        if metadata.file_type().is_symlink() {
            return Err(AdapterError::ProfileArchive {
                directory: configured.to_path_buf(),
                operation: ProfileArchiveOperation::ReadEntry,
                detail: format!("profile entry '{}' is a symbolic link", path.display()),
            });
        }
        let canonical = fs::canonicalize(&path).map_err(|error| {
            archive_error(configured, ProfileArchiveOperation::ReadEntry, error)
        })?;
        if !canonical.starts_with(root) {
            return Err(AdapterError::ProfileArchive {
                directory: configured.to_path_buf(),
                operation: ProfileArchiveOperation::ReadEntry,
                detail: format!(
                    "profile entry '{}' escapes the configured root",
                    path.display()
                ),
            });
        }
        entries.push(path.clone());
        if metadata.is_dir() {
            collect_entries(configured, root, &path, entries)?;
        } else if !metadata.is_file() {
            return Err(AdapterError::ProfileArchive {
                directory: configured.to_path_buf(),
                operation: ProfileArchiveOperation::ReadEntry,
                detail: format!("profile entry '{}' is not a regular file", path.display()),
            });
        }
    }
    Ok(())
}

fn archive_error(
    directory: &Path,
    operation: ProfileArchiveOperation,
    error: impl std::fmt::Display,
) -> AdapterError {
    AdapterError::ProfileArchive {
        directory: directory.to_path_buf(),
        operation,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    struct TempProfile {
        path: PathBuf,
    }

    impl TempProfile {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "harw-firefox-profile-{label}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir(&path).expect("temporary profile root is created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempProfile {
        fn drop(&mut self) {
            let _cleanup_result = fs::remove_dir_all(&self.path);
        }
    }

    fn decoded_archive(encoded: &str) -> zip::ZipArchive<Cursor<Vec<u8>>> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("generated archive is valid base64");
        zip::ZipArchive::new(Cursor::new(bytes)).expect("generated bytes are a valid zip archive")
    }

    #[test]
    fn encoding_the_same_profile_twice_is_byte_deterministic() {
        let profile = TempProfile::new("deterministic");
        fs::create_dir(profile.path().join("storage"))
            .expect("nested profile directory is created");
        fs::write(
            profile.path().join("prefs.js"),
            b"user_pref(\"a\", true);\n",
        )
        .expect("profile preference is written");
        fs::write(
            profile.path().join("storage/state.json"),
            b"{\"ready\":true}",
        )
        .expect("nested profile state is written");

        let first = encode_firefox_profile(profile.path()).expect("first encoding succeeds");
        let second = encode_firefox_profile(profile.path()).expect("second encoding succeeds");

        assert_eq!(first, second);
    }

    #[test]
    fn decoding_preserves_relative_nested_and_empty_directory_paths() {
        let profile = TempProfile::new("paths");
        fs::create_dir_all(profile.path().join("storage/default/empty"))
            .expect("nested empty directory is created");
        fs::write(
            profile.path().join("storage/default/state.json"),
            b"{\"version\":1}",
        )
        .expect("nested state file is written");

        let encoded = encode_firefox_profile(profile.path()).expect("profile encoding succeeds");
        let mut archive = decoded_archive(&encoded);
        let names: Vec<String> = archive.file_names().map(str::to_owned).collect();

        assert!(names.contains(&"storage/".to_owned()));
        assert!(names.contains(&"storage/default/".to_owned()));
        assert!(names.contains(&"storage/default/empty/".to_owned()));
        assert!(names.contains(&"storage/default/state.json".to_owned()));
        assert!(names.iter().all(|name| !name.starts_with('/')));

        let mut state = String::new();
        archive
            .by_name("storage/default/state.json")
            .expect("nested state path exists")
            .read_to_string(&mut state)
            .expect("nested state bytes are readable");
        assert_eq!(state, "{\"version\":1}");
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_is_rejected_as_a_typed_archive_read_error() {
        use std::os::unix::fs::symlink;

        let profile = TempProfile::new("symlink");
        fs::write(profile.path().join("real.txt"), b"content").expect("symlink target is written");
        symlink("real.txt", profile.path().join("alias.txt")).expect("profile symlink is created");

        let error = encode_firefox_profile(profile.path())
            .expect_err("profile symlinks must be rejected before archiving");
        match error {
            AdapterError::ProfileArchive {
                operation, detail, ..
            } => {
                assert_eq!(operation, ProfileArchiveOperation::ReadEntry);
                assert!(detail.contains("symbolic link"));
                assert!(detail.contains("alias.txt"));
            }
            other => panic!("expected typed profile archive error, got {other}"),
        }
    }
}
