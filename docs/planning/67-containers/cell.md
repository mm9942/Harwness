---
id: CONTAINERS-CELL
title: "Mode B: harw-Zelle (harw im Container mit lokal kompilierten Worker-Agenten)"
status: proposed
date: 2026-09-27
tags: [containers, cell, oci, placement, workers, dod]
related:
  - README.md
  - ../66-placement/README.md
  - ../65-cloud-sessions/README.md
  - ../70-decisions/DEC-003-provider-limits.md
  - ../../design/agent-definition-dsl.md
---

> **Idee (Mia):** Das Container-System übernimmt die Container-Verwaltung.
> harw selbst läuft im Container als kleine Einheit mit einem Haufen lokal
> sitzender, kompilierter Worker-Agenten, pro Auftrag erzeugt und danach
> bereinigt. Zellen, Familien und Clans waren von Anfang an Teil der Planung.
>
> Dieses Blatt ergänzt Mode A aus `README.md` (Host verwaltet Container als
> Job-Versuche) um Mode B (eine Zelle ist einer dieser Container). Der Abgleich
> mit Familie/Clan/Zelle/Organisation aus `agent-definition-dsl.md` §14–15 und
> Lücke G3 folgt als eigener Abschnitt.

# Mode B: harw-Zelle

**Answer.** The idea fits as a second mode on top of Mode A. In Mode A (67 §1–§8), the host harw starts, supervises and reaps containers as job attempts (`67-containers/README.md:44`). In Mode B, one of those ContainerInstances is a **cell**: a harw job worker and work driver, plus N compiled worker agents running as local processes. The cell is created for one job and removed once its results are exported. None of this exists yet:
- The repo has no Containerfile or Dockerfile.
- The crates `harw-container-model`, `harw-job-executor-oci`, `harw-placement` and `harw-dod-container` do not exist.
- No code mentions "cell".

Two binding rules shape the design:
- **DEC-003:** one provider client per process, and no worker may open its own (`DEC-003-provider-limits.md`, "Folgen").
- **DEC-004:** workers never build.

## B.1 Model
- **What a cell is:** a `ContainerInstance` carrying the extra label `harw.kind=cell` next to the labels in 67 §3 (`:79-86`).
- **Lifecycle:** create → claim (secrets as a tmpfs file) → run → export → drain → executor removes it. This maps onto Requested→…→Reaped (`:78`).
- **Process tree:** init as PID 1 → `harw cell run` → work driver → N worker processes.
- **`harw cell run` reuses the `harw serve` composition:** the job worker runs on its own runtime (`harw-cli/src/lib.rs:1383`) with a 10 s shutdown grace (`:1435`).
- **The gap:** all of today's workers run in-process. `JobTurnSpawner` shares one provider in the same process (`harw-cli/src/job_worker_work_driver.rs:60-62,2508-2524`). The only out-of-process spawner relaunches `current_exe()` (`harw-agent-runner/src/job_child_backend.rs:70-84`). Its protocol is private and assumes the same executable (`child_protocol.rs:24-27`), with an exact version check (`:514`). So sibling worker binaries need a new spawner (B2). A cell with zero worker binaries, using only in-process workers, stays valid as a fallback.
- **Relation to Mode A:** the host's OCI executor starts the cell as one job attempt. Placement sees an `oci-cell` RuntimeOffer, in addition to the kinds in `67:151`. The host sees the cell's internal fan-out only through leases (B.4).

## B.2 Image

| Stage | Content |
|---|---|
| Builder | Toolchain, `cargo fetch --locked`, a cargo-chef layer cache, and `harw agent build <role> --native` per worker role. This produces the Runner flavor (`harw-agent-compiler/src/backend/native.rs:20-31,112-151`) with the artifact embedded via `include_bytes!` (`:166`). That step runs `cargo build --release` (`:410-481`), so it is a build. It happens only here or in a verify cell. |
| Worker-cell runtime | harw, `/opt/harw/agents/<role>` binaries (digests go into the cell spec), tini, CA certs. No cargo, no rustc, no bwrap by default (B.3). Base image distroless or debian-slim, pinned by digest. |
| Verify cell | Toolchain, the workspace, and a target-cache volume. At most one per workspace: the ledger's `BuildSlot` capacity is 1 (`66-placement/README.md:203`), and `.harw/verify.lock` still applies (`DEC-004-no-parallel-builds.md:45`). Build settings come from `CLAUDE.md` and `.github/workflows/copilot-setup-steps.yml`. |

