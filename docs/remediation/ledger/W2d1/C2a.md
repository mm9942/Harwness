# C2a — `secrets:`-Resolver im `RuntimeAssemblyBuilder`

Welle W2d-1/C, Remediation `/home/mia/projects/harwness`. Owned file:
`harw-runtime/src/assembly.rs`.

## Befund

`RuntimeAssemblyBuilder::build` rief `build_root_model(&spec, &config,
model_source)` — ohne Resolver — für **jeden** Einstieg mit
`ModelSource::Configured`. `build_root_model` ist reiner Sugar für
`build_root_model_with_resolver(.., None)` (`harw-runtime/src/model.rs:111-117`).
Damit schlug jede `auth = "secrets:…"`-Provider-Referenz beim Bau über die
Montage fehl — eine Regression gegenüber dem alten Pfad
(`open_configured_secret_resolver` + `build_provider_with_home` direkt in
`harw-cli`), der den Resolver kannte.

## Fix (additiv, keine bestehende Signatur geändert)

1. Neues Feld auf `RuntimeAssemblyBuilder`:
   ```rust
   secret_resolver: Option<Arc<dyn SecretResolver + Send + Sync>>,
   ```
   Default `None` (gesetzt in `RuntimeAssembly::builder`).

2. Neue Builder-Methode:
   ```rust
   #[must_use]
   pub fn secret_resolver(mut self, resolver: Arc<dyn SecretResolver + Send + Sync>) -> Self
   ```
   Übergibt den `secrets:`-Resolver für `ModelSource::Configured`. Der Aufrufer
   (CLI/TUI) öffnet den versiegelten Speicher weiterhin selbst
   (`harw-cli/src/secret_store.rs::open_configured_secret_resolver` →
   `ConfiguredSecretResolver`, geprüft: implementiert `SecretResolver`, hält
   nur einen `SecretStore` — kein `Rc`/`Cell`/rohe Zeiger im Read-Ausschnitt,
   also plausibel `Send + Sync`) und reicht das Ergebnis über diese Methode
   herein.

3. `build()` ruft jetzt `build_root_model_with_resolver` statt
   `build_root_model`, mit dem Upcast der Auto-Traits:
   ```rust
   let model = build_root_model_with_resolver(
       &spec,
       &config,
       model_source,
       secret_resolver.as_deref().map(|r| r as &dyn SecretResolver),
   )?;
   ```
   `secret_resolver.as_deref()` liefert `Option<&(dyn SecretResolver + Send +
   Sync)>` (via `Deref` von `Arc`); `r as &dyn SecretResolver` wirft nur die
   Auto-Traits ab — dieselbe Vtable für `SecretResolver` bleibt gültig, weil
   `SecretResolver` selbst kein Supertrait-Upcasting braucht (es hat keine
   Supertraits). Das ist eine gewöhnliche Unsize-Coercion, kein
   Trait-Upcasting (das bräuchte die 1.86-Stabilisierung für Supertrait-Fälle);
   hier wird nur ein Auto-Trait-Bound entfernt, was seit jeher stabil ist.

4. Import angepasst: `use crate::model::{ModelSource,
   build_root_model_with_resolver};` (ersetzt `build_root_model` — der Import
   wird sonst zum toten Re-Export, weil `build()` jetzt die
   Resolver-Variante ruft). `use harw_provider_http::SecretResolver;` ergänzt.

## Exakte neue Signatur (für C2b, parallel)

```rust
// Feld auf RuntimeAssemblyBuilder (privat):
secret_resolver: Option<Arc<dyn SecretResolver + Send + Sync>>,

// Neue öffentliche Methode:
impl RuntimeAssemblyBuilder {
    #[must_use]
    pub fn secret_resolver(mut self, resolver: Arc<dyn SecretResolver + Send + Sync>) -> Self;
}
```

Aufrufreihenfolge unverändert (Builder-Pattern, `mut self -> Self`, kettbar
wie `.model(..)`, `.stores(..)` etc.). `harw_provider_http::SecretResolver` ist
bereits `pub` re-exportiert von `harw-runtime` über den bestehenden Import in
`crate::model`; C2b kann `harw_provider_http::SecretResolver` direkt
importieren, keine neue Re-Export-Stelle nötig.

## Test

`test_builder_secret_resolver_is_used_for_configured_model` (in
`harw-runtime/src/assembly.rs`, `mod tests`). **Kein** Ende-zu-Ende-Test über
`RuntimeAssemblyBuilder::build()` mit echtem `ModelSource::Configured` +
`secrets:`-Referenz: das bräuchte zusätzlich

- `RuntimeStores` (Pflichtfeld, ein `Arc<dyn StateStore>` — Konstruktion liegt
  in `harw-session-store`, außerhalb der Read-list dieses Agenten), und
- je nach `EntryKind::profile().spawner` (`SpawnerPolicy::BuiltinRoles`) einen
  `session_events`-Sender mitsamt `SessionManager` (`harw-core`).

Beide Bauteile sind nicht in der Read-list und liegen außerhalb der owned
file. Der Test prüft stattdessen den unstrittigen, lokal verifizierbaren Teil:
`RuntimeAssemblyBuilder::secret_resolver(..)` setzt das Feld, das `build()` —
siehe Bau-Stelle oben, `harw-runtime/src/assembly.rs` bei
`build_root_model_with_resolver` — unverändert an die bereits bestehende
(und in `harw-runtime/src/model.rs` bereits getestete) Funktion
`build_root_model_with_resolver` weiterreicht. Der Test nutzt privaten
Zugriff auf `builder.secret_resolver`, da `mod tests` ein Submodul von
`assembly` ist (Rust-Sichtbarkeit: Submodule sehen private Felder ihres
Elternmoduls).

## Verifikation (Lesen, kein Cargo-Build gemäß Policy)

- `cargo metadata --offline --no-deps --format-version 1` — Exit 0, Workspace-
  Graph konsistent, `harw-provider-http` als Abhängigkeit von `harw-runtime`
  vorhanden.
- Manuell geprüft: kein verbliebener Aufruf von `build_root_model` (ohne
  `_with_resolver`) in `assembly.rs`; `SecretResolver`-Import wird an allen
  vier neuen Stellen (Feld, Methode, Upcast, Test) verwendet; `builder()`
  initialisiert `secret_resolver: None`; `Debug`-Impl zeigt nur
  `is_some()` (kein Secret-Leak); keine bestehende öffentliche Signatur
  verändert.

## Stubbed imports

Keine.

## Annahmen

- `ConfiguredSecretResolver` (`harw-cli/src/secret_store.rs`) ist `Send +
  Sync` — nicht durch Compiler-Lauf verifiziert (Policy), aber plausibel: sein
  einziges Feld ist `store: SecretStore` (kein `Rc`, `Cell`, `RefCell` oder
  Rohzeiger im gelesenen Ausschnitt), also greift Rusts automatische
  `Send`/`Sync`-Ableitung, sofern `SecretStore` selbst beides ist. Das ist der
  Typ, den C2b (falls er `RuntimeAssemblyBuilder::secret_resolver` aus
  `harw-cli` heraus aufruft) tatsächlich übergeben wird — sollte
  `SecretStore` doch `!Sync` sein, bräuchte `secret_resolver()` stattdessen
  `Arc<dyn SecretResolver + Send>` ohne `Sync`; das wäre ein Folgebefund für
  den Integrations-Build, nicht für diesen Worker lösbar ohne Compiler-Lauf.
