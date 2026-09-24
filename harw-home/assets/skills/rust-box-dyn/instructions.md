# Box<dyn Trait> — Trait-Objekte und heterogene Collections

**Regel:** Nutze `Box<dyn Trait>` wenn der konkrete Typ zur Compile-Zeit unbekannt ist oder wenn eine Collection verschiedene implementierende Typen halten soll. Der Dispatch ist dynamisch (vtable-Lookup zur Laufzeit).

**Warum:** Generics (`fn foo<T: Trait>`) erzeugen monomorphisierte Kopien — effizienter, aber homogen. `Box<dyn Trait>` erlaubt Heterogenität: mehrere verschiedene Typen in einem `Vec` oder als Rückgabewert aus einer Factory. Das Rust Book (Kap. 18.2): *"A trait object points to both an instance of a type implementing our specified trait and a table used to look up trait methods on that type at runtime."*

## Falsch

```rust
// Generics funktionieren nur für EINEN konkreten Typ pro Vec-Instanz
fn build_vault<V: SecretVault>() -> V { /* ... */ }  // ❌ Caller muss Typ nennen

// Kann keine FileVault und PlaintextVault im selben Vec halten
let vaults: Vec<FileVault> = vec![FileVault::new()];  // ❌ nur ein Typ möglich
```

## Richtig

```rust
// Factory: gibt entweder FileVault oder InProcessPlaintextVault zurück —
// Caller kennt nur den Trait, nicht die konkrete Implementierung.
//
// Realer Code: apps/acme-app/src/services/secret_vault.rs:841
pub fn build_vault() -> AppResult<Box<dyn SecretVault>> {  // ✅
    if let Ok(backend) = std::env::var("SECRET_VAULT_BACKEND") {
        match backend.to_lowercase().as_str() {
            "file" => {
                let vault = FileVault::open(&kek_path, &vault_dir)?;
                return Ok(Box::new(vault));   // ✅ konkrete Impl geboxed
            }
            _ => {}
        }
    }
    if insecure_env_flag() {
        tracing::warn!("using insecure plaintext vault — dev/test only");
        return Ok(Box::new(InProcessPlaintextVault::new()));  // ✅ anderer Typ, gleicher Trait
    }
    Err(AppError::ConfigMissing("no vault backend configured".to_owned()))
}

// Heterogene Collection: App-Context hält verschiedene Repository-Impls
// Realer Code: apps/acme-app/src/app.rs:180
pub struct AppContext {
    pub users:         Arc<dyn UserRepository>,      // ✅ Trait-Objekt in Arc (shared)
    pub invoice_read:  Arc<dyn InvoiceReadRepository>,
    pub sachkonto:     Arc<dyn SachkontoRepository>,
    // ... weitere Repos — jeder kann eine andere Implementierung sein
}
```

## Box<dyn> vs Arc<dyn>

```rust
// Box<dyn Trait>  — Alleinbesitz, single-threaded übergabe
let vault: Box<dyn SecretVault> = build_vault()?;

// Arc<dyn Trait>  — geteilter Besitz über Threads (Send + Sync required)
let repo: Arc<dyn UserRepository> = Arc::new(PgUserRepository::new(pool));
let repo2 = Arc::clone(&repo);  // ✅ nur Pointer-Clone, nicht die Repo-Daten
```

## Wann Box<dyn> vs Generics?

| Kriterium | Generics `T: Trait` | `Box<dyn Trait>` |
|---|---|---|
| Dispatch-Kosten | statisch (compile-time) | dynamisch (vtable, ~1 Indirektion) |
| Heterogene Collection | nein | ja |
| Typ zur Compile-Zeit bekannt | ja | nein (Factory/Plugin) |
| Binary-Größe | größer (monomorphisiert) | kleiner |

Faustregel: Generics für Performance-kritische Pfade, `Box<dyn>` für Factory-Funktionen, Plugin-Systeme und heterogene Collections.

## Quelle

- <https://doc.rust-lang.org/book/ch18-02-trait-objects.html> — Kap. 18.2: Using Trait Objects That Allow for Values of Different Types

Allgemeine Variante: `interfaces-and-abstraction` (siehe auch `shared-state-and-concurrency`)