- The cell image is referenced only as `name@sha256:…` (`67:96`). Signatures and an SBOM attestation are optional (DEC-032).
- Warm pools are keyed by (tenant, digest, profile) (`67:95`, DEC-035).

## B.3 Inner isolation
- **Outer boundary:** the container itself, with the §5 options unchanged: rootless, `--cap-drop ALL`, `no-new-privileges`, read-only rootfs, network `none` (`67:122-123`).
- **Inner layer per worker, default:** the Landlock trampoline (`harw-job-exec/src/lib.rs:27`, `trampoline.rs:188`). The crate has no `unshare` call, so it works without user namespaces.
- **Why bwrap is not the default:**
  - bwrap always passes `--unshare-all --unshare-net`, plus `--unshare-user` when it sets uid/gid (`harw-sandbox/src/bwrap.rs:464-481`).
  - It also mounts a fresh `--proc` (`:490`).
  - Docker's default seccomp profile blocks unprivileged `CLONE_NEWUSER`. This comes from the research input (docs.docker.com seccomp page) and was not re-fetched here.
  - Therefore bwrap is opt-in only, through a narrow seccomp profile that allows only `CLONE_NEWUSER`; never `--privileged` or `seccomp=unconfined`.
  - The bwrap binary ships only with that profile. Otherwise the host probe would over-claim, because it counts bwrap whenever the binary exists and user namespaces are not known to be off (`harw-job-runtime/src/host.rs:257`).
- **Enforcement has two scopes:**
  - **Boundary (host ↔ cell):** taken from the OCI inspect read-back (`67:111-117`). Placement admits a cell only if the read-back meets every `Required` in the union of its agents' requirements (`66:103`).
  - **Inner (worker ↔ worker and ↔ orchestrator secrets):** every worker still runs `admission::check` (`harw-agent-runner/src/admission.rs:130`, `lib.rs:302`) against the probe taken inside the cell (`host.rs:222-268`).
  - `Required` stays never-overridable (`admission.rs:13,16`).
  - The launcher passes `--allow-degraded` (`args.rs:104`) only if the grant allows a BestEffort dimension to degrade. It is never baked into the image.
  - **Rule:** the outer boundary protects the host. It never upgrades an inner `Required`.
- **Per-worker limits:**
  - Enforced only with a writable, delegated cgroup v2 subtree (`host.rs:223`). That means rootless Podman with `--cgroupns=private` plus `Delegate=` on the host.
  - Default: the engine enforces cell-level limits (read-back), and per-worker limits are `Partial` (rlimits only). `kernel.cgroup_v2` is then admitted only as degraded.

## B.4 Shared limits across cells
- **Why a shared authority is needed:** account and gateway limits apply per account, but harw's buckets are per process (`75-harness-patterns/cost-model.md:276-281`). Workers AI frontier models allow 20 or 50 requests per minute per model and account (`gateway-contract.md:50-51`). N cells are N processes, so they must share through the ledger (DEC-023, `66:236`).
- **Inside a cell there is exactly one provider client: the orchestrator.**
  - Worker binaries reach the model only through a cell-local model relay over a Unix socket, and they hold no API key.
  - The runner builds its provider through `harw_runtime::EmbeddedAgent` (`harw-agent-runner/src/lib.rs:16,64`). **Not verified:** whether a relay `base_url` fits that path. B3 must check this.
- **Across cells:**
  - The ledger grants each cell a lease of k provider slots, and the cell's `max_concurrency` is k.
  - `effective_parallel` reads it (`66:195`), and the existing reserved slot still applies (`job_worker_work_driver.rs:164,377`).
  - Lease TTL follows DEC-024. Losing the lease means drain, then re-place (`66:150-155`).
  - The own Cloudflare Worker gateway is only a backstop: a 429 there means "wait" (`gateway-contract.md:57`). harw never auto-shards across several gateways (`cost-model.md:281`).
- **Cache affinity:** one cell uses one prefix-group key, `x-harw-cache-affinity` (`gateway-contract.md:60-66`), and only within one tenant (`66:209`). Placement scores cache warmth by prefix group (`66:268-276`). The header is planned only: grep finds no code for it.

## B.5 State and cleanup

