# C2b — Secret-Resolver für die Gateway-`RuntimeAssembly`

Rolle: focused-coding-task-agent C2b, Welle W2d-1/C, Remediation
`/home/mia/projects/harwness`. Nur Lesebefehle ausgeführt (`Read`, `grep`);
**kein** `cargo build/check/test/clippy/run/add`, kein `make`, `rustc`,
`rust-analyzer`, keine `git`-Schreibbefehle (Ausnahme: keine
`cargo metadata`-Läufe nötig, da `harw_runtime`/`harw_provider_http` bereits
in `harw-cli`s bestehenden Imports als Abhängigkeiten belegt sind — siehe
`harw-cli/src/secret_store.rs:10`, `harw-cli/src/gateway.rs:394/413`).
Verifikation ausschließlich durch Lesen (Signaturen, Imports, Fehlerpfade).

## Geänderte Dateien

- `harw-cli/src/runtime_gateway.rs` — Moduldoku, `gateway_assembly`-Signatur
  und -Körper, ein Testaufruf.
- `harw-cli/src/gateway.rs` — nur Moduldoku-Zeile ~7 und `mount_gateway_assembly`
  (Vertrag eingehalten; `run`, `supervise`, alles andere unangetastet).

## Befund (Regression B3)

`gateway_assembly` baute die Montage über `crate::runtime_entry::build_assembly`,
die keinen `secret_resolver`-Parameter kennt. Damit lief `ModelSource::Configured`
im Gateway ohne Secret-Resolver — `secrets:`-Provider-Credentials (versiegelter
Store) konnten dort nicht mehr aufgelöst werden, während `env:`/Klartext-
Referenzen weiter funktionierten. B3s eigenes Ledger benennt das explizit als
offene Annahme/Risiko (`docs/remediation/ledger/W2d1/B3.md`, Abschnitt „Offen /
Hinweise für Parent“).

## Fix

### 1. `runtime_gateway::gateway_assembly` (Spec 1)

Neuer letzter Parameter:

```rust
pub(crate) fn gateway_assembly(
    entry: GatewayEntry,
    home: &Path,
    cwd: &Path,
    principal: Principal,
    state_store: Arc<dyn StateStore>,
    secret_resolver: Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>>,
) -> Result<RuntimeAssembly, String>
```

Baut `spec`/`stores` unverändert wie zuvor, verwendet aber **nicht** mehr
`crate::runtime_entry::build_assembly`, sondern den Builder direkt (Parallel-
Vertrag mit C2a, `harw-runtime/src/assembly.rs`):

```rust
let mut builder = RuntimeAssembly::builder(spec)
    .model(ModelSource::Configured)
    .stores(stores);
if let Some(resolver) = secret_resolver {
    builder = builder.secret_resolver(resolver);
}
builder.build().map_err(|error| format!("gateway: {error}"))
```

`RuntimeAssemblyBuilder::secret_resolver(self, resolver: Arc<dyn
harw_provider_http::SecretResolver + Send + Sync>) -> Self` exakt wie im
Parallel-Vertrag übergeben und in C2as eigenem Ledger (`ledger/W2d1/C2a.md`,
Abschnitt „Exakte neue Signatur“) bestätigt. Kein Echo-Fallback (G-048)
bleibt erhalten: jeder `RuntimeAssemblyBuilder::build`-Fehler läuft
unverändert mit Präfix `"gateway: "` durch.

Test `test_gateway_assembly_without_provider_config_returns_err_not_echo`
angepasst: zusätzliches `None`-Argument am Aufrufende.

### 2. `gateway::mount_gateway_assembly` (Spec 2)

Problem: die für die Montage maßgebliche Konfiguration entsteht erst
*innerhalb* von `RuntimeAssemblyBuilder::build` (`load_config(&spec)` als
erster Schritt, `harw-runtime/src/assembly.rs:462`), aber
`crate::secret_store::open_configured_secret_resolver` braucht eine bereits
aufgelöste `&ResolvedConfig`, um zu prüfen, ob ein aktivierter Provider
überhaupt `secrets:` nutzt.

Lösung wie im Brief vorgegeben: vorab dieselbe, vertrauensbewusste
Konfiguration ein zweites Mal laden, nur um den Resolver zu öffnen:

