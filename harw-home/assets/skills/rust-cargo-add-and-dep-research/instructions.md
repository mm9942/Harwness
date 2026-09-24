# Dependencies hinzufügen & Crate-Syntax recherchieren

**Regel:** Schreibe **niemals** eine Versionsnummer von Hand in `Cargo.toml`. Nutze `cargo add <crate>` (ggf. `--features …`), damit immer die **neueste** kompatible Version gewählt wird. Für Syntax/API-Fragen recherchiere **zwei** Quellen — docs.rs (aktuell, im Web) **und** die tatsächlich aufgelöste Quelle unter `~/.cargo/registry/src/…` (exakt die Version, gegen die du baust). Lagere die Recherche **immer an Subagents** aus.

**Warum:** Hart eingetippte Versionen (`"0.8"`, `"1"`) veralten und erzeugen Auflösungskonflikte, sobald eine andere Crate im Workspace eine neuere Transitiv-Abhängigkeit zieht. `cargo add` kennt die verfügbaren Feature-Flags und wählt die aktuellste Version, die mit dem restlichen Lockfile verträglich ist. docs.rs zeigt die dokumentierte API; die Registry-Quelle zeigt die **echte** Signatur der Version, die du wirklich verwendest (docs.rs kann eine andere Minor-Version sein).

---

## 1. Crate hinzufügen — `cargo add` mit Features

```bash
# Einfach: neueste Version, in das Crate im aktuellen Verzeichnis
cargo add tokio

# Mit Features (Komma-getrennt, KEIN Leerzeichen)
cargo add tokio --features rt-multi-thread,macros,net

# In ein bestimmtes Workspace-Member (nicht cwd-abhängig)
cargo add axum --features json -p acme-s3iced

# Genaue Version erzwingen (nur wenn Kompatibilität es verlangt — mit Kommentar begründen)
cargo add serde@1.0.150 --features derive

# Dev-dependency
cargo add --dev tokio --features rt,macros

# Workspace-Dep referenzieren (Version steht zentral in [workspace.dependencies])
cargo add serde_json --workspace   # ins Root, dann `serde_json.workspace = true` im Member
```

`cargo add` schreibt die aufgelöste Version selbst in `Cargo.toml`. **Nie** danach die Zeile von Hand „aufhübschen".

**Verfügbare Features herausfinden**, bevor man rät:

```bash
cargo add <crate> --dry-run          # zeigt alle Features + welche default sind, ohne zu schreiben
```

---

## 2. Versionskonflikt auflösen — „failed to select a version"

Symptom (realer Fall aus diesem Workspace):

```
error: failed to select a version for `matchit`.
    ... required by package `axum v0.8.0`
    ... previously selected package `matchit v0.8.5`
        ... required by `vectory-api`
```

Ursache: `axum 0.8.0` pinnt `matchit` **exakt** (`=0.8.4`), aber `vectory-api` verlangt `matchit ^0.8.5`. Alte axum-Zeile war `axum = "0.8"` → cargo wählte fälschlich die älteste 0.8.0.

**Falsch** — Versionen von Hand in `Cargo.toml` gegeneinander schieben, raten welche Kombination passt.

**Richtig** — die konfliktverursachende Crate auf die neueste Version heben; `cargo add` findet die Kombination, die den Konflikt auflöst:

```bash
# axum auf die neueste 0.8.x heben — die nutzt matchit 0.9 statt =0.8.4
cargo add axum -p acme-s3iced
# → schreibt z.B. axum = "0.8.6"; Konflikt verschwindet, weil 0.8.6 den harten Pin gelockert hat
```

Wenn nur der Lockfile-Eintrag veraltet ist (Cargo.toml passt schon):

```bash
cargo update -p axum                 # eine Crate auf neueste kompatible Lock-Version
cargo update -p matchit --precise 0.9.2   # exakt auf eine Version zwingen
```