| Where | What |
|---|---|
| Outside (host) | The job store holding the cell attempt: fs4 with a fenced epoch (`harw-session-store/src/job_store.rs:14,528`), on a local volume with a single writer, never NFS (`65-cloud-sessions/README.md:392,422`). Transcripts and approvals follow 65 §6 (`:390-396`). The KEK is never on a volume. The AuthHub socket `/run/harw/infra/secure.sock` (`harw-auth-hub/src/config.rs:72`, `SO_PEERCRED` check in `auth.rs:3`) stays on the host and is not mounted into cells (`67:112,122`). |
| Injected | Secrets are resolved on the host after the claim and injected as a tmpfs file (`67:118`, `66:207`). |
| Inside (per attempt) | An ephemeral `HARW_HOME`, plus the inner job store for durable worker sessions on a per-attempt local volume. Sockets and live state live on tmpfs. |
| Export | Results go to an `/out` volume or are pushed to a git branch before exit. The exit code is the job outcome. |

- **Do not use `docker run --rm`.** Mode A requires `RestartPolicy=no` and never `AutoRemove`, so the exit code survives a controller restart (`67:88`). The executor removes the cell after reading the exit code (`67:106`). Orphans are garbage-collected by labels, the `gc_grace` rule and the lease epoch (`67:89-92`).
- **Init:** `--init` or tini as PID 1 reaps worker zombies and forwards SIGTERM.
- **SIGTERM:** drain as in 65 §6 (`:398-404`): stop the current wave, interrupt turns, flush, export partial results, and exit within the grace period.
- **Crash:** the attempt becomes `Lost`, the retry policy runs, and placement re-places without that offer (`67:154`). Inner state survives only if the per-attempt volume can be reattached; otherwise the job restarts from the last export.

## B.6 Security
- Rootless is the default (DEC-030).
- No Docker or Podman socket inside a cell, because it is root-equivalent (`67:122`). Cells never start containers themselves, which extends the non-goal at `67:48`.
- Network is `none`. Egress goes only through a relay to the gateway (`67:119`), following the relay precedent in `bwrap.rs:23-26`.
- Read-only rootfs. Only `/workspace`, `/out`, `/tmp` (tmpfs) and the per-attempt state are writable.
- The API key lives only in the orchestrator's tmpfs file, never in labels or env (`67:87`).
- DoD:
  - `harw-dod-container` (C4) sees a cell as one `libpod-<id>.scope` (`67:136`).
  - Findings per worker need delegated sub-cgroups.
  - `harw.kind=cell` goes into the expected-instances set (`67:145`).
  - `FreezeCgroup` and `KillProcessTree` act on the whole cell (`67:147`).

## B.7 Roadmap (one agent per file; no new ring)

| Round | File (ring) | Depends | Tests |
|---|---|---|---|
| B0 | This section, DEC-036–041, and DEC-010 if not yet landed (`65:591`) | – | – |
| B1 | `harw-container-model/src/cell.rs` (I) | C1 | serde round trip; missing digest or plain tag refused |
| B2a | `harw-agent-runner/src/job_child_backend.rs`: `BinaryPathSpawner` (A) | – | wrong digest refused; protocol mismatch refused |
| B2b | `harw-cli/src/cell_spawner.rs`: `WorkerSpawner` implementation (A) | B2a | fake binary; wave width ≤ lease |
| B3 | `harw-cli/src/cell_model_relay.rs` (A) | – | N clients never exceed `max_concurrency`; 429 passed on as wait; bounded frames (M5) |
| B4 | `harw-cli/src/cell.rs`: `harw cell run` (A) | B2b, B3 (stub ledger; P4 for real leases) | SIGTERM drain; export before exit |
| B5 | `harw-job-executor-oci/src/cell.rs` + `offer.rs` (J) | C2, C3, B1; offers also need P1 | two-scope enforcement; no host `/run` mounts |
| B6 | `deploy/cell/{Containerfile,verify.Containerfile,seccomp-userns.json}`, `examples/cells/…` | B4, B5, C4 | – |

**Combined order:**
- **W0:** C0, P0, B0.
- **W1:** C1, P1, B2a, B3.
- **W2:** B1, C2, C4, C5 (all after C1); P2, P3 (after P1); B2b.
- **W3:** C3 (after C2); P4 (after P2 and P3); B4.
- **W4:** P5 (after P4); C6 (after P1, C2, C5); B5.
- **W5:** B6; P6 (any time after P1); P7 (after 65 RS2 and RS3).
- **Critical path:** C1→C2→C3→B5→B6. P1→P2/P3→P4 feeds B4's real ledger.

