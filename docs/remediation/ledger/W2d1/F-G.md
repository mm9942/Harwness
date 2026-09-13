# W2d-1 / F-G — Fix-Agent Gateway (Befunde aus Z2d1-gateway)

Rolle: focused-bug-fix (Opus). BUILD-POLICY eingehalten: nur Lesen/grep, kein cargo build/check/test/clippy,
kein make/rustc/rust-analyzer, keine git-Schreibbefehle. **Nichts kompiliert, keine Tests ausgeführt** —
Verifikation durch Lesen.

## Geänderte Dateien

- `harw-cli/src/gateway.rs` — Moduldoku §Fehler, `run`, neue private Items `GatewaySecretResolver`,
  `GatewayAssemblies`, `GatewayProviders`, `open_gateway_secret_resolver`, `mount_gateway_assembly`
  (Rückgabetyp), `supervise` (Parameter), Import `Principal`, drei Tests + zwei Test-Helfer.
- `harw-cli/src/runtime_gateway.rs` — nur Doku (Moduldoku, `GatewayEntry::Dream`, `channel_principal` G6,
  `gateway_assembly` Arg-Doku). Keine Signatur-/Verhaltensänderung.

Nicht angefasst: W1-12-Code (`start_telegram_long_poll`, `supervise_telegram_long_poll`,
`telegram_restart_backoff`, Token-Redaktion, Pinning), `dream_scheduler`/`run_dream_job`, alle anderen Dateien.

## Geänderte / neue Signaturen (exakt)

```rust
// gateway.rs (alle privat)
type GatewaySecretResolver = Arc<dyn harw_provider_http::SecretResolver + Send + Sync>;

struct GatewayAssemblies {
    telegram: harw_runtime::RuntimeAssembly,
    dream: harw_runtime::RuntimeAssembly,
}

struct GatewayProviders {
    telegram: Arc<dyn ModelProvider>,
    dream: Arc<dyn ModelProvider>,
}

fn open_gateway_secret_resolver(
    entry: GatewayEntry,
    home: &Path,
    cwd: &Path,
    principal: &Principal,
) -> Result<Option<GatewaySecretResolver>, String>;

// vorher: -> Result<harw_runtime::RuntimeAssembly, String>
fn mount_gateway_assembly(
    home: &Path,
    cwd: &Path,
    sessions_root: &Path,
) -> Result<GatewayAssemblies, String>;

// vorher: provider: Arc<dyn ModelProvider> an Position 3
async fn supervise(
    home: &Path,
    config: Arc<ResolvedConfig>,
    providers: GatewayProviders,
    knowledge: &KnowledgeStore,
    dream_transcript_root: &Path,
    telemetry_sink: Arc<dyn TelemetrySink>,
    audit_chain_check_interval_secs: u64,
) -> Result<(), String>;
```

`pub fn run` unverändert in der Signatur. `GatewayProviders` bündelt beide Modelle, weil ein achter
`supervise`-Parameter `clippy::too_many_arguments` (Schwelle 7) auslösen würde — kein `#[allow]`.

## Befunde

### G1 (blocker) — geschlossen
`mount_gateway_assembly` baut jetzt zusätzlich
`gateway_assembly(GatewayEntry::Dream, home, cwd, channel_principal(GatewayEntry::Dream, ""),
crate::runtime_entry::transcript_state_store(sessions_root, dream_thread_for_session), secret_resolver)`.
`GatewayEntry::Dream` wird damit im Produktivpfad konstruiert (dead_code behoben). Dream-State-Store:
derselbe Mapper `dream_thread_for_session` (gateway.rs `fn dream_thread_for_session`), den
`build_dream_state_store` nutzt; als `Arc<dyn StateStore>` über den echten Konstruktor
`runtime_entry::transcript_state_store` (runtime_entry.rs, `-> Arc<dyn StateStore>`). Resolver wird einmal
geöffnet, Telegram bekommt `secret_resolver.as_ref().map(Arc::clone)`, Dream das Original.
`run` reicht `Arc::clone(assemblies.dream.model())` über `GatewayProviders::dream` an
`dream_scheduler(dream_provider.as_ref(), …)`.
Dream-Deaktivierung: `harw-config` kennt **keinen** Dream-Schalter (`grep -i dream harw-config/src` leer);
der Scheduler läuft immer, daher wird die Dream-Montage stets gebaut (in `GatewayAssemblies`-Doku vermerkt).

### G2 (major) — geschlossen (Tests geschrieben, nicht ausgeführt)
- `test_mount_gateway_assembly_sealed_secret_provider_resolves_with_kek`: Temp-Home mit
  `config.toml` (`default_provider = "sealed"`, `default_model = "model"`), `providers/sealed.toml`
  (`api = "openai-chat"`, `base_url = "https://example.test/v1"`, `auth = "secrets:provider-token"`),
  `models/model.toml` (`id = "model"`, `provider = "sealed"`), `auth.toml` (`[kek] provenance = "key_file"`,
  `key_file_path`), KEK über bestehendes `write_gateway_test_kek`, Token versiegelt über
  `harw_secrets::SecretStore::with_key_material(<home>/sealed-secrets, …).create("provider-token", …)`.
  Erwartet `Ok`; im Fehlerfall zuerst Assertion „kein `sealed secret`/`secret resolver failed`", dann panic
  mit Fehlertext. Zusätzlich G1-Beleg: `rights_snapshot().entry` = `GatewayTelegram` bzw. `GatewayDream`,
  `principal.id()` = `telegram:gateway` bzw. `gateway-dream`. Kein Netz (Provider wird nur gebaut).
