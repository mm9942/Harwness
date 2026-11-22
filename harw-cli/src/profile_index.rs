//! Profil-Index für das Teilen (Plan §3.2 Schritt 3/4). Nur lesen.
//!
//! # Description
//! `FsProfileIndex` implementiert [`ProfileIndex`] über dem Transcript-Verzeichnis des
//! Profils (`<profile>/sessions`). Je Session wird `meta::peek` gelesen (nie
//! `discover_sessions`), die Herkunft (`ThreadOrigin`) stammt aus dem Thread des ERSTEN
//! Datensatzes. Ergebnisse werden je Session zwischengespeichert; gültig bleibt ein Eintrag,
//! solange (mtime, Länge) der `.jsonl` und die mtime der `.meta.json` gleich bleiben
//! (ein `stat` je Datei und Abfrage). Die Herkunft wird nur neu gelesen, wenn die Datei
//! kürzer wurde.
//!
//! Sichtbar ist eine Session nur, wenn die `.jsonl` eine reguläre Datei ist (kein Symlink,
//! kein `.corrupt-`), `meta::peek` gelingt, sie nicht leer ist und (falls gesetzt) nicht
//! älter als `since` ist. Verwaiste Metas (ohne `.jsonl`) werden nie aufgezählt. Der Index
//! schreibt nie.

// Wird erst von der Verdrahtung (S4a) benutzt.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use harw_protocol::session_wire::{PageParams, ProjectSummary, ProjectsParams};
use harw_session_host::HostError;
use harw_session_host::index::{
    ProfileIndex, ProfilePage, ProfileRow, ProjectPage, ResolvedProject, ThreadOrigin,
    decode_cursor, encode_cursor,
};
use harw_session_store::meta;
use harw_types::SessionId;
use jiff::Timestamp;

const JSONL_SUFFIX: &str = ".jsonl";
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
/// Obergrenze beim Lesen der ersten Zeile (Herkunft).
const FIRST_LINE_CAP: u64 = 8 * 1024 * 1024;
const NO_PROJECT_LABEL: &str = "Ohne Projekt";

/// Optionen aus `SharePlan` (session_listener.rs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct IndexOptions {
    /// Nur Sessions mit `updated_at >= since`.
    pub since: Option<Timestamp>,
    /// Kanonische, existierende Wurzeln; leer = keine Einschränkung.
    pub share_roots: Vec<PathBuf>,
}

type FileSig = (SystemTime, u64);

/// Zwischengespeicherter Zustand einer Session.
struct Entry {
    jsonl_sig: FileSig,
    meta_mtime: Option<SystemTime>,
    origin: ThreadOrigin,
    /// `None`: `meta::peek` fehlgeschlagen.
    row: Option<ProfileRow>,
    project_root: Option<PathBuf>,
    /// Sichtbar für Listen (nicht leer, nicht vor `since`).
    visible: bool,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<SessionId, Arc<Entry>>,
}

pub(crate) struct FsProfileIndex {
    sessions_root: PathBuf,
    options: IndexOptions,
    cache: Mutex<Cache>,
}

impl FsProfileIndex {
    /// Kein I/O außer `canonicalize` der `share_roots` (nicht existierende Wurzeln werden mit
    /// `warn!` ohne Pfad verworfen).
    pub(crate) fn open(sessions_root: PathBuf, options: IndexOptions) -> Self {
        let mut share_roots = Vec::with_capacity(options.share_roots.len());
        for root in &options.share_roots {
            match std::fs::canonicalize(root) {
                Ok(canonical) => share_roots.push(canonical),
                Err(_) => tracing::warn!("share root does not exist; ignoring it"),
            }
        }
        // Waren Wurzeln konfiguriert, aber keine gültig, bleibt die Einschränkung bestehen
        // (nichts ist erlaubt) statt stillschweigend aufzuheben.
        let restrict_all = !options.share_roots.is_empty() && share_roots.is_empty();
        let options = IndexOptions {
            since: options.since,
            share_roots,
        };
        let index = Self {
            sessions_root,
            options,
            cache: Mutex::new(Cache::default()),
        };
        if restrict_all {
            return index.with_deny_all_roots();
        }
        index
    }

