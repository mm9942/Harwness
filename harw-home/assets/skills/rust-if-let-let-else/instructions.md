# `if let` und `let … else` in Rust

**Regel:** Verwende `if let` wenn nur ein Muster relevant ist; verwende `let … else` wenn der fehlende Wert eine sofortige Divergenz (return / break / continue) auslösen soll. Niemals `.unwrap()` oder `.expect()` in Produktionspfaden.

**Warum:** `if let` reduziert Verschachtelungstiefe gegenüber einem vollständigen `match`. `let … else` hält den Happy-Path-Code flach — der extrahierte Wert ist nach dem `else`-Block im äußeren Scope verfügbar, ohne eine weitere Einrückungsebene zu erzeugen.

## Falsch

```rust
// unwrap() in Produktionspfad — panic bei None/Err, verboten
let exe = std::env::current_exe().unwrap();

// übermäßige Verschachtelung statt Guard-Klausel
fn needs_setup() -> bool {
    let maybe_bin = setup_binary_path();
    if maybe_bin.is_some() {
        let setup_bin = maybe_bin.unwrap(); // doppelter unwrap
        if setup_bin.is_file() {
            // …mehr Logik, immer tiefer eingerückt
            return true;
        }
    }
    false
}
```

## Richtig — `if let` für optionale Verarbeitung

Wenn ein Wert vorhanden sein *kann* und du im Fehler-/None-Fall einfach weitermachst:

```rust
// Datei: apps/sgh-flow/src/services/auto_cli_key.rs:208
// Env-Variable ist optional; fehlt sie, macht die Funktion einfach weiter.
if let Ok(raw) = std::env::var(ADMIN_CLI_KEY_ORG_ENV) {
    match raw.trim().parse::<Uuid>() {
        Ok(id) => {
            info!(org_id = %id, env = ADMIN_CLI_KEY_ORG_ENV, "using org from env override");
            return Ok(Some(id));
        }
        Err(_) => {
            warn!(env = ADMIN_CLI_KEY_ORG_ENV, value = %raw, "not a valid UUID — falling back");
        }
    }
}
// Kein else nötig — Kontrollfluss läuft einfach weiter

// Datei: apps/sgh-flow/src/services/auto_cli_key.rs:235
if let Some(ref oid) = id {
    info!(org_id = %oid, "resolved first organization");
}
```

## Richtig — `let … else` als Guard-Klausel (frühe Rückkehr)

Wenn das Fehlen des Wertes bedeutet, dass die Funktion sofort abbrechen soll.
Der extrahierte Wert (`exe`, `setup_bin`) ist *nach* dem `else`-Block im Scope — kein
zusätzliches Einrücken nötig.

```rust
// Datei: apps/sgh-flow/src/main.rs:25
// Kann current_exe nicht auflösen → einfach zurückkehren, kein Crash
let Ok(exe) = std::env::current_exe() else {
    return;
};
// `exe` ist hier als PathBuf verfügbar

// Datei: apps/sgh-flow/src/setup_guard.rs:60
pub fn needs_setup() -> bool {
    // Setup-Binary fehlt → Setup bereits abgeschlossen, false zurückgeben
    let Some(setup_bin) = setup_binary_path() else {
        return false;
    };
    if !setup_bin.is_file() {
        return false;
    }
    // Datei: apps/sgh-flow/src/setup_guard.rs:67
    let Some(prefix) = install_prefix() else {
        return true; // Prefix nicht auflösbar → Konfiguration fehlt
    };
    REQUIRED_CONFIGS
        .iter()
        .any(|rel| !prefix.join(rel).is_file())
}
```

## Entscheidungsbaum

| Situation | Muster |
|---|---|
| Nur ein Muster interessiert, Rest wird ignoriert | `if let Some(x) = …` |
| Ein Muster plus expliziter Fallback | `if let Some(x) = … { … } else { … }` |
| Fehlender Wert = sofortige Divergenz (return/break) | `let Some(x) = … else { return Err(…); }` |
| Mehrere Muster oder Exhaustivität gewünscht | `match` |

### Verbote in Produktionspfaden

- `unwrap()` / `expect()` — paniken bei `None`/`Err`
- `todo!()` / `unimplemented!()` — paniken immer
- Fehler still verschlucken: `let _ = risky_call();` ohne Logging und ohne Begründung im Kommentar

Stattdessen immer explizit behandeln oder mit `?` und einem eigenen Fehler-Enum propagieren.

## Quelle
- https://doc.rust-lang.org/book/ch06-03-if-let.html

Allgemeine Variante: `null-and-optional-handling`