**Open decisions (after DEC-035):**
- **DEC-036:** "Cell" is a ContainerInstance with `harw.kind=cell`. It is distinct from the 65 host container and extends DEC-034.
- **DEC-037:** one provider client per cell. Workers use the relay only and hold no keys. Extends DEC-003.
- **DEC-038:** provider slots across cells are ledger leases. The gateway is a backstop. One prefix-group key per cell. Extends DEC-023 and DEC-026.
- **DEC-039:** enforcement has two scopes. The boundary never upgrades an inner `Required`. bwrap in a cell only with the narrow user-namespace seccomp profile. Extends DEC-027 and DEC-031.
- **DEC-040:** worker cells carry no toolchain. Native builds happen only in the image build stage or in a verify cell that holds the `BuildSlot`. Extends DEC-004.
- **DEC-041:** cell state follows 65 §6; no `AutoRemove`; results go to `/out` or git. Requires DEC-010.

## B.8 Abgleich mit Familie, Clan, Zelle, Organisation (DSL §14–15)

(`dsl.md` = `docs/design/agent-definition-dsl.md`)

**Antwort.** Die DSL-Zelle und die Container-Zelle aus B.1 sind nicht dasselbe.
- **DSL-Zelle (`[[cells]]`):** eine logische, run-lokale Fan-out-Welle über Plan-Knoten mit Schreibtrennung (`harw-agent-dsl/src/organization.rs:303-420`, Invariante 11 `dsl.md:1401`). Sie ist implementiert. Genutzt wird sie über `CellPlan::from_cell_nodes` (`harw-plan-bridge/src/cells.rs:176-229`), heute nur von `/analyze` (`harw-ops/src/analyze.rs:1642`).
- **Container-Zelle aus B.1:** eine Laufzeit-Hülle, also eine `ContainerInstance` mit `harw.kind=cell`. Die Aussage in B.1, kein Code erwähne „cell“, stimmt nur in diesem Sinn.

Vor B2b und B4 muss G3 geschlossen werden. Die heutige Disjunktheitsrelation erkennt keine Überlappungen mit Globs und ist bei ungültigen Pfaden fail-open. Außerdem liegt sie in Ring A, wo DSL (C) und Executor (J) sie nicht erreichen.

### B.8.1 Ein Vokabular (DEC-042)
- **Zelle** (ohne Zusatz) ist die DSL-Zelle. Sie legt fest, *wer* *gleichzeitig* an *welchen disjunkten* Schreibbereichen arbeitet.
- **Zellhost** ist der Mode-B-Container. Er ist *eine mögliche Laufzeit-Platzierung* von Clan-Wellen und trägt `harw.kind=cellhost`.
  - Das ändert nur die Wortwahl von DEC-036, nicht deren Inhalt.
  - Die Dateinamen in B.7 folgen dieser Entscheidung.

| DSL | Placement-Einheit | Container | Ledger-Lease | Schreibpartition | Provider-Client (DEC-003/037) |
|---|---|---|---|---|---|
| Organisation (eine Root, `organization.rs:11`) | keine; verengt nur den `PlacementGrant` (`66:82,104-107`) | keiner; die Root bleibt auf dem Host (Inv. 13, `dsl.md:1403`) | Eltern-Lease des Laufs; Clans ziehen Kind-Leases (`66:203`) | Clan-Selektoren paarweise disjunkt | Host |
| Familie (`family.rs`) | keine | Roster → Rollen-Binaries im Image (`dsl.md:913-923`, B.2) | – | Invariante `disjoint_write_sets` (`dsl.md:932`); heute ein freier String (`family.rs:131`), den kein Code auswertet | – |
| Clan (`plan_scope`, Leader) | **ja**: Leader plus seine Wellen | standardmäßig 1 Zellhost je Clan | eine Lease je Zellhost mit k Slots (B.4) | Clan-`WriteScope` ⊆ Clan-Selektor | genau einer: der Leader im Zellhost |
| Zelle | Teil der Clan-Einheit; die Breiteneinheit ist die `CellStage` (`cells.rs:320-325`) | läuft ganz in einem Zellhost | Stufenbreite ≤ k − Reserve | `DisjointSet` je Stufe | – |
| Worker | nein | ein Prozess | teilt die Slots | eigener `WriteScope`, umgesetzt als Landlock-`read_write` | Relay |