    // Eine nicht auflösbare Wurzel (leerer Pfad) lässt `starts_with` nie zutreffen.
    fn with_deny_all_roots(mut self) -> Self {
        self.options.share_roots = vec![PathBuf::new()];
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Alle Einträge (auch unsichtbare), Cache wird aufgefrischt und bereinigt.
    fn scan(&self) -> Result<Vec<(SessionId, Arc<Entry>)>, HostError> {
        let dir = match std::fs::read_dir(&self.sessions_root) {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.lock().entries.clear();
                return Ok(Vec::new());
            }
            Err(_) => return Err(HostError::Storage("sessions root unreadable".to_owned())),
        };
        let mut cache = self.lock();
        let mut seen: HashSet<SessionId> = HashSet::new();
        let mut out = Vec::new();
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
            if let Some(entry) = self.refresh(&mut cache, &id) {
                seen.insert(id.clone());
                out.push((id, entry));
            }
        }
        cache.entries.retain(|id, _| seen.contains(id));
        Ok(out)
    }

    /// Eintrag einer Session auffrischen (stat je Datei) oder aus dem Cache nehmen.
    fn refresh(&self, cache: &mut Cache, id: &SessionId) -> Option<Arc<Entry>> {
        if !is_plain_component(id.as_str()) {
            return None;
        }
        let jsonl = self
            .sessions_root
            .join(format!("{}{JSONL_SUFFIX}", id.as_str()));
        let md = match std::fs::symlink_metadata(&jsonl) {
            Ok(md) if md.file_type().is_file() => md,
            _ => {
                cache.entries.remove(id);
                return None;
            }
        };
        let jsonl_mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let sig: FileSig = (jsonl_mtime, md.len());
        let meta_mtime = std::fs::symlink_metadata(meta::meta_path(&self.sessions_root, id))
            .ok()
            .and_then(|md| md.modified().ok());

        let previous = cache.entries.get(id).cloned();
        if let Some(prev) = &previous
            && prev.jsonl_sig == sig
            && prev.meta_mtime == meta_mtime
        {
            return Some(Arc::clone(prev));
        }

        let origin = match &previous {
            Some(prev) if sig.1 >= prev.jsonl_sig.1 => prev.origin,
            _ => read_origin(&jsonl),
        };
        let entry = Arc::new(self.build_entry(id, sig, meta_mtime, origin));
        cache.entries.insert(id.clone(), Arc::clone(&entry));
        Some(entry)
    }

    fn build_entry(
        &self,
        id: &SessionId,
        sig: FileSig,
        meta_mtime: Option<SystemTime>,
        origin: ThreadOrigin,
    ) -> Entry {
        let Ok(session_meta) = meta::peek(&self.sessions_root, id) else {
            return Entry {
                jsonl_sig: sig,
                meta_mtime,
                origin,
                row: None,
                project_root: None,
                visible: false,
            };
        };
        let mut updated_at = Timestamp::try_from(sig.0).unwrap_or(session_meta.last_opened_at);
        updated_at = updated_at.max(session_meta.last_opened_at);
        if let Some(activity) = session_meta.last_activity_at {
            updated_at = updated_at.max(activity);
        }
        let project_key = session_meta
            .project_key
            .clone()
            .filter(|key| !key.is_empty());
        let project_label = project_key
            .as_deref()
            .map(|key| label_for(session_meta.project_root.as_deref(), key));
        let is_empty = session_meta.turns == 0 && session_meta.first_user_message.is_none();
        let visible = !is_empty && self.options.since.is_none_or(|since| updated_at >= since);
        let row = ProfileRow {
            session_id: id.clone(),
            title: session_meta.display_title(),
            project_key,
            project_label,
            updated_at,
            turns: session_meta.turns,
            first_message: session_meta.first_user_message.clone(),
            origin,
            last_origin: session_meta.last_origin,
            archived: session_meta.archived,
        };
        Entry {
            jsonl_sig: sig,
            meta_mtime,
            origin,
            row: Some(row),
            project_root: session_meta.project_root.clone(),
            visible,
        }
    }

    /// Sichtbare Zeilen (optional ohne Archivierte), unsortiert.
    fn visible_rows(&self, include_archived: bool) -> Result<Vec<ProfileRow>, HostError> {
        Ok(self
            .scan()?
            .into_iter()
            .filter(|(_, entry)| entry.visible)
            .filter_map(|(_, entry)| entry.row.clone())
            .filter(|row| include_archived || !row.archived)
            .collect())
    }

    fn within_share_roots(&self, path: &Path) -> bool {
        self.options.share_roots.is_empty()
            || self
                .options
                .share_roots
                .iter()
                .any(|root| path.starts_with(root))
    }
}

