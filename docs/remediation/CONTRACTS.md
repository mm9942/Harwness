# Remediation – Verträge

Status: **eingefroren nach Commit W0b** (Änderungen nur durch den Orchestrator zwischen Wellen).
Plan: `~/.claude/plans/eventual-wandering-pebble.md`. Ledger: `docs/remediation/ledger/<welle>/<agent>.md`.

Signaturen sind verbindlich. Wo „additiv“ steht, dürfen spätere Wellen ergänzen, aber nichts entfernen/umbenennen.

## §fsutil – `harw-fsutil` (neu, L0, einzige Abhängigkeit `rustix` mit Features `fs`, `process`)

```rust
pub struct OpenMode { pub read: bool, pub write: bool, pub create: bool, pub create_new: bool,
                      pub truncate: bool, pub append: bool, pub mode: u32 }
impl OpenMode { pub const fn read_only() -> Self; }            // additiv: weitere Konstruktoren erlaubt

/// Öffnet `path`; das letzte Pfadglied darf kein Symlink sein (O_NOFOLLOW über rustix, plattformkorrekt).
pub fn open_nofollow(path: &Path, mode: OpenMode) -> io::Result<File>;
pub fn open_dir_nofollow(path: &Path) -> io::Result<OwnedFd>;
/// openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS); bei ENOSYS komponentenweises openat(O_NOFOLLOW).
pub fn open_beneath(root: BorrowedFd<'_>, rel: &Path, mode: OpenMode) -> io::Result<File>;

pub struct AtomicWriteOptions { pub mode: u32, pub fsync_dir: bool }
/// Tempdatei im Zielverzeichnis (NAME_MAX-sicherer Name, create_new, 0600→mode), fsync, rename; Ziel-Symlink wird ersetzt, nie gefolgt.
pub fn write_atomic(path: &Path, bytes: &[u8], opts: AtomicWriteOptions) -> io::Result<()>;
/// Regulär, Eigentümer == effektive UID, `mode & 0o077 == 0`.
pub fn ensure_private_regular(file: &File) -> io::Result<()>;

pub struct WalkLimits { pub max_depth: usize, pub max_entries: usize, pub deadline: Option<Instant> }
pub enum EntryType { File, Dir, Symlink, Other }
pub struct WalkEntry { pub rel_path: PathBuf, pub entry_type: EntryType, pub len: u64 }
pub enum WalkStop { DepthLimit, EntryLimit, Deadline }
/// Folgt nie Symlinks (meldet sie als `EntryType::Symlink`), (dev,ino)-Zyklenschutz.
pub fn walk_beneath(root: &Path, limits: WalkLimits) -> io::Result<WalkBeneath>;
// impl Iterator<Item = io::Result<WalkEntry>> for WalkBeneath; impl WalkBeneath { pub fn stopped(&self) -> Option<WalkStop>; }
```

## §egress – `harw-sandbox::egress` (L1, `url` 2.5.8)

```rust
pub struct EgressUrl { /* url::Url + normalisierter Host + Port */ }
pub enum EgressHost { Domain(String), Ip(std::net::IpAddr) }
pub enum EgressUrlError { Parse, UnsupportedScheme(String), UserinfoPresent, MissingHost, InvalidHost }
impl EgressUrl {
    pub fn parse(input: &str) -> Result<Self, EgressUrlError>;   // nur http/https, keine Userinfo, IDNA→Punycode, trailing dot entfernt, lowercase
    pub fn as_url(&self) -> &url::Url;
    pub fn host(&self) -> &EgressHost;
    pub fn host_str(&self) -> String;
    pub fn port(&self) -> u16;
    pub fn is_https(&self) -> bool;
}
impl EgressHost { pub fn is_loopback(&self) -> bool; pub fn is_private_or_special(&self) -> bool; }
/// `allowed = "docs.rs"` matcht `docs.rs` und `*.docs.rs`, nie `notdocs.rs`.
pub fn host_matches_suffix(allowed: &str, host: &str) -> bool;
```
`harw_tools::host_from_url(&str) -> Option<String>` behält Signatur (Makro-Codegen `harw-macros/src/tool.rs:449`) und delegiert an `EgressUrl::parse`.

## §principal – `harw-types::principal` (L0)

```rust
// PermissionTier zieht aus harw-operations/src/operation.rs:112 nach harw-types (identische Varianten, Derives
// Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord + Hash + serde Serialize/Deserialize, snake_case);
// harw-operations behält `pub use harw_types::PermissionTier;`.
pub enum PrincipalKind { Human, Model, Operation, Channel }
pub enum IngressSurface { Tui, Cli, Web, Mcp, Telegram, Gateway, JobWorker, Child }
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]   // bewusst KEIN Deserialize
pub struct Principal { kind: PrincipalKind, id: String, surface: IngressSurface, tier: PermissionTier }
impl Principal {
    pub fn trusted_ingress(kind: PrincipalKind, id: impl Into<String>, surface: IngressSurface, tier: PermissionTier) -> Self;
    pub fn kind(&self) -> PrincipalKind;
    pub fn id(&self) -> &str;
    pub fn surface(&self) -> IngressSurface;
    pub fn tier(&self) -> PermissionTier;
    pub fn child_of(&self, role: &str) -> Principal;          // kind Model, surface Child, tier = min(parent, Operator)
    pub fn approval_actor(&self) -> Option<ApprovalActor>;    // Human+Tui→"local-tui", Human+Cli→"local-cli", Human+Web→"owner", Channel+Mcp→Operator{id}, sonst None
}
```

