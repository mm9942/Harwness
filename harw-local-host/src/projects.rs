//! Ordner-basierter Projekt-Index des lokalen Hosts (Android-App ohne Profil-Store).
//!
//! Projekte sind Ordner unter `<projects_root>/<slug>`; der Projektschluessel ist der Slug.
//! Sessions stammen aus den Metas des Transcript-Verzeichnisses des Hosts. Der Index
//! schreibt nur beim Anlegen eines neuen Projektordners (`resolve_project`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harw_protocol::session_wire::{PageParams, ProjectSummary, ProjectsParams};
use harw_session_host::HostError;
use harw_session_host::index::{
    ProfileIndex, ProfilePage, ProfileRow, ProfileSessionSelection, ProjectPage, ResolvedProject,
    ThreadOrigin,
    decode_cursor, encode_cursor,
};
use harw_session_store::{TranscriptStore, meta};
use harw_types::SessionId;
use jiff::Timestamp;

const JSONL_SUFFIX: &str = ".jsonl";
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const NO_PROJECT_LABEL: &str = "Ohne Projekt";

/// Projekt-Index ueber `<projects_root>` und dem Transcript-Verzeichnis des Hosts.
#[derive(Debug)]
pub struct FolderProfileIndex {
    sessions_root: PathBuf,
    projects_root: PathBuf,
}

impl FolderProfileIndex {
    /// Index ueber `sessions_root` (Transcripte) und `projects_root` (Projektordner).
    #[must_use]
    pub fn new(sessions_root: PathBuf, projects_root: PathBuf) -> Self {
        Self {
            sessions_root,
            projects_root,
        }
    }

    fn canonical_projects_root(&self) -> Option<PathBuf> {
        std::fs::canonicalize(&self.projects_root).ok()
    }

    fn rows(
        &self,
        include_archived: bool,
        selection: ProfileSessionSelection,
    ) -> Result<Vec<ProfileRow>, HostError> {
        let dir = match std::fs::read_dir(&self.sessions_root) {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(HostError::Storage("sessions root unreadable".to_owned())),
        };
        let mut rows = Vec::new();
        for item in dir.flatten() {
            let name = item.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = name.strip_suffix(JSONL_SUFFIX) else {
                continue;
            };
            if stem.is_empty() || name.contains(".corrupt-") || !is_plain_component(stem) {
                continue;
            }
            let id = SessionId::from_str(stem);
            if let Some(row) = self.build_row(&id, &item.path()) {
                if (include_archived || !row.archived) && selection.allows(&row) {
                    rows.push(row);
                }
            }
        }
        Ok(rows)
    }

    fn build_row(&self, id: &SessionId, jsonl: &Path) -> Option<ProfileRow> {
        let md = std::fs::symlink_metadata(jsonl).ok()?;
        if !md.file_type().is_file() {
            return None;
        }
        // Do not decode internal prompts to derive row titles.
        match TranscriptStore::new(&self.sessions_root).catalog_session_purpose(id).ok()? {
            Some(purpose) => {
                let archived = meta::load(&self.sessions_root, id)
                    .ok().flatten().is_some_and(|saved| saved.archived);
                let updated_at = Timestamp::try_from(md.modified().ok()?).ok()?;
                return ProfileRow::system_metadata(id.clone(), updated_at, archived, purpose);
            }
            None => {}
        }
        let session_meta = meta::peek(&self.sessions_root, id).ok()?;
        if session_meta.turns == 0 && session_meta.first_user_message.is_none() {
            return None;
        }
        let mut updated_at =
            Timestamp::try_from(md.modified().ok()?).unwrap_or(session_meta.last_opened_at);
        updated_at = updated_at.max(session_meta.last_opened_at);
        if let Some(activity) = session_meta.last_activity_at {
            updated_at = updated_at.max(activity);
        }
        let project_key = session_meta
            .project_key
            .clone()
            .filter(|key| !key.is_empty());
        Some(ProfileRow {
            session_id: id.clone(),
            title: session_meta.display_title(),
            stored_title: (session_meta.title_source != harw_session_store::TitleSource::Fallback)
                .then(|| session_meta.title.clone()).flatten(),
            project_label: project_key.clone(),
            project_key,
            updated_at,
            turns: session_meta.turns,
            first_message: session_meta.first_user_message.clone(),
            origin: ThreadOrigin::Gateway,
            last_origin: session_meta.last_origin,
            archived: session_meta.archived,
        })
    }

    /// Projektordner (Slug -> kanonischer Pfad), nur echte Ordner.
    fn folders(&self) -> BTreeMap<String, PathBuf> {
        let mut out = BTreeMap::new();
        let Some(root) = self.canonical_projects_root() else {
            return out;
        };
        let Ok(dir) = std::fs::read_dir(&root) else {
            return out;
        };
        for item in dir.flatten() {
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_safe_slug(&name) {
                continue;
            }
            if let Ok(md) = std::fs::symlink_metadata(item.path())
                && md.file_type().is_dir()
                && let Ok(canonical) = std::fs::canonicalize(item.path())
            {
                out.insert(name, canonical);
            }
        }
        out
    }
}