### B.8.2 G3: Lücke, Besitz, Reihenfolge

**Herkunft.**
- G3 steht in `docs/design/hardening-gap-analysis.md:79-95` (Priorität P3, `:401`).
- Die Idee zum „Borrow-Checker“ stammt aus `docs/design/agent-ir-v1.md:118-132`. Dort wird auch der Pass `PartitionWriteSets` vorgeschlagen (`:106`).
- Der Beleg `ids.rs:163 PathOrSymbol(String)` in der Gap-Analyse ist veraltet. Heute ist es ein Enum mit `Path` und `Symbol` (`harw-plan/src/ids.rs:318-333`).

**Stand im Code:**
- **Organisation:**
  - `plan_scope` und `members_from_plan` sind Strings (`organization.rs:181,350`).
  - `resolve_organization` prüft keine Überlappung (`:680-706`). Der Test `:973-986` löst `*-synthesis` und `security-*` fehlerfrei auf.
  - Die Default-Organisation enthält genau dieses Paar (`harw-registry-defaults/agents/organization/default.toml:81,108`). Ein Knoten `security-synthesis` gehört damit zu zwei Clans.
- **Partition:**
  - Sie ist greedy (`harw-plan/src/graph.rs:332-351`) und entscheidet über `write_scopes_conflict`. Das ist eine reine Präfix-Relation (`harw-plan/src/admission.rs:791-803`).
  - **Globs werden übersehen:** `src/*.rs` und `src/lib.rs` gelten als disjunkt. Die Admission macht aus `src/*.rs` aber eine Glob-Regel (`admission.rs:608-609`), die `src/lib.rs` zulässt.
  - **Fail-open:** Ist ein Pfad nicht normalisierbar, lautet das Ergebnis „kein Konflikt“ (`:792-794`). Der Test `:1283` hält dieses Verhalten fest.
- **Zweite, unabhängige Relation:** `paths_overlap` prüft `WorkScope.owned_paths: Vec<String>` (`harw-plan-bridge/src/work_driver.rs:170-180,1272-1294`). Auch `WorkerRequest.owned_paths` ist untypisiert (`harw-cli/src/job_worker_work_driver.rs:418`).
- **Durchsetzung erst nachträglich:**
  - Sie läuft über Schnappschuss und Diff; es gibt „keine pfadgenaue Sandbox“ (`job_worker_work_driver.rs:46-58`).
  - Die Zuordnung erfolgt nach Besitz, nicht nach Schreiber (`:1961-1990`). Schreibt ein Worker in den Pfad eines Geschwisters, fällt das nicht auf.
- **IR:** Sie kennt nur `filesystem_write: bool` (`ir_v2.rs:1074`). Für ORG gibt es keine Diagnosecodes (`diagnostics.rs:101`).
- **Vorbild:** `AuthorityCeiling::is_disjoint_from` (`harw-agent-dsl/src/authority.rs:155`, genutzt in `family.rs:1536`).

**Besitz.**
- `harw-plan` liegt in Ring A (`xtask/arch-policy.toml:333-334`), `harw-agent-dsl` in C (`:105`), und C darf nur auf F, I und C zeigen (`:47`). Die Algebra gehört deshalb nach Ring I, als `harw-authority/src/write_scope.rs` (`:113-114`).
- Dort liegt sie neben `NetworkScope`, das schon `intersection` und `is_subset_of` hat (`harw-authority/src/lib.rs:320-402`).
- Die DSL erreicht `harw-authority` bereits über `harw-context` (`harw-agent-dsl/Cargo.toml:13`, `harw-context/Cargo.toml:12`). Die Ringe J, D und A dürfen I ohnehin nutzen (`:46,50,51`).

**Warum vor B2b/B4.**
- Im Zellhost teilen sich N Worker-Prozesse ein `/workspace` (B.5).
- Die einzige harte Schranke ist Landlock je Prozess (B.3). Dessen Policy kennt nur Pfad-Hierarchien, keine Globs (`harw-job-linux/src/sandbox.rs:60-61`).
- Die Disjunktheit muss also genau auf dieser Form entschieden werden. Sonst vergibt B2b unbemerkt überlappende Schreibrechte, und B4 exportiert einen vermischten Stand.