## §runtime-spec – `harw-runtime::spec` / `::error` (L12)

```rust
pub enum EntryKind { Tui, OneShot, LocalEcho, Analyze, Doctor, Web, McpServe, JobPrompt, JobPlanNode, GatewayTelegram, GatewayDream }
pub enum AskResolution { Interactive, RejectTurn, BlockJob, Fail }
pub enum SpawnerPolicy { None, BuiltinRoles }
pub enum CeilingPolicy { LocalRoot, Closed }
pub enum OperationSurface { AllWithModelTools, CommandsOnly, None }
pub struct EntryProfile { pub permissions: PermissionSet, pub registry_profile: RegistryProfile, pub operations: OperationSurface,
                          pub ask: AskResolution, pub spawner: SpawnerPolicy, pub ceiling: CeilingPolicy }
impl EntryKind { pub fn profile(self) -> EntryProfile }   // EINZIGE Reduktionstabelle (siehe Tabelle unten)
pub struct RootBudget { pub max_model_rounds: u32, pub max_total_tokens: u64, pub max_wall: std::time::Duration }
pub struct RuntimeSpec { pub entry: EntryKind, pub home: PathBuf, pub cwd: PathBuf, pub principal: Principal,
                         pub mode_override: Option<InteractionMode>, pub active_agent: Option<String>,
                         pub reasoning_effort: Option<ReasoningEffort> }
pub struct RightsSnapshot { pub entry: EntryKind, pub principal: Principal, pub approval_actor: Option<ApprovalActor>,
    pub permissions: Vec<String>, pub tools: Vec<String>, pub approval_chain: Vec<(&'static str, ApprovalHandlerKind)>,
    pub config_policy_tools: Vec<String>, pub ceiling_sections: Vec<String>, pub spawner_roles: Vec<String>,
    pub budget: RootBudget, pub untrusted_repo: Option<PathBuf> }
#[derive(HarwError)] pub enum RuntimeError { Config, Trust, Discovery, Registry, Sandbox, Provider, Store, Spawner }
```

| Entry | Rechte | Registry / Ops | Ask | Spawner | Decke |
|---|---|---|---|---|---|
| Tui | {ReadWorkspace, WriteWorkspace, ExecuteProcess} | Full + AllWithModelTools | Interactive | BuiltinRoles | LocalRoot |
| OneShot | {R, W, X} | Full + AllWithModelTools | RejectTurn | BuiltinRoles | LocalRoot |
| LocalEcho | {R, W, X} | Full + None | Fail | None | LocalRoot |
| Analyze | {R, W, X} | Full + CommandsOnly | Fail | BuiltinRoles | LocalRoot |
| Doctor | wie Tui | wie Tui | Fail | None | LocalRoot |
| Web | nach Tier (W2B-02 `permissions_for_tier`; Spec: {R}) | Full + CommandsOnly | Fail | None | Closed |
| McpServe | {} | NoTools + None | BlockJob | None | Closed |
| JobPrompt | {} | NoTools + None | BlockJob | None | Closed |
| JobPlanNode | ≤ {R, W} (aus Plan-Knoten-Vertrag) | Full + None | Fail | None | LocalRoot |
| GatewayTelegram / GatewayDream | {} | NoTools + None | Fail | None | Closed |

Netz überall leer bis Welle W5 (P1.7).

## §extension – additive Verträge (`harw-extension-api`, `harw-core::activation`)

```rust
pub enum ApprovalHandlerKind { ConfigPolicy, DefaultPolicy, Interactive, Channel, Durable, Other }
trait ApprovalHandler { /* bestehend */ fn kind(&self) -> ApprovalHandlerKind { ApprovalHandlerKind::Other } fn label(&self) -> &'static str { "unnamed" } }
// Doku-Vertrag: `ApprovalHandler::review` ist seiteneffektfrei (öffnet keine Prompts, schreibt nichts).
#[derive(Clone)] pub struct ApprovalModeCell(/* Arc<RwLock<ApprovalMode>> */);
impl ApprovalModeCell { pub fn new(mode: ApprovalMode) -> Self; pub fn get(&self) -> ApprovalMode; pub fn set(&self, mode: ApprovalMode);
                        pub fn detached(&self) -> Self; /* neue unabhängige Zelle mit aktuellem Wert */ }
impl ExtensionRegistry { pub fn into_builder(self) -> ExtensionRegistryBuilder; }   // erhält Namespace-Map
impl SessionActivation { pub fn intersect(&self, other: &Self) -> Self; }            // monoton: Ergebnis ⊆ beide
```