impl ProfileIndex for FolderProfileIndex {
    fn sessions_root(&self) -> &Path {
        &self.sessions_root
    }

    fn supports_dream_session_rows(&self) -> bool {
        true
    }

    fn supports_system_session_rows(&self) -> bool {
        true
    }

    fn projects(&self, params: &ProjectsParams) -> Result<ProjectPage, HostError> {
        struct Group {
            count: u32,
            last: Timestamp,
        }
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        for row in self.rows(false, ProfileSessionSelection::Ordinary)? {
            let key = row.project_key.clone().unwrap_or_default();
            let group = groups.entry(key).or_insert(Group {
                count: 0,
                last: row.updated_at,
            });
            group.count = group.count.saturating_add(1);
            group.last = group.last.max(row.updated_at);
        }
        let now_epoch = Timestamp::UNIX_EPOCH;
        let mut summaries: Vec<ProjectSummary> = Vec::new();
        for (slug, root) in self.folders() {
            let group = groups.remove(&slug);
            let folder_time = std::fs::metadata(&root)
                .ok()
                .and_then(|md| md.modified().ok())
                .and_then(|time| Timestamp::try_from(time).ok())
                .unwrap_or(now_epoch);
            summaries.push(ProjectSummary {
                label: slug.clone(),
                project_key: slug,
                path_hint: None,
                session_count: group.as_ref().map_or(0, |g| g.count),
                last_activity: group.map_or(folder_time, |g| g.last),
                active_count: 0,
            });
        }
        // Sessions ohne (oder mit nicht mehr vorhandenem) Projekt.
        for (key, group) in groups {
            summaries.push(ProjectSummary {
                label: if key.is_empty() {
                    NO_PROJECT_LABEL.to_owned()
                } else {
                    key.clone()
                },
                project_key: key,
                path_hint: None,
                session_count: group.count,
                last_activity: group.last,
                active_count: 0,
            });
        }
        let query = params.query.as_deref().map(str::trim).unwrap_or_default();
        if !query.is_empty() {
            let needle = query.to_lowercase();
            summaries.retain(|summary| summary.label.to_lowercase().contains(&needle));
        }
        summaries.sort_by(|a, b| {
            b.last_activity
                .as_millisecond()
                .cmp(&a.last_activity.as_millisecond())
                .then_with(|| a.project_key.cmp(&b.project_key))
        });
        if let Some(cursor) = params.cursor.as_deref() {
            let (at, key) = decode_cursor(cursor)
                .ok_or_else(|| HostError::Protocol("invalid cursor".to_owned()))?;
            let cur_ms = at.as_millisecond();
            summaries.retain(|summary| {
                let ms = summary.last_activity.as_millisecond();
                ms < cur_ms || (ms == cur_ms && summary.project_key > key)
            });
        }
        let limit = clamp_limit(params.limit);
        let has_more = summaries.len() > limit;
        summaries.truncate(limit);
        let next_cursor = if has_more {
            summaries
                .last()
                .map(|last| encode_cursor(last.last_activity, &last.project_key))
        } else {
            None
        };
        Ok(ProjectPage {
            projects: summaries,
            next_cursor,
        })
    }

    fn page(&self, params: &PageParams) -> Result<ProfilePage, HostError> {
        let mut rows = self.rows(
            params.include_archived, ProfileSessionSelection::from_page(params)?,
        )?;
        if let Some(wanted) = params.project_key.as_deref() {
            if wanted.is_empty() {
                rows.retain(|row| row.project_key.is_none());
            } else {
                rows.retain(|row| row.project_key.as_deref() == Some(wanted));
            }
        }
        let query = params.query.as_deref().map(str::trim).unwrap_or_default();
        if !query.is_empty() {
            let needle = query.to_lowercase();
            rows.retain(|row| {
                row.title.to_lowercase().contains(&needle)
                    || row
                        .first_message
                        .as_deref()
                        .is_some_and(|text| text.to_lowercase().contains(&needle))
            });
        }
        sort_rows(&mut rows);
        if let Some(cursor) = params.cursor.as_deref() {
            let (at, key) = decode_cursor(cursor)
                .ok_or_else(|| HostError::Protocol("invalid cursor".to_owned()))?;
            let cur_ms = at.as_millisecond();
            rows.retain(|row| {
                let ms = row.updated_at.as_millisecond();
                ms < cur_ms || (ms == cur_ms && row.session_id.as_str() < key.as_str())
            });
        }
        let limit = clamp_limit(params.limit);
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = if has_more {
            rows.last()
                .map(|last| encode_cursor(last.updated_at, last.session_id.as_str()))
        } else {
            None
        };
        Ok(ProfilePage { rows, next_cursor })
    }

    fn row(&self, id: &SessionId) -> Result<Option<ProfileRow>, HostError> {
        if !is_plain_component(id.as_str()) {
            return Ok(None);
        }
        let jsonl = self
            .sessions_root
            .join(format!("{}{JSONL_SUFFIX}", id.as_str()));
        Ok(self.build_row(id, &jsonl).filter(|row| !row.is_system()))
    }

