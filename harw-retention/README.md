# harw-retention

Retention core for ephemeral data (logs, caches, spools). Layer `I`
(shared infrastructure); depends only on `harw-macros` and `serde`.

There is no scheduler, CLI or job kind here. Later waves plug into this
API: the maintenance job calls `ResolvedClass::sweep` per class, the CLI
and doctor read `CLASSES` / `resolve_all` / `doctor_warning`.

## Model

* **Sweep** (`sweep`): one directory, non-recursive. Plan first (no
  deletes), enforce **age -> count -> bytes**, the `keep_newest` newest
  files are exempt from every rule. Unlink only; never follows symlinks
  (`lstat`; symlinks and non-regular files are skipped, a symlinked sweep
  directory is refused); skips `*.lock` and dot-files. Each delete is
  independent and idempotent (a vanished file is not an error); the
  deadline is checked before every delete and a timed-out sweep returns the
  partial `Report` with `timed_out = true`, leaving the rest untouched.
* **Class**: one kind of ephemeral data, declared once in
  `src/classes.rs` through `harw_macros::retention_classes!`. The macro
  derives the `[retention]` config struct (`RetentionConfig`, serde
  defaults, `deny_unknown_fields`), the `CLASSES` registry, `policy_for` and
  `resolve_all` from that one list.
* **Kinds**: `Ephemeral` classes are on by default with default limits.
  `SecurityRelevant` classes are **opt-in**: default `enabled = false`
  (never deleted; dry runs only report) and `Apply` is refused unless
  config says `enabled = true` (`explicit_optin`).

Initial classes. Ephemeral: `tui_log`, `telemetry_rotated`, `job_logs`,
`bug_reports`, `scan_reports`. Security-relevant: `dod_spool`,
`sentinel_export`, `freeze_resolved`, `session_transcripts`,
`session_corrupt_backups`. Adding a class means adding one block to
`retention_classes!`; the config key, registry row, doctor warning and
sweep job follow automatically.

## Config (`[retention]`, in `harw-config`)

```toml
[retention.telemetry_rotated]
max_age_secs = 259200      # override one limit; the others keep class defaults
[retention.dod_spool]
enabled = true             # explicit opt-in for a security-relevant class
max_files = 5000
```

Keys per class table: `enabled`, `max_age_secs`, `max_bytes`, `max_files`,
`keep_newest` (all optional; `0` limits are rejected by `validate()`).
Merge: trusted layers (home, profile) replace; an untrusted project layer
can only lower `max_*` and cannot set `enabled` or `keep_newest`.

## Frozen API

```rust
// policy
pub struct NameMatch { pub prefix: Option<Cow<'static, str>>,
                       pub suffix: Option<Cow<'static, str>>,
                       pub contains: Option<Cow<'static, str>> }
impl NameMatch { pub const fn any() -> Self;
                 pub const fn prefix(&'static str) -> Self;
                 pub const fn suffix(&'static str) -> Self;
                 pub const fn contains(&'static str) -> Self;
                 pub const fn prefix_suffix(&'static str, &'static str) -> Self;
                 pub fn matches(&self, name: &str) -> bool; }

pub struct RetentionPolicy { pub max_age: Option<Duration>, pub max_bytes: Option<u64>,
    pub max_files: Option<usize>, pub keep_newest: usize, pub name_match: NameMatch,
    pub dry_run: bool, pub deadline: Option<Instant> }
impl RetentionPolicy { pub const fn unlimited() -> Self; }   // keep_newest = 1

pub enum RemovalReason { Age, Count, Bytes }                 // as_str()
pub struct Removal { pub path: PathBuf, pub bytes: u64, pub reason: RemovalReason }
pub struct ItemError { pub path: PathBuf, pub message: String }
pub struct Report { pub removed: Vec<Removal>, pub kept: usize, pub kept_bytes: u64,
    pub skipped: usize, pub errors: Vec<ItemError>, pub timed_out: bool, pub dry_run: bool }
impl Report { pub fn removed_bytes(&self) -> u64; }
pub enum SweepMode { DryRun, Apply }
pub enum RetentionError { Io { path, source }, NotADirectory(PathBuf),
    OptInRequired(&'static str), Disabled(&'static str), UnknownClass(String) }

// sweep
pub fn sweep(dir: &Path, policy: &RetentionPolicy, now: SystemTime)
    -> Result<Report, RetentionError>;                       // honours policy.dry_run

// classes
pub enum ClassKind { Ephemeral, SecurityRelevant }           // default_enabled(), as_str()
pub struct ClassDefaults { pub max_age_secs: Option<u64>, pub max_bytes: Option<u64>,
    pub max_files: Option<u64>, pub keep_newest: u64 }
pub struct Roots { pub home: PathBuf, pub project: Option<PathBuf> }
pub type DirResolver = fn(&Roots) -> Vec<PathBuf>;
pub struct Class { pub id: &'static str, pub kind: ClassKind, pub name_match: NameMatch,
    pub defaults: ClassDefaults, pub resolve_dirs: DirResolver }
pub struct ClassConfig { pub enabled: Option<bool>, pub max_age_secs: Option<u64>,
    pub max_bytes: Option<u64>, pub max_files: Option<u64>, pub keep_newest: Option<u64> }
impl ClassConfig { pub fn validate(&self) -> Result<(), String>; pub fn is_unset(&self) -> bool; }
pub const CLASS_CONFIG_FIELDS: [&str; 5];
pub struct ResolvedClass { pub class: &'static Class, pub enabled: bool, pub explicit_optin: bool,
    pub max_age_secs: Option<u64>, pub max_bytes: Option<u64>, pub max_files: Option<u64>,
    pub keep_newest: u64 }
impl ResolvedClass {
    pub fn resolve(&'static Class, &ClassConfig) -> Self;
    pub fn policy(&self, SweepMode, Option<Instant>) -> Result<RetentionPolicy, RetentionError>;
    pub fn doctor_warning(&self) -> Option<String>;
    pub fn sweep(&self, &Roots, SweepMode, Option<Instant>, SystemTime)
        -> Result<Vec<DirOutcome>, RetentionError>;          // one outcome per resolved dir
}
pub struct DirOutcome { pub dir: PathBuf, pub result: Result<Report, RetentionError> }

// generated by retention_classes! (re-exported at the crate root)
pub struct RetentionConfig { pub <class_id>: ClassConfig, .. }   // serde, deny_unknown_fields
impl RetentionConfig { pub fn class_config(&self, &str) -> Option<&ClassConfig>;
                       pub fn class_config_mut(&mut self, &str) -> Option<&mut ClassConfig>;
                       pub fn validate(&self) -> Result<(), String>; }
pub static CLASSES: &[Class];
pub fn policy_for(&RetentionConfig, id: &str) -> Option<ResolvedClass>;
pub fn resolve_all(&RetentionConfig) -> Vec<ResolvedClass>;
```