**Typ-Skizze (DEC-043):**
```rust
// harw-authority/src/write_scope.rs (Ring I)
pub struct RepoPath(String);                             // normalisiert; `..`/absolut/leer ⇒ Err
pub enum WriteRoot { File(RepoPath), Subtree(RepoPath) } // Landlock-fähig, keine Globs
pub struct WriteScope(BTreeSet<WriteRoot>);              // leer = nur lesen
impl WriteScope {
    pub fn parse(e: &[&str]) -> Result<Self, ScopeError>;    // Glob ⇒ ScopeError::Glob
    pub fn intersects(&self, o: &Self) -> bool;
    pub fn is_subset_of(&self, ceiling: &Self) -> bool;
    pub fn roots(&self) -> impl Iterator<Item = &WriteRoot>; // → FilesystemPolicy.read_write
}
pub struct DisjointSet<K>(Vec<(K, WriteScope)>);         // Beweis-Token, nur über prove
impl<K: Ord + Clone> DisjointSet<K> {
    pub fn prove(v: Vec<(K, WriteScope)>) -> Result<Self, Overlap<K>>;
}
pub enum PlanSelector { Exact(String), Prefix(String), Glob(String) } // Auswahl, kein Schreibrecht
impl PlanSelector { pub fn may_overlap(&self, o: &Self) -> bool; }     // unentscheidbar ⇒ true
```

**Prüfung an drei Stellen (DEC-044):**
1. Beim DSL-Validate werden die Clan-Selektoren paarweise geprüft (neuer `HARW-ORG`-Code).
2. Bei der Partition wird `CellPlan.batches` zu `Vec<DisjointSet<TaskId>>`.
3. Bei der Zellhost-Admission wird vor jeder Stufe geprüft, gegen die Stufe und gegen die laufenden Worker (`work_driver.rs:1296`). Danach gilt `roots()` als Landlock-Regel.

### B.8.3 Konflikte zwischen B.1–B.7 und §14–15
1. **Name:** siehe B.8.1.
2. **Fan-out-Breite (DEC-046):**
   - Heute berechnen drei Stellen die Breite unterschiedlich:
     - DSL: `min(Batch, max_parallel)` (`cells.rs:366-381`).
     - Work-Driver: `min(spec, cap−1)` (`job_worker_work_driver.rs:374-380`).
     - Zellhost: k aus der Lease (B.4).
   - **Regel:** Breite = min(Partition-Batch, `max_parallel_workers`, k − `RESERVED_PROVIDER_SLOTS`).
   - Die DSL begrenzt, *wer* zusammen läuft, der Ledger, *wie viele*. Keine Seite erweitert die andere.
3. **Wo `required` gilt (DEC-044):**
   - Heute nur in `cells.rs:200-209` und nur für `/analyze`. Der Work-Driver nutzt keine Org-Zellen.
   - Fehlt die Organisation oder die Zelle, läuft die Welle ungeteilt (`cells.rs:51-55`, `analyze.rs:1561-1572`). Das ist fail-open.
   - B.1–B.7 erwähnen `write_partition` nicht.
   - **Regel:** Im Zellhost gilt `advisory`/`none` wie `required`, sobald mehr als ein Mitglied schreibt. Einen Rückfall auf die ungeteilte Welle gibt es dort nicht.
4. **Clans und Zellhosts (DEC-045):**
   - Die DSL sieht alle Clans in *einem* Root-Lauf (`dsl.md:1036-1038`). B.1 sieht einen Container je Job und sagt nichts zu Clans.
   - **Regel:**
     - Standard ist ein Zellhost je Clan.
     - Der Leader, ein Child-Orchestrator (`default.toml:30-34`), ist dort der einzige Provider-Client.
     - Ein Clan läuft nie über mehrere Zellhosts auf demselben Workspace.
     - Die Rollen im Image kommen aus dem Familien-Roster.
   - **Befund:** Bei der Security-Familie ist `[orchestrators].allowed` leer (`default.toml:97-99`).
5. **Organisation und Placement-Policy (DEC-047):**
   - Eine Organisation wird als „organization revision“ eingefroren (`dsl.md:1226`).
   - Sie verengt den Grant nur (`meet`, Inv. 7 `dsl.md:1397`, DEC-022) und trägt keine Topologie (DEC-020). `child_depth_cost` ist schon so eine Verengung (`organization.rs:195-236`).
   - Leases und Fencing gehören der Laufzeit (Inv. 24, `dsl.md:1414`). Das passt zu DEC-038.