    fn newest(&self, limit: usize) -> Result<Vec<ProfileRow>, HostError> {
        let mut rows = self.rows(false, ProfileSessionSelection::Ordinary)?;
        sort_rows(&mut rows);
        rows.truncate(limit);
        Ok(rows)
    }

    fn resolve_project(&self, project_key: &str) -> Result<Option<ResolvedProject>, HostError> {
        if !is_safe_slug(project_key) {
            return Ok(None);
        }
        // Der Projektordner wird bei Bedarf angelegt (neues Projekt).
        if std::fs::create_dir_all(&self.projects_root).is_err() {
            return Ok(None);
        }
        let Some(parent) = self.canonical_projects_root() else {
            return Ok(None);
        };
        let target = parent.join(project_key);
        match std::fs::symlink_metadata(&target) {
            Ok(found) if found.file_type().is_dir() => {}
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::create_dir(&target).is_err() {
                    return Ok(None);
                }
            }
            Err(_) => return Ok(None),
        }
        let Ok(canonical) = std::fs::canonicalize(&target) else {
            return Ok(None);
        };
        if canonical.parent() != Some(parent.as_path()) {
            return Ok(None);
        }
        Ok(Some(ResolvedProject {
            project_key: project_key.to_owned(),
            label: project_key.to_owned(),
            project_root: canonical,
        }))
    }

    fn supports_existing_project_assignment(&self) -> bool {
        true
    }

    fn resolve_existing_project(&self, project_key: &str) -> Result<Option<ResolvedProject>, HostError> {
        if !is_safe_slug(project_key) {
            return Ok(None);
        }
        let Some(parent) = self.canonical_projects_root() else { return Ok(None) };
        let target = parent.join(project_key);
        if !std::fs::symlink_metadata(&target).is_ok_and(|md| md.file_type().is_dir()) {
            return Ok(None);
        }
        let Ok(canonical) = std::fs::canonicalize(&target) else { return Ok(None) };
        if canonical.parent() != Some(parent.as_path()) || canonical != target || !canonical.is_dir() {
            return Ok(None);
        }
        Ok(Some(ResolvedProject {
            project_key: project_key.to_owned(),
            label: project_key.to_owned(),
            project_root: canonical,
        }))
    }

    fn workdir(&self, id: &SessionId) -> Result<Option<PathBuf>, HostError> {
        if !is_plain_component(id.as_str()) {
            return Ok(None);
        }
        let Ok(Some(session_meta)) = meta::load(&self.sessions_root, id) else {
            return Ok(None);
        };
        let Some(dir) = session_meta
            .project_root
            .clone()
            .or_else(|| session_meta.cwd.clone())
        else {
            return Ok(None);
        };
        let (Ok(canonical), Some(root)) =
            (std::fs::canonicalize(&dir), self.canonical_projects_root())
        else {
            return Ok(None);
        };
        if canonical.is_dir() && canonical != root && canonical.starts_with(&root) {
            Ok(Some(canonical))
        } else {
            Ok(None)
        }
    }
}

fn sort_rows(rows: &mut [ProfileRow]) {
    rows.sort_by(|a, b| {
        b.updated_at
            .as_millisecond()
            .cmp(&a.updated_at.as_millisecond())
            .then_with(|| b.session_id.as_str().cmp(a.session_id.as_str()))
    });
}

fn clamp_limit(limit: Option<u32>) -> usize {
    match limit {
        None | Some(0) => DEFAULT_LIMIT,
        Some(n) => usize::try_from(n).unwrap_or(MAX_LIMIT).min(MAX_LIMIT),
    }
}

fn is_plain_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', '\0'])
}

/// Sicherer Projekt-Slug: 1..=40 Zeichen aus `a-z0-9-`, nicht mit `-` beginnend.
fn is_safe_slug(slug: &str) -> bool {
    (1..=40).contains(&slug.len())
        && !slug.starts_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use harw_session_store::SessionPurpose;
    use harw_types::ThreadRef;

    use super::*;

    #[test]
    fn embedded_dream_metadata_is_opt_in_only()
        -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        let id = SessionId::from_str("embedded-dream");
        let directory = root.path().join("sessions");
        TranscriptStore::new(&directory).ensure_session_purpose(
            &id, &ThreadRef::from_str("gateway-dream:embedded-dream"), SessionPurpose::Dream,
        )?;
        let index = FolderProfileIndex::new(directory, root.path().join("projects"));
        assert!(index.page(&PageParams::default())?.rows.is_empty());
        assert!(index.newest(100)?.is_empty());
        assert!(index.row(&id)?.is_none());
        assert!(index.projects(&ProjectsParams::default())?.projects.is_empty());
        let rows = index.page(&PageParams {
            include_dreams: true, ..PageParams::default()
        })?.rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, id);
        assert!(rows[0].is_dream() && rows[0].first_message.is_none());
        let system = index.page(&PageParams {
            include_system: true, ..PageParams::default()
        })?.rows;
        assert_eq!(system, rows);
        Ok(())
    }
}