```rust
let (preliminary_config, _trust_report) = harw_runtime::load_config(
    &crate::runtime_entry::runtime_spec(
        harw_runtime::EntryKind::GatewayTelegram,
        home,
        cwd,
        principal.clone(),
    ),
)
.map_err(|error| format!("gateway: {error}"))?;
let secret_resolver: Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>> =
    match crate::secret_store::open_configured_secret_resolver(home, &preliminary_config) {
        Ok(Some(resolver)) => {
            Some(Arc::new(resolver) as Arc<dyn harw_provider_http::SecretResolver + Send + Sync>)
        }
        Ok(None) => None,
        Err(error) => return Err(format!("gateway: {error}")),
    };

let assembly = gateway_assembly(
    GatewayEntry::Telegram,
    home,
    cwd,
    principal,
    state_store,
    secret_resolver,
)?;
```

`harw_runtime::load_config`/`harw_runtime::EntryKind` sind Top-Level-Re-Exports
(`harw-runtime/src/lib.rs:29,37-40`: `pub use config::{ConfigTrustReport,
load_config};` bzw. `pub use spec::{.., EntryKind, ..};`), signaturgeprüft
gegen `harw-runtime/src/config.rs:90`: `pub fn load_config(spec: &RuntimeSpec)
-> RuntimeResult<(ResolvedConfig, ConfigTrustReport)>` — `RuntimeResult<T> =
Result<T, RuntimeError>`, `RuntimeError: Display`, daher `format!("gateway:
{error}")` gültig. `principal.clone()` für die vorläufige Spec, das
Original-`principal` wandert unverändert (kein Doppel-Move) in
`gateway_assembly`.

Kein neuer `use`-Import nötig: `harw_runtime`/`harw_provider_http` sind bereits
Crate-Abhängigkeiten von `harw-cli` (u. a. `harw-cli/src/secret_store.rs:10`,
`harw-cli/src/gateway.rs:394,413` referenzieren beide Crates bereits
vollqualifiziert) — konsistent mit dem bestehenden Stil der Datei
(`harw_runtime::RuntimeAssembly` wird an anderen Stellen ebenfalls ohne `use`
vollqualifiziert verwendet).

### 3. `ConfiguredSecretResolver: Send + Sync` geprüft (nicht nur angenommen)

Felder von `harw-secrets::SecretStore` (`harw-secrets/src/store.rs:70-79`)
gelesen und rekursiv geprüft:

- `root: PathBuf`, `policy: CryptoPolicy`, `provenance: KekProvenance`,
  `key_version: KeyVersion` — reine Werttypen.
- `key_material: Option<KekMaterial>` — `KekMaterial { public_key: Vec<u8>,
  hpke_seed: SecretBox<[u8]> }` (`store.rs:35-38`); `secrecy::SecretBox` hält
  nur einen `Box`, kein `Rc`/`Cell`.
- `index: HashMap<SecretId, StoredSecret>` — `StoredSecret { record:
  SecretRecord, metadata: SecretMetadata }`, beide `#[derive(Serialize,
  Deserialize)]`-Datentypen ohne Interior Mutability.
- `audit: AuditLog` — `{ events: Vec<AuditEvent>, head: [u8; 32] }`
  (`audit/chain.rs:139-142`).
- `checkpoints: CheckpointLog` — `{ checkpoints: Vec<Checkpoint>, head: [u8;
  32] }` (`audit/checkpoint.rs:60-63`).

Kein Feld verwendet `Rc`, `Cell`, `RefCell`, rohe Zeiger oder ein explizites
`impl !Send`/`impl !Sync`. `SecretStore` ist damit automatisch `Send + Sync`
(Rusts Auto-Trait-Ableitung über alle Felder), also ebenso
`ConfiguredSecretResolver { store: SecretStore }` — der `Arc<dyn
harw_provider_http::SecretResolver + Send + Sync>`-Upcast in
`mount_gateway_assembly` ist zulässig. Kein BLOCKED nötig.

### 4. Moduldoku-Zeile ~Z.7 (`gateway.rs`, Spec 3)

Vorher: „**Agenten/Gateway** — der Provider-Weg (nativer Anthropic/Foundry via
[`harw_provider_http::build_provider_with_home`]), an den Nachrichten als
Turns gehen.“