- `test_mount_gateway_assembly_sealed_secret_provider_without_kek_returns_err`: gleiche Config ohne
  `auth.toml` → `error.starts_with("gateway: enabled sealed-secret provider requires a configured KEK")`,
  kein `provider-token` im Text.
- `test_open_gateway_secret_resolver_without_sealed_provider_returns_none`: leeres Home → `Ok(None)`.

Neue Test-Helfer: `write_gateway_sealed_provider_home(home, Option<&Path>)`,
`seal_gateway_test_provider_token(home, key_path)`.

### G3 (minor) — geschlossen durch G1.

### G4 (minor) — geschlossen
Hart codiertes `harw_runtime::EntryKind::GatewayTelegram` entfernt; `open_gateway_secret_resolver` baut die
vorläufige Spec über `entry.entry_kind()` und läuft einmal für beide Montagen (`load_config` hängt nur von
`home`/`cwd` ab, config.rs `load_config`: `config_layers_report_at(&spec.home, &spec.cwd)`). Das doppelte
Config-Laden (vorläufig + im Builder) bleibt strukturell bestehen (Builder lädt intern), aber nicht mehr je
Montage zusätzlich.

### G5 (minor) — geschlossen
Moduldoku §Fehler und `run`-Doku §Errors ergänzt: Provider fehlt/nicht baubar, KEK fehlt (exakter Text),
KEK-Material/Store, Config/Trust, untrusted Repo nur Warnung, cwd-Abhängigkeit (`current_dir`, systemd
`WorkingDirectory=`), Audit-Scheduler startet bei Mount-Fehler nicht.

### G6 (minor) — geschlossen
`telegram:gateway` als Daemon-Platzhalter bis P1.6 dokumentiert: Kommentar in `mount_gateway_assembly` und
Abschnitt „Daemon-Platzhalter `telegram:gateway` (Befund G6)" in `channel_principal`-Doku. Keine
Verhaltensänderung.

### G7 (minor) — offen, Folgewelle W4a A-GW
Assemblies bleiben Config/Provider-Fabrik; Telegram-Consumer und `run_dream_job` bauen weiter eigene
`TranscriptStateStore`/`AgentSession` ohne `new_root_session`. Nicht angefasst.

### G8 — unverändert ok (W1-12-Code nicht verändert).

## API-Nachweise (Datei:Zeile, gelesen)

- `harw-cli/src/runtime_gateway.rs:123` `channel_principal`, `:180` `gateway_assembly(entry, home, cwd,
  principal, state_store, secret_resolver: Option<Arc<dyn SecretResolver + Send + Sync>>)`.
- `harw-cli/src/runtime_entry.rs` `runtime_spec(EntryKind, &Path, &Path, Principal) -> RuntimeSpec`,
  `transcript_state_store(&Path, SessionThreadMapper) -> Arc<dyn StateStore>`.
- `harw-core/src/state_store.rs:127` `type SessionThreadMapper = fn(&SessionId) -> ThreadRef`.
- `harw-cli/src/secret_store.rs:65-92` `open_configured_secret_resolver -> Result<Option<ConfiguredSecretResolver>, String>`;
  `:77` Text `"enabled sealed-secret provider requires a configured KEK"`; `:165-170` Gate (`enabled` +
  `SecretRef::Secrets`).
- `harw-runtime/src/lib.rs` Re-Exports `load_config`, `EntryKind`, `RuntimeAssembly`.
- `harw-runtime/src/config.rs` `load_config(&RuntimeSpec) -> RuntimeResult<(ResolvedConfig, ConfigTrustReport)>`.
- `harw-runtime/src/assembly.rs:1060` `config()`, `:1066` `trust_report()`, `:1149` `model()`,
  `:1346` `rights_snapshot()` (Felder `entry`, `principal`, spec.rs:248-252); `:566` Model-Bau mit Resolver.
- `harw-types/src/principal.rs:150` `Principal::id`.
- Config-Format (zusätzlich gelesen, laut Brief „harw-config verifizieren"): `harw-config/src/discovery.rs:453-510`
  (`config.toml`, `providers/*.toml` Key = `name`, `models/*.toml` Key = `id` `:853`, `auth.toml`),
  `:54-80` `validate` (default_model muss in `models` existieren), `auth_toml.rs:109-129` `[kek]`
  (`provenance = "key_file"`, `key_file_path`), `model_toml.rs:5-22`.
- `harw-provider-http/src/lib.rs:183-215,979-987` Provider-Bau; `secrets:` ohne Resolver/Fehler →
  `"secret resolver failed"`; `:775` Endpoint muss HTTPS sein.
- `harw-secrets/src/store.rs:367` `get_by_reference` akzeptiert Metadatenname.

## Risiken für den Parent-Build

- G2(a) erwartet `Ok` für die komplette Montage (Telegram + Dream) in Tempdirs. Belegt bis Schritt 9
  (A4-Test erreichte den Provider-Fehler); danach nur Spawner `None` + Contributors — nicht ausgeführt.
- Start baut zwei Montagen (doppelte Registry-/Discovery-Arbeit beim Daemon-Start, einmalig).

## Stubbed imports / dep-requests

Keine.