> `cargo update -p <name>` schlägt fehl, wenn die Crate im Default-Member-Graph gar nicht vorkommt
> (`did not match any packages`). Dann die Ziel-Crate über `-p <member>` ansprechen oder
> `cargo add` im richtigen Member ausführen — das zieht sie in den Graph.

**Grundsatz:** Bei Konflikten immer die **neuere** Seite hochziehen, nie die andere künstlich herunterpinnen.

---

## 3. Syntax/API recherchieren — docs.rs + `~/.cargo`-Quelle

Zwei Quellen, immer beide, weil sie sich ergänzen:

**a) docs.rs (Web) — dokumentierte API, Feature-Matrix, Beispiele**

```
https://docs.rs/<crate>/<version>/<crate>/
https://docs.rs/<crate>/latest/<crate>/            # neueste veröffentlichte
```

Per Subagent mit WebFetch abrufen. Wichtig: die Version in der URL auf die **tatsächlich aufgelöste** setzen (aus `Cargo.lock`), nicht blind `latest`.

**b) `~/.cargo`-Registry-Quelle — der echte Code der gebauten Version**

Der Quelltext exakt der Version, gegen die du kompilierst, liegt lokal:

```bash
# Pfad der aufgelösten Version finden
ls ~/.cargo/registry/src/index.crates.io-*/ | grep '^axum-'
#   → axum-0.8.6

# Signatur/Feature real nachschlagen (Beispiele)
grep -rn 'pub fn ' ~/.cargo/registry/src/index.crates.io-*/axum-0.8.6/src/routing/
grep -rn 'pub async fn' ~/.cargo/registry/src/index.crates.io-*/tokio-*/src/net/tcp/
# Feature-Gates einer API prüfen
grep -rn 'feature = ' ~/.cargo/registry/src/index.crates.io-*/<crate>-<ver>/src/lib.rs
```

Das ist die verlässlichste Quelle: es ist derselbe Code, den `rustc` sieht. docs.rs kann eine abweichende Minor-Version rendern.

> ⚠️ Ein Compilerfehler mit Pfad `~/.cargo/registry/src/…` liegt **in der Dependency**, nicht im
> Projektcode — nicht inline „reparieren"; per `dep-patcher`/`debug-orchestrator` behandeln.

---

## 4. Immer Subagents nehmen — Arbeitsteilung

Die gesamte Recherche wird an **fokussierte, parallele Sonnet-Subagents** delegiert. Jeder Subagent:

- ruft docs.rs per **WebFetch** ab **und** grept die `~/.cargo/registry/src`-Quelle,
- meldet die **exakte** `cargo add …`-Kommandozeile (inkl. Features) + die relevante Signatur zurück,
- **baut niemals selbst** (`cargo build/check/test/run`, `make`, `rustc` sind für Subagents verboten — crasht den Server).

Der **Main-Loop** führt das gemeldete `cargo add` / `cargo update` aus (das ist erlaubt: `add`, `fmt`, `metadata`, `search`, `doc`) und baut anschließend.

**Muster:**

```
Subagent (Sonnet, background):
  „Recherchiere Crate `axum` neueste 0.8.x: (1) WebFetch docs.rs für die
   Router-API + verfügbare Features, (2) grep ~/.cargo/registry/src/*/axum-*/src
   nach der echten Signatur von `Router::route`. Baue NICHT. Gib zurück:
   exakte `cargo add`-Zeile mit Features + die Signatur + docs.rs-URL."
     ↓ meldet zurück
Main-Loop:  cargo add axum --features … -p <member>   → dann make/cargo check
```

Mehrere unabhängige Crates → mehrere Subagents **parallel** (ein Message, mehrere Tool-Calls), disjunkte Crates pro Agent.

---

## Quelle

- <https://doc.rust-lang.org/cargo/commands/cargo-add.html>
- <https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html>
- <https://doc.rust-lang.org/cargo/commands/cargo-update.html>
- <https://doc.rust-lang.org/cargo/reference/resolver.html> (Version-Auflösung & Konflikte)
- `cargo add --help`, `cargo update --help`

Allgemeine Variante: `dependency-add-and-research`