### B.8.4 Gemeinsame Roadmap (eine Datei je Schritt)

| Schritt | Datei (Ring) | Hängt ab | Test |
|---|---|---|---|
| G0 | dieser Abschnitt, DEC-042–047 | – | – |
| G1 | `harw-authority/src/write_scope.rs` (I) | – | Property: `prove` ok ⇒ jeder Pfad in ≤1 Scope; Glob/`..`/absolut abgelehnt; Serde |
| G2 | `harw-plan/src/admission.rs` (A): `conflicts` delegiert an `WriteScope`; Glob oder ungültiger Pfad ⇒ Konflikt | G1 | `src/*.rs` gegen `src/lib.rs` ⇒ Konflikt; `admission.rs:1283` umdrehen |
| G3 | `harw-plan-bridge/src/work_driver.rs` (A) | G1 | Fälle `:2036-2041` bleiben grün |
| G4 | `harw-agent-dsl/src/organization.rs` (C) + ORG-Code | G1 | überlappende Clans ⇒ Fehler |
| G5 | `harw-registry-defaults/agents/organization/default.toml` (A), in derselben Runde wie G4 | G4 | Default-Organisation löst auf |
| G6 | `harw-plan-bridge/src/cells.rs` (A) | G2 | Glob-Knoten teilen nie einen Batch; kein ungeteilter Rückfall bei Schreibern |
| B1 | `harw-container-model/src/cell.rs` (I) + `write_scope` | C1, G1 | Scope ⊆ Clan-Selektor |
| B2b | `harw-cli/src/cell_spawner.rs` (A) | B2a, G6 | Fake-Binary kann nicht in den Pfad des Geschwisters schreiben |
| B4 | `harw-cli/src/cell.rs` (A) | B2b, B3, G6 | überlappende Stufe wird vor dem Spawn abgelehnt |
| B5 | `harw-job-executor-oci/src/cell.rs` (J) | C2, C3, B1 | Read-back: nur Scope-Wurzeln sind `rw` |
| P1 | `harw-placement-model/src/requirements.rs` (I): `WriteNeed` trägt `WriteScope` (66) | G1 | `meet` weitet nie |
| P4 | `harw-placement/src/lease.rs` (A): überlappende Workspace-Claims ⇒ `CapacityShort` | P2, P3, G1 | zwei Claims |

**Unabhängig von G3:** C0–C6, P0, P2, P3, P5–P7, B0, B2a und B3. B6 hängt nur transitiv davon ab.

**Wellen:**
- W0: G0, C0, P0, B0
- W1: G1, C1, B2a, B3
- W2: G2, G3, G4, G5, P1, B1, C2, C4, C5
- W3: G6, P2, P3, C3
- W4: B2b, P4
- W5: B4, P5, C6
- W6: B5, P6
- W7: B6, P7

**Kritischer Pfad:** G1→G2→G6→B2b→B4→B6, parallel dazu C1→C2→C3→B5.

### Offene Entscheidungen (B.8)
- **DEC-042:** Heißt es „Zellhost“ (`harw.kind=cellhost`), oder bleibt es bei „Container-Zelle“ als Pflichtzusatz?
- **DEC-043:** Kommt die Algebra als Modul in `harw-authority` oder in ein eigenes I-Crate `harw-scope`? Werden Globs in `write_scope` verboten oder konservativ als Konflikt gewertet?
- **DEC-044:** Symbol-Scopes bleiben dateiweit (`ids.rs:283-286`). Wenn Landlock im Zellhost fehlt: nur noch ein Schreiber je Stufe?
- **DEC-045:** Mehrere Zellhosts auf einem Workspace: disjunkte `rw`-Mounts auf einem Volume oder ein Klon je Zellhost mit Merge-Barriere? Für die Default-Organisation: `*-synthesis` in `synthesis-*` umbenennen (das betrifft die Knotennamen in `/analyze`, `default.toml:19-20`) oder eine Vorrangregel einführen?
- **DEC-046:** Zählt der Leader-Turn gegen k, oder bekommt er einen eigenen Slot?
- **DEC-047:** Kommen `[clans.placement]`-Hinweise (nur verengend, ⊆ Grant) zusammen mit P6 oder später?
