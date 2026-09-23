# Option::as_ref / as_deref and the AsRef Trait

**Regel:** Verwende `.as_deref()` um `Option<String>` → `Option<&str>` zu konvertieren ohne die `Option` zu konsumieren. Verwende `impl AsRef<T>` in Funktionssignaturen um mehrere Typen (z. B. `&str`, `String`, `&Path`, `PathBuf`) mit einer einzigen Signatur zu akzeptieren.

**Warum:** `.clone()` auf einer `Option<String>` erzeugt eine unnötige Allokation nur um einen `&str` zu erhalten. Die Methoden `.as_ref()` (gibt `Option<&T>`) und `.as_deref()` (gibt `Option<&T::Target>` nach automatischer Deref-Koersion) borgen den Inhalt ohne Eigentümerschaft zu übertragen. `impl AsRef<X>` in Parametern macht eine Funktion generisch über alle Typen, die zu `&X` dereferenziert werden können — analog zum Buchempfehlungsmuster `&str` statt `&String`.

## Falsch
```rust
// Option<String> klonen nur um den Inhalt weiterzugeben — unnötige Allokation
fn find_users(filter: &UserFilter) -> Vec<User> {
    // filter.q ist Option<String>
    let q: Option<String> = filter.q.clone();              // ❌ Allokation
    let lower = q.unwrap_or_default().to_ascii_lowercase(); // ❌ weiterer Owned-String
    // ...
}

// Funktion akzeptiert nur genau &str — zwingt Caller zu Konvertierungen
fn set_env(key: &str, value: &str) {                       // ❌ zu einschränkend
    std::env::set_var(key, value);
}
set_env("KEY", &my_string); // Caller muss `&` ergänzen, Pathbuf geht gar nicht
```

## Richtig
```rust
// Option<String> → Option<&str> via as_deref(); kein Clone, kein Move
fn find_users(filter: &UserFilter) -> Vec<User> {
    // filter.q: Option<String>  (aus db/repositories/org_users.rs:341)
    let q_lower = filter.q.as_deref().unwrap_or("").to_ascii_lowercase();
    // ^^^^^^^^^^^^^ borgt den String-Inhalt, gibt Option<&str>
    // ...
}

// impl AsRef<str> akzeptiert &str, String, Cow<str>, … ohne Konvertierung
// (aus apps/sgh-flow/src/runtime.rs:508)
fn set_env_if_absent(key: &str, value: impl AsRef<str>) {
    if key.is_empty() || std::env::var_os(key).is_some() {
        return;
    }
    std::env::set_var(key, value.as_ref()); // .as_ref() gibt &str
}

// Option::as_ref bei Option<T> wo T nicht Deref implementiert:
// borgt den Inhalt als Option<&T> um z. B. .map() anzuwenden
let text: Option<String> = Some("Hallo".to_owned());
let len: Option<usize> = text.as_ref().map(|s| s.len()); // text bleibt gültig
println!("text ist noch da: {text:?}");

// as_deref für Option<String> → Option<&str> (kompakteste Form):
let x: Option<String> = Some("hey".to_owned());
assert_eq!(x.as_deref(), Some("hey")); // Option<&str>, kein Clone
```

## Quelle
- https://doc.rust-lang.org/book/ch06-01-defining-an-enum.html (Option-Enum, Kapitel 6.1)
- https://doc.rust-lang.org/book/ch06-03-if-let.html (if let mit Option, Kapitel 6.3)
- https://doc.rust-lang.org/std/option/enum.Option.html#method.as_ref (Option::as_ref, stabil seit 1.0.0)
- https://doc.rust-lang.org/std/option/enum.Option.html#method.as_deref (Option::as_deref, stabil seit 1.40.0)
