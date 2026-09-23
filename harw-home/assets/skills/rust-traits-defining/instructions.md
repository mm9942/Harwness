# Traits nach Verhalten benennen

**Regel:** Benenne jeden Trait nach dem Verhalten, das er kapselt — nicht nach dem Typ, der ihn implementiert, und nicht nach der Technologie dahinter.

**Warum:** Ein Trait ist ein Vertrag über das, was ein Typ *kann*, nicht was er *ist*. Behavior-Namen (`KeyRepository`, `AuditRepository`, `Builder`) sind austauschbar: ein `MockKeyRepository` und ein `PostgresKeyRepository` erfüllen beide `KeyRepository`, weil sie dieselbe Rolle spielen — nicht weil sie beide "Datenbank" sind.

> „Trait definitions are a way to group method signatures together to define a set of behaviors necessary to accomplish some purpose."
> — The Rust Programming Language, Ch. 10.2

## Falsch

```rust
// Trait nach Typ benannt → koppelt Interface an Implementierung
pub trait PostgresDatabase {
    fn create_keyset(&self, org_id: Uuid) -> impl Future<Output = Result<Uuid>> + Send;
}

// Trait nach Sammelbegriff benannt → sagt nichts über Verhalten aus
pub trait DataHandler {
    fn store(&self, data: &[u8]) -> impl Future<Output = Result<()>> + Send;
}
```

## Richtig

```rust
// crates/securehub-db/src/repository.rs:172
pub trait KeyRepository: Send + Sync {
    fn create_keyset(
        &self,
        org_id: Uuid,
        vault_id: Uuid,
        purpose: &str,
        label: &str,
        aead_alg: &str,
    ) -> impl std::future::Future<Output = Result<Uuid>> + Send;

    fn list_keysets(
        &self,
        org_id: Uuid,
    ) -> impl std::future::Future<Output = Result<Vec<KeysetRow>>> + Send;
}

// crates/securehub-db/src/repository.rs:219
pub trait VaultRepository: Send + Sync {
    fn get_or_create_default(
        &self,
        org_id: Uuid,
    ) -> impl std::future::Future<Output = Result<Uuid>> + Send;
}

// crates/securehub-db/src/repository.rs:272
pub trait AuditRepository: Send + Sync {
    fn append(
        &self,
        org_id: Uuid,
        actor_sub: Option<Uuid>,
        actor_kind: &str,
        action: &str,
        resource_kind: &str,
        resource_id: Option<Uuid>,
        outcome: &str,
        reason: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}
```

### Wann einen neuen Trait extrahieren

Extrahiere einen Trait, sobald **zwei oder mehr Typen dieselben Methoden anbieten müssen** — z. B. Postgres-Impl + Mock-Impl für Tests. Der Trait ist der Vertrag; beide Typen implementieren ihn unabhängig voneinander.

### `Send + Sync` Supertrait

Async-Repository-Traits in diesem Projekt tragen immer `: Send + Sync`, damit sie hinter `Arc<dyn …>` in `axum`-Handlern und `tokio`-Tasks genutzt werden können.

### Hausregeln

- Trait-Name: Nomen oder Partizip, das das **Verhalten** beschreibt (`Repository`, `Builder`, `Resolver`, `Installer`).
- Niemals nach Technologie benennen (`Postgres…`, `Redis…`, `Http…`).
- Kein `anyhow` / `thiserror` — eigene Error-Enums.
- Keine `clone()`-Aufrufe, wo `to_owned()` / `Arc::clone` ausreicht.

## Quelle

- https://doc.rust-lang.org/book/ch10-02-traits.html