Nachher: „**Agenten/Gateway** — der Provider-Weg über die
Gateway-`RuntimeAssembly` (mit Secret-Resolver für versiegelte
`secrets:`-Credentials, siehe [`crate::runtime_gateway::gateway_assembly`]),
an den Nachrichten als Turns gehen.“ — `build_provider_with_home` wird seit
B3 in `gateway.rs` nicht mehr aufgerufen (B3-Ledger Abschnitt 2); die Doku
folgte dem bisher nicht nach.

Zusätzlich `runtime_gateway.rs`s eigene Moduldoku (volle Schreibrechte auf
diese Datei) um einen Abschnitt „Secret-Resolver für `ModelSource::Configured`
(Regression aus B3)“ ergänzt und Punkt 3 der Bau-Reihenfolge sowie den
„Kein stiller Fallback“-Abschnitt von `build_assembly` auf den direkten
Builder-Aufruf umgestellt.

### 5. Test (Spec 4)

`mount_gateway_assembly`s bestehender Test
(`test_gateway_run_without_provider_fails_instead_of_echo`,
`harw-cli/src/gateway.rs:1627`) ruft nur `mount_gateway_assembly(home, cwd,
sessions_root)` auf — Signatur unverändert, kein Testcode-Update nötig. Für
ein leeres Tempdir-Home ohne `default_provider` läuft der neue vorab geladene
`load_config`-Aufruf durch (derselbe Home/Cwd, den A4s bzw. B3s Tests bereits
erfolgreich durch `load_config` innerhalb der Montage laufen ließen — kein
`RuntimeError::Trust`, siehe `ledger/W2d1/A4.md` Testbegründung),
`configured_provider_uses_sealed_secret` liefert `false` (kein Provider
konfiguriert) → `open_configured_secret_resolver` gibt `Ok(None)` →
`secret_resolver = None`; der eigentliche Fehlschlag bleibt wie zuvor
`RuntimeError::Provider` aus dem Modellbau, Präfix `"gateway: "` unverändert.

`runtime_gateway.rs`s eigener Test
(`test_gateway_assembly_without_provider_config_returns_err_not_echo`) um das
neue `None`-Argument ergänzt.

## API-Belege (gelesen, Datei:Zeile)

- `harw-cli/src/secret_store.rs:1-92` — `ConfiguredSecretResolver`,
  `open_configured_secret_resolver(home: &Path, config: &ResolvedConfig) ->
  Result<Option<ConfiguredSecretResolver>, String>`.
- `harw-runtime/src/assembly.rs:1-92,309-462` — Moduldoku (Bau-Reihenfolge,
  Schritt 1 `load_config(&spec)`), `RuntimeAssemblyBuilder`-Felder/-Methoden,
  Imports (`build_root_model_with_resolver`, `SecretResolver`), `build()`-Kopf
  bis Schritt 9.
- `harw-runtime/src/lib.rs` — Re-Exports (`load_config`, `EntryKind`,
  `RuntimeAssembly`, `RuntimeAssemblyBuilder`, `ModelSource`, `RuntimeStores`,
  `build_root_model_with_resolver`).
- `harw-runtime/src/config.rs:1-110` — `ConfigTrustReport`, exakte Signatur
  und Fehlerfälle von `load_config`.
- `harw-cli/src/runtime_entry.rs` (vollständig) — `runtime_spec`,
  `build_assembly` (letzteres jetzt bewusst umgangen, siehe Befund).
- `harw-secrets/src/store.rs:1-100`, `audit/chain.rs:130-150`,
  `audit/checkpoint.rs:50-70` — Felder von `SecretStore`, `KekMaterial`,
  `AuditLog`, `CheckpointLog` für den `Send + Sync`-Beleg.
- `docs/remediation/ledger/W2d1/A4.md`, `.../B3.md` — Vorzustand,
  Parallel-Vertrag-Historie, B3s offene Annahme (Regression).
- `docs/remediation/AGENT-BRIEF.md` — Rollen-/Brief-Vertrag.

## Stubbed imports

Keine.

## Annahmen

Keine über den Brief hinaus. Der Parallel-Vertrag mit C2a
(`RuntimeAssemblyBuilder::secret_resolver`) stimmt exakt mit C2as eigenem
Ledger überein; keine Anpassung an den hier geschriebenen Aufrufstellen nötig.

## Abweichungen / Blocker

Keine. `ConfiguredSecretResolver: Send + Sync` wurde geprüft (Abschnitt 3),
kein BLOCKED.