impl ProfileIndex for FsProfileIndex {
    fn sessions_root(&self) -> &Path {
        &self.sessions_root
    }

    fn projects(&self, params: &ProjectsParams) -> Result<ProjectPage, HostError> {
        let entries = self.scan()?;
        struct Group {
            label: String,
            root: Option<PathBuf>,
            count: u32,
            last: Timestamp,
            newest_root_at: Option<Timestamp>,
        }
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        for (_, entry) in &entries {
            let Some(row) = entry.row.as_ref().filter(|_| entry.visible) else {
                continue;
            };
            if row.archived {
                continue;
            }
            let key = row.project_key.clone().unwrap_or_default();
            let group = groups.entry(key.clone()).or_insert_with(|| Group {
                label: if key.is_empty() {
                    NO_PROJECT_LABEL.to_owned()
                } else {
                    label_for(None, &key)
                },
                root: None,
                count: 0,
                last: row.updated_at,
                newest_root_at: None,
            });
            group.count = group.count.saturating_add(1);
            group.last = group.last.max(row.updated_at);
            if let Some(root) = &entry.project_root
                && !key.is_empty()
                && group.newest_root_at.is_none_or(|at| row.updated_at >= at)
            {
                group.root = Some(root.clone());
                group.newest_root_at = Some(row.updated_at);
                group.label = label_for(Some(root), &key);
            }
        }

        let mut label_counts: HashMap<&str, usize> = HashMap::new();
        for group in groups.values() {
            *label_counts.entry(group.label.as_str()).or_default() += 1;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut summaries: Vec<ProjectSummary> = groups
            .iter()
            .map(|(key, group)| {
                let collides = label_counts.get(group.label.as_str()).copied().unwrap_or(0) > 1;
                ProjectSummary {
                    project_key: key.clone(),
                    label: group.label.clone(),
                    path_hint: if collides {
                        group
                            .root
                            .as_deref()
                            .and_then(|root| path_hint(root, home.as_deref()))
                    } else {
                        None
                    },
                    session_count: group.count,
                    last_activity: group.last,
                    active_count: 0,
                }
            })
            .collect();

        let query = params.query.as_deref().map(str::trim).unwrap_or_default();
        if !query.is_empty() {
            let needle = query.to_lowercase();
            summaries.retain(|summary| {
                summary.label.to_lowercase().contains(&needle)
                    || summary
                        .path_hint
                        .as_deref()
                        .is_some_and(|hint| hint.to_lowercase().contains(&needle))
            });
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
        let mut rows = self.visible_rows(params.include_archived)?;
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
        let mut cache = self.lock();
        Ok(self
            .refresh(&mut cache, id)
            .filter(|entry| entry.visible)
            .and_then(|entry| entry.row.clone()))
    }

    fn newest(&self, limit: usize) -> Result<Vec<ProfileRow>, HostError> {
        let mut rows = self.visible_rows(false)?;
        sort_rows(&mut rows);
        rows.truncate(limit);
        Ok(rows)
    }

    fn resolve_project(&self, project_key: &str) -> Result<Option<ResolvedProject>, HostError> {
        if project_key.is_empty() {
            return Ok(None);
        }
        let entries = self.scan()?;
        let newest = entries
            .iter()
            .filter(|(_, entry)| entry.project_root.is_some())
            .filter_map(|(_, entry)| entry.row.as_ref().map(|row| (row, entry)))
            .filter(|(row, _)| row.project_key.as_deref() == Some(project_key))
            .max_by_key(|(row, _)| row.updated_at.as_millisecond());
        let Some((_, entry)) = newest else {
            return Ok(None);
        };
        let Some(root) = entry.project_root.as_deref() else {
            return Ok(None);
        };
        let Ok(canonical) = std::fs::canonicalize(root) else {
            return Ok(None);
        };
        if !canonical.is_dir() || !self.within_share_roots(&canonical) {
            return Ok(None);
        }
        Ok(Some(ResolvedProject {
            project_key: project_key.to_owned(),
            label: label_for(Some(&canonical), project_key),
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
        let Ok(canonical) = std::fs::canonicalize(&dir) else {
            return Ok(None);
        };
        if canonical != dir || !canonical.is_dir() {
            return Ok(None);
        }
        if !self.within_share_roots(&canonical) {
            return Ok(None);
        }
        let mut known: HashSet<PathBuf> = HashSet::new();
        if let Some(own) = session_meta.project_root.as_deref()
            && let Ok(own) = std::fs::canonicalize(own)
        {
            known.insert(own);
        }
        for (_, entry) in self.scan()? {
            if let Some(root) = entry.project_root.as_deref()
                && let Ok(root) = std::fs::canonicalize(root)
            {
                known.insert(root);
            }
        }
        if known.iter().any(|root| canonical.starts_with(root)) {
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

/// Nur ein einzelner, harmloser Dateiname (kein Pfadtrick).
fn is_plain_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', '\0'])
}

/// Thread des ersten Datensatzes -> Herkunft. Alles Unlesbare -> `Other`.
fn read_origin(path: &Path) -> ThreadOrigin {
    #[derive(serde::Deserialize)]
    struct First {
        thread: String,
    }
    let Ok(file) = std::fs::File::open(path) else {
        return ThreadOrigin::Other;
    };
    let mut line = String::new();
    let mut reader = BufReader::new(file.take(FIRST_LINE_CAP));
    if reader.read_line(&mut line).is_err() {
        return ThreadOrigin::Other;
    }
    match serde_json::from_str::<First>(line.trim_end()) {
        Ok(first) => ThreadOrigin::from_thread(&first.thread),
        Err(_) => ThreadOrigin::Other,
    }
}

/// Anzeige-Name: Basename von `project_root`, sonst Teil vor dem letzten `-<12 hex>` im Schlüssel.
fn label_for(root: Option<&Path>, key: &str) -> String {
    if key.is_empty() {
        return NO_PROJECT_LABEL.to_owned();
    }
    if let Some(name) = root
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
    {
        return name;
    }
    match key.rsplit_once('-') {
        Some((prefix, hash))
            if hash.len() == 12
                && hash.bytes().all(|b| b.is_ascii_hexdigit())
                && !prefix.is_empty() =>
        {
            prefix.to_owned()
        }
        _ => key.to_owned(),
    }
}

/// `~/…/<eltern>/<name>` (HOME ersetzt) bzw. `…/<eltern>/<name>` außerhalb von HOME.
fn path_hint(root: &Path, home: Option<&Path>) -> Option<String> {
    let name = root.file_name()?.to_string_lossy().into_owned();
    let parent = root
        .parent()
        .and_then(Path::file_name)
        .map(|parent| parent.to_string_lossy().into_owned());
    if let Some(home) = home.filter(|home| !home.as_os_str().is_empty())
        && let Ok(rest) = root.strip_prefix(home)
    {
        if rest.components().count() <= 1 {
            return Some(format!("~/{name}"));
        }
        return Some(match parent {
            Some(parent) => format!("~/…/{parent}/{name}"),
            None => format!("~/…/{name}"),
        });
    }
    Some(match parent {
        Some(parent) => format!("…/{parent}/{name}"),
        None => format!("…/{name}"),
    })
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::time::{Duration, Instant};

    use harw_session_store::record::{RecordKind, TranscriptRecord};
    use harw_session_store::store::TranscriptStore;
    use harw_types::ThreadRef;

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    fn record_line(id: &str, thread: &str) -> String {
        format!(
            "{{\"session_id\":\"{id}\",\"thread\":\"{thread}\",\"sequence\":0,\
             \"recorded_at\":\"2026-01-01T00:00:00Z\",\"kind\":\"turn\",\"payload\":{{}}}}\n"
        )
    }

    #[derive(Default)]
    struct Spec<'a> {
        thread: &'a str,
        turns: u64,
        first: Option<&'a str>,
        title: Option<&'a str>,
        project: Option<(&'a str, &'a str)>,
        cwd: Option<&'a str>,
        archived: bool,
    }

    fn write_meta(root: &Path, id: &str, spec: &Spec<'_>) -> TestResult {
        let mut value = serde_json::json!({
            "version": 1,
            "session_id": id,
            "title": spec.title,
            "title_source": "none",
            "created_at": "2020-01-01T00:00:00Z",
            "last_opened_at": "2020-01-01T00:00:00Z",
            "cwd": spec.cwd,
            "project_root": spec.project.map(|(root, _)| root),
            "project_key": spec.project.map(|(_, key)| key),
            "first_user_message": spec.first,
            "turns": spec.turns,
        });
        if spec.archived
            && let Some(object) = value.as_object_mut()
        {
            object.insert("archived".to_owned(), serde_json::Value::Bool(true));
        }
        std::fs::write(
            root.join(format!("{id}.meta.json")),
            serde_json::to_vec(&value)?,
        )?;
        Ok(())
    }

    fn write_session(root: &Path, id: &str, spec: &Spec<'_>) -> TestResult {
        let thread = if spec.thread.is_empty() {
            "cli-session:synthetic"
        } else {
            spec.thread
        };
        std::fs::write(root.join(format!("{id}.jsonl")), record_line(id, thread))?;
        write_meta(root, id, spec)
    }

    fn visible_spec() -> Spec<'static> {
        Spec {
            turns: 1,
            first: Some("hallo welt"),
            ..Spec::default()
        }
    }

    fn open(root: &Path) -> FsProfileIndex {
        FsProfileIndex::open(root.to_path_buf(), IndexOptions::default())
    }

    fn all(index: &FsProfileIndex) -> Result<Vec<ProfileRow>, HostError> {
        index
            .page(&PageParams {
                limit: Some(200),
                ..PageParams::default()
            })
            .map(|page| page.rows)
    }

    #[test]
    fn three_thousand_metas_cold_under_300ms_warm_under_50ms() -> TestResult {
        let temp = tempfile::tempdir()?;
        for n in 0..3000 {
            let id = format!("sess-{n:04}");
            write_session(
                temp.path(),
                &id,
                &Spec {
                    turns: 2,
                    first: Some("synthetische Nachricht"),
                    project: Some(("/w/proj", "proj-0123456789ab")),
                    ..Spec::default()
                },
            )?;
        }
        let index = open(temp.path());
        let started = Instant::now();
        let rows = all(&index)?;
        let cold = started.elapsed();
        assert_eq!(rows.len(), 200);
        assert!(cold < Duration::from_millis(300), "cold {cold:?}");

        let started = Instant::now();
        let rows = all(&index)?;
        let warm = started.elapsed();
        assert_eq!(rows.len(), 200);
        assert!(warm < Duration::from_millis(50), "warm {warm:?}");
        Ok(())
    }

    #[test]
    fn append_changes_updated_at() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        let id = SessionId::from_str("sess-append");
        let record = |sequence: u64| TranscriptRecord {
            session_id: id.clone(),
            thread: ThreadRef::from_str("cli-session:synthetic"),
            sequence,
            recorded_at: Timestamp::now(),
            kind: RecordKind::Turn,
            payload: serde_json::json!({}),
        };
        store.append(&record(0))?;
        write_meta(temp.path(), "sess-append", &visible_spec())?;
        let index = open(temp.path());
        let first = index.row(&id)?.ok_or("row missing")?.updated_at;
        std::thread::sleep(Duration::from_millis(60));
        store.append(&record(1))?;
        let second = index
            .row(&id)?
            .ok_or("row missing after append")?
            .updated_at;
        assert!(second > first, "{first} -> {second}");
        Ok(())
    }

    #[test]
    fn web_session_is_not_continuable() -> TestResult {
        let temp = tempfile::tempdir()?;
        write_session(
            temp.path(),
            "sess-web",
            &Spec {
                thread: "web:x",
                ..visible_spec()
            },
        )?;
        write_session(temp.path(), "sess-cli", &visible_spec())?;
        let index = open(temp.path());
        let web = index
            .row(&SessionId::from_str("sess-web"))?
            .ok_or("web missing")?;
        assert_eq!(web.origin, ThreadOrigin::Web);
        assert!(!web.origin.is_continuable());
        let cli = index
            .row(&SessionId::from_str("sess-cli"))?
            .ok_or("cli missing")?;
        assert_eq!(cli.origin, ThreadOrigin::Cli);
        assert!(cli.origin.is_continuable());
        Ok(())
    }

    #[test]
    fn corrupt_orphan_empty_archived_hidden() -> TestResult {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        write_session(root, "sess-ok", &visible_spec())?;
        write_session(root, "sess-empty", &Spec::default())?;
        write_session(
            root,
            "sess-arch",
            &Spec {
                archived: true,
                ..visible_spec()
            },
        )?;
        write_meta(root, "sess-orphan", &visible_spec())?;
        std::fs::write(
            root.join("sess-bad.jsonl.corrupt-123"),
            record_line("sess-bad", "cli-session:x"),
        )?;
        std::fs::write(root.join("sess-ok.jsonl.lock"), b"")?;
        std::fs::write(root.join("sess-ok.meta.lock"), b"")?;
        let index = open(root);
        let ids: Vec<String> = all(&index)?
            .into_iter()
            .map(|row| row.session_id.as_str().to_owned())
            .collect();
        assert_eq!(ids, vec!["sess-ok".to_owned()]);
        assert!(index.row(&SessionId::from_str("sess-empty"))?.is_none());
        assert!(index.row(&SessionId::from_str("sess-orphan"))?.is_none());
        assert!(index.row(&SessionId::from_str("sess-bad"))?.is_none());
        // Archivierte nur auf Wunsch (und `row` liefert sie).
        let with_archived = index.page(&PageParams {
            include_archived: true,
            ..PageParams::default()
        })?;
        assert_eq!(with_archived.rows.len(), 2);
        assert!(index.row(&SessionId::from_str("sess-arch"))?.is_some());
        assert_eq!(index.newest(10)?.len(), 1);
        let projects = index.projects(&ProjectsParams::default())?;
        assert_eq!(
            projects
                .projects
                .iter()
                .map(|p| p.session_count)
                .sum::<u32>(),
            1
        );
        Ok(())
    }

    #[test]
    fn since_filters_older_sessions() -> TestResult {
        let temp = tempfile::tempdir()?;
        write_session(temp.path(), "sess-new", &visible_spec())?;
        let index = FsProfileIndex::open(
            temp.path().to_path_buf(),
            IndexOptions {
                since: Some("2999-01-01T00:00:00Z".parse()?),
                share_roots: Vec::new(),
            },
        );
        assert!(all(&index)?.is_empty());
        Ok(())
    }

    #[test]
    fn name_collision_sets_path_hint() -> TestResult {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let spec = |dir: &'static str, key: &'static str| Spec {
            project: Some((dir, key)),
            ..visible_spec()
        };
        write_session(root, "sess-a", &spec("/w/a/app", "app-aaaaaaaaaaaa"))?;
        write_session(root, "sess-b", &spec("/w/b/app", "app-bbbbbbbbbbbb"))?;
        write_session(root, "sess-c", &spec("/w/c/lib", "lib-cccccccccccc"))?;
        let projects = open(root).projects(&ProjectsParams::default())?.projects;
        let hint = |key: &str| {
            projects
                .iter()
                .find(|p| p.project_key == key)
                .and_then(|p| p.path_hint.clone())
        };
        assert_eq!(hint("app-aaaaaaaaaaaa").as_deref(), Some("…/a/app"));
        assert_eq!(hint("app-bbbbbbbbbbbb").as_deref(), Some("…/b/app"));
        assert_eq!(hint("lib-cccccccccccc"), None);
        assert!(projects.iter().all(|p| p.active_count == 0));
        Ok(())
    }

    #[test]
    fn path_hint_replaces_home() {
        let home = Path::new("/home/synthetic");
        assert_eq!(
            path_hint(Path::new("/home/synthetic/x/y/app"), Some(home)).as_deref(),
            Some("~/…/y/app")
        );
        assert_eq!(
            path_hint(Path::new("/home/synthetic/app"), Some(home)).as_deref(),
            Some("~/app")
        );
        assert_eq!(
            path_hint(Path::new("/srv/y/app"), Some(home)).as_deref(),
            Some("…/y/app")
        );
    }

    #[test]
    fn label_falls_back_to_key_prefix() {
        assert_eq!(label_for(None, "my-app-0123456789ab"), "my-app");
        assert_eq!(label_for(None, "plain"), "plain");
        assert_eq!(
            label_for(Some(Path::new("/w/proj")), "x-0123456789ab"),
            "proj"
        );
        assert_eq!(label_for(None, ""), "Ohne Projekt");
    }

    #[test]
    fn page_cursor_walks_all_rows_once() -> TestResult {
        let temp = tempfile::tempdir()?;
        for n in 0..25 {
            write_session(temp.path(), &format!("sess-{n:02}"), &visible_spec())?;
        }
        let index = open(temp.path());
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = index.page(&PageParams {
                limit: Some(7),
                cursor: cursor.clone(),
                ..PageParams::default()
            })?;
            seen.extend(
                page.rows
                    .iter()
                    .map(|row| row.session_id.as_str().to_owned()),
            );
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen.len(), 25);
        let unique: HashSet<&String> = seen.iter().collect();
        assert_eq!(unique.len(), 25);
        let full: Vec<String> = all(&index)?
            .into_iter()
            .map(|row| row.session_id.as_str().to_owned())
            .collect();
        assert_eq!(seen, full);
        assert!(matches!(
            index.page(&PageParams {
                cursor: Some("garbage".to_owned()),
                ..PageParams::default()
            }),
            Err(HostError::Protocol(_))
        ));
        Ok(())
    }

    #[test]
    fn query_matches_title_and_first_message() -> TestResult {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        write_session(
            root,
            "sess-t",
            &Spec {
                title: Some("Alpha Titel"),
                ..visible_spec()
            },
        )?;
        write_session(
            root,
            "sess-m",
            &Spec {
                turns: 1,
                first: Some("enthält BETA hier"),
                ..Spec::default()
            },
        )?;
        let index = open(root);
        let find = |query: &str| -> Result<Vec<String>, HostError> {
            Ok(index
                .page(&PageParams {
                    query: Some(format!("  {query} ")),
                    ..PageParams::default()
                })?
                .rows
                .into_iter()
                .map(|row| row.session_id.as_str().to_owned())
                .collect())
        };
        assert_eq!(find("alpha")?, vec!["sess-t".to_owned()]);
        assert_eq!(find("beta")?, vec!["sess-m".to_owned()]);
        assert!(find("gamma")?.is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn workdir_rejects_symlink_and_unknown_root() -> TestResult {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let sessions = base.join("sessions");
        std::fs::create_dir_all(&sessions)?;
        let real = base.join("real");
        let sub = real.join("sub");
        let other = base.join("other");
        std::fs::create_dir_all(&sub)?;
        std::fs::create_dir_all(&other)?;
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link)?;
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let (real_s, sub_s, other_s, link_s) = (s(&real), s(&sub), s(&other), s(&link));

        write_session(
            &sessions,
            "sess-known",
            &Spec {
                project: Some((&real_s, "real-0123456789ab")),
                ..visible_spec()
            },
        )?;
        write_session(
            &sessions,
            "sess-link",
            &Spec {
                cwd: Some(&link_s),
                ..visible_spec()
            },
        )?;
        write_session(
            &sessions,
            "sess-other",
            &Spec {
                cwd: Some(&other_s),
                ..visible_spec()
            },
        )?;
        write_session(
            &sessions,
            "sess-sub",
            &Spec {
                cwd: Some(&sub_s),
                ..visible_spec()
            },
        )?;
        write_session(&sessions, "sess-none", &visible_spec())?;
        let index = open(&sessions);
        let wd = |id: &str| index.workdir(&SessionId::from_str(id));
        assert_eq!(wd("sess-known")?, Some(real.clone()));
        assert_eq!(wd("sess-link")?, None);
        assert_eq!(wd("sess-other")?, None);
        assert_eq!(wd("sess-sub")?, Some(sub));
        assert_eq!(wd("sess-none")?, None);
        assert_eq!(wd("sess-missing")?, None);
        assert_eq!(wd("../escape")?, None);
        Ok(())
    }

    #[test]
    fn share_roots_narrow_workdir() -> TestResult {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let sessions = base.join("sessions");
        std::fs::create_dir_all(&sessions)?;
        let p1 = base.join("p1");
        let p2 = base.join("p2");
        std::fs::create_dir_all(&p1)?;
        std::fs::create_dir_all(&p2)?;
        let (p1_s, p2_s) = (
            p1.to_string_lossy().into_owned(),
            p2.to_string_lossy().into_owned(),
        );
        write_session(
            &sessions,
            "sess-1",
            &Spec {
                project: Some((&p1_s, "p1-0123456789ab")),
                ..visible_spec()
            },
        )?;
        write_session(
            &sessions,
            "sess-2",
            &Spec {
                project: Some((&p2_s, "p2-0123456789ab")),
                ..visible_spec()
            },
        )?;
        let open_with = |roots: Vec<PathBuf>| {
            FsProfileIndex::open(
                sessions.clone(),
                IndexOptions {
                    since: None,
                    share_roots: roots,
                },
            )
        };
        let narrow = open_with(vec![p1.clone(), base.join("does-not-exist")]);
        assert_eq!(
            narrow.workdir(&SessionId::from_str("sess-1"))?,
            Some(p1.clone())
        );
        assert_eq!(narrow.workdir(&SessionId::from_str("sess-2"))?, None);
        assert!(narrow.resolve_project("p1-0123456789ab")?.is_some());
        assert!(narrow.resolve_project("p2-0123456789ab")?.is_none());
        // Nur ungültige Wurzeln: nichts erlaubt.
        let none = open_with(vec![base.join("does-not-exist")]);
        assert_eq!(none.workdir(&SessionId::from_str("sess-1"))?, None);
        // Ohne Einschränkung beide.
        let wide = open_with(Vec::new());
        let resolved = wide
            .resolve_project("p2-0123456789ab")?
            .ok_or("p2 unresolved")?;
        assert_eq!(resolved.project_root, p2);
        assert_eq!(resolved.label, "p2");
        assert!(wide.resolve_project("unknown-key")?.is_none());
        assert!(wide.resolve_project("")?.is_none());
        Ok(())
    }

    #[test]
    fn no_project_pseudo_key() -> TestResult {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        write_session(root, "sess-np", &visible_spec())?;
        write_session(
            root,
            "sess-p",
            &Spec {
                project: Some(("/w/proj", "proj-0123456789ab")),
                ..visible_spec()
            },
        )?;
        let index = open(root);
        let projects = index.projects(&ProjectsParams::default())?.projects;
        let none = projects
            .iter()
            .find(|p| p.project_key.is_empty())
            .ok_or("pseudo project missing")?;
        assert_eq!(none.label, "Ohne Projekt");
        assert_eq!(none.session_count, 1);
        let only_none = index.page(&PageParams {
            project_key: Some(String::new()),
            ..PageParams::default()
        })?;
        assert_eq!(only_none.rows.len(), 1);
        assert_eq!(only_none.rows[0].session_id.as_str(), "sess-np");
        let only_proj = index.page(&PageParams {
            project_key: Some("proj-0123456789ab".to_owned()),
            ..PageParams::default()
        })?;
        assert_eq!(only_proj.rows.len(), 1);
        assert_eq!(only_proj.rows[0].project_label.as_deref(), Some("proj"));
        assert_eq!(index.page(&PageParams::default())?.rows.len(), 2);
        Ok(())
    }

    #[test]
    fn missing_sessions_root_is_empty() -> TestResult {
        let temp = tempfile::tempdir()?;
        let index = open(&temp.path().join("absent"));
        assert!(all(&index)?.is_empty());
        assert!(index.newest(5)?.is_empty());
        Ok(())
    }
}
