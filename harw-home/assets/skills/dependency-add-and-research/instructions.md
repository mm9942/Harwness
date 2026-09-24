# Dependencies hinzufügen, auswählen & API recherchieren — ökosystem-neutral

**Regel:**
1. **Nie eine Versionsnummer von Hand ins Manifest tippen.** Füge Dependencies über die **CLI des Paketmanagers** hinzu, damit die neueste mit dem restlichen Lockfile verträgliche Version gewählt wird.
2. **Vor dem Hinzufügen prüfen**: richtiger Name (Typosquatting), Lizenz, Wartungszustand, Security-Advisories, Größe/transitive Last, Install-Skripte.
3. **API-Fragen aus zwei Quellen** beantworten: Registry-/Doku-Seite (aktuell, im Web) **und** die lokal installierte Quelle (exakt die Version, gegen die gebaut wird).
4. **Lockfile committen**, Konflikte durch **Hochziehen der neueren Seite** lösen, nicht durch künstliches Herunterpinnen.
5. **Recherche an Subagents delegieren**; der Main-Loop führt die Paketmanager-Befehle aus und baut.

**Warum:** Handgetippte Versionen veralten, erzeugen Auflösungskonflikte und verfehlen oft die aktuelle, gepatchte Version. Die Paketmanager-CLI kennt verfügbare Versionen, Features/Extras und das Lockfile. Web-Doku kann eine andere Version beschreiben als die installierte — die lokale Quelle ist die Wahrheit für den Build. Supply-Chain-Angriffe kommen fast immer über falsch geschriebene, verlassene oder übernommene Pakete.

## Wann anwenden

- Neue Library/Tool soll ins Projekt, oder „welche Library soll ich für X nehmen?“.
- Upgrade einer Dependency / Security-Advisory.
- Versionskonflikt: npm `ERESOLVE`, pip `ResolutionImpossible`, Go `ambiguous import`/MVS-Upgrade, Maven/Gradle „Could not resolve“, Cargo „failed to select a version“.
- Unklare Signatur/API einer Library.

## 1. Hinzufügen per CLI

| Ökosystem | Hinzufügen | Dev/Test-Dependency | Lockfile |
|---|---|---|---|
| npm | `npm install <pkg>` | `npm install -D <pkg>` | `package-lock.json` |
| pnpm | `pnpm add <pkg>` | `pnpm add -D <pkg>` | `pnpm-lock.yaml` |
| yarn | `yarn add <pkg>` | `yarn add -D <pkg>` | `yarn.lock` |
| uv | `uv add <pkg>` / `uv add '<pkg>[extra]'` | `uv add --dev <pkg>` | `uv.lock` |
| poetry | `poetry add <pkg>` | `poetry add --group dev <pkg>` | `poetry.lock` |
| pip (ohne Projekt-Tool) | Eintrag in `requirements.in` + `pip-compile` | separate `dev-requirements.in` | kompilierte `requirements.txt` |
| Go | `go get <module>@latest` + `go mod tidy` | (Tools: `go get -tool` ab 1.24) | `go.sum` |
| Gradle | Version-Catalog `libs.versions.toml` pflegen; Updates via `gradle-versions-plugin` | `testImplementation` | Dependency Locking (`--write-locks`) |
| Maven | `<dependency>` + `versions:display-dependency-updates` / `versions:use-latest-releases` | `<scope>test</scope>` | — (Versionen explizit, BOMs nutzen) |
| .NET | `dotnet add package <pkg>` | — | `packages.lock.json` (`RestorePackagesWithLockFile`) |
| Cargo | `cargo add <crate> --features a,b` | `cargo add --dev <crate>` | `Cargo.lock` |

Maven/Gradle haben keinen „add“-Befehl, der die neueste Version einsetzt: die aktuelle Version **auf Maven Central nachsehen** (nicht raten), dann mit dem Versions-Plugin verifizieren.

Monorepos/Workspaces: in das **richtige Paket** hinzufügen (`pnpm add --filter <pkg>`, `npm install -w <ws>`, `cargo add -p <member>`, `uv add --package <member>`), zentrale Versionen (Workspace-Deps, Version-Catalog, BOM) respektieren.

## 2. Vor dem Hinzufügen prüfen

| Prüfung | Wie |
|---|---|
| Richtiges Paket? | exakter Name, verlinktes Repo, Downloads/Dependents, Maintainer; Vorsicht bei Tippfehler-Varianten und frisch veröffentlichten Namensvettern |
| Lizenz | Registry-Seite + `LICENSE` im Repo; mit Projektlizenz/Policy verträglich (copyleft beachten) |
| Wartung | letztes Release, Commit-Aktivität, offene Security-Issues, Anzahl Maintainer |
| Advisories | `npm audit`, `pnpm audit`, `pip-audit`, `govulncheck ./...`, `cargo audit`, `dotnet list package --vulnerable`, OWASP dependency-check; Datenbanken: osv.dev, GitHub Advisory Database |
| Gewicht | transitive Abhängigkeiten (`npm ls`, `pipdeptree`, `go mod graph`, `gradle dependencies`, `cargo tree`) |
| Install-Skripte | npm `preinstall`/`postinstall`, Python-sdists mit Build-Code — bei unbekannten Paketen prüfen |
| Brauchen wir sie überhaupt? | reicht die Standardbibliothek oder eine schon vorhandene Dependency? |

## 3. Versionskonflikte

**Grundsatz:** die Seite, die den Konflikt verursacht, auf die **neueste** kompatible Version heben; nicht die andere Seite herunterpinnen.

| Ökosystem | „Warum ist X in dieser Version da?“ | Gezielt aktualisieren |
|---|---|---|
| npm/pnpm/yarn | `npm ls <pkg>` / `pnpm why <pkg>` / `yarn why <pkg>` | `npm install <pkg>@latest`; letzter Ausweg: `overrides`/`resolutions` mit Kommentar |
| Python | `uv tree`, `pipdeptree -r -p <pkg>` | `uv lock --upgrade-package <pkg>` / `poetry update <pkg>` |
| Go | `go mod why -m <module>`, `go mod graph` | `go get <module>@latest && go mod tidy` |
| Gradle/Maven | `gradle dependencyInsight --dependency <x>` / `mvn dependency:tree` | Version im Catalog/BOM heben |
| Cargo | `cargo tree -i <crate>` | `cargo add <crate>` / `cargo update -p <crate>` |

`--force`/`--legacy-peer-deps`/`--no-deps` sind keine Lösung, sondern Verschiebung — nur mit Begründung.

## 4. API recherchieren — zwei Quellen

**a) Web (aktuell, mit Beispielen)** — Version in der URL auf die **aufgelöste** Version setzen (aus dem Lockfile):

| Ökosystem | Registry / Doku |
|---|---|
| npm | `https://www.npmjs.com/package/<pkg>`, Typen: mitgeliefert oder `@types/<pkg>` |
| Python | `https://pypi.org/project/<pkg>/<version>/`, Projekt-Doku (oft Read the Docs) |
| Go | `https://pkg.go.dev/<module>@<version>` |
| Java | `https://central.sonatype.com/artifact/<group>/<artifact>`, `https://javadoc.io/doc/<group>/<artifact>/<version>` |
| .NET | `https://www.nuget.org/packages/<pkg>/<version>` |
| Rust | `https://crates.io/crates/<crate>`, `https://docs.rs/<crate>/<version>/` |

**b) Lokal installierte Quelle (exakt die gebaute Version)**:

```bash
# Node
ls node_modules/<pkg>/ && cat node_modules/<pkg>/package.json | grep '"version"'
grep -rn "export declare function" node_modules/<pkg>/dist/*.d.ts
# Python (im aktiven venv)
python -c "import <pkg>, sys; print(<pkg>.__file__)"
# Go
ls "$(go env GOMODCACHE)"/<module-path>@<version>/
# Java
ls ~/.m2/repository/<group-als-pfad>/<artifact>/<version>/   # Sources-JAR ggf. per IDE/`-Dclassifier=sources`
# Rust
ls ~/.cargo/registry/src/index.crates.io-*/ | grep '^<crate>-'
```

Fehlermeldungen mit Pfaden **innerhalb** von `node_modules`/`site-packages`/Modul-Cache/Registry liegen in der Dependency — nicht dort „reparieren“; Version wechseln, Issue suchen, oder bewusst patchen (`patch-package`, `[patch]`, `replace`).

## 5. Arbeitsteilung mit Subagents

- Recherche (Registry-Seite, Advisories, lokale Quelle) an **fokussierte, parallele Subagents** delegieren — ein Paket pro Subagent.
- Subagents liefern zurück: **exakte CLI-Zeile** (inkl. Extras/Features/Scope), relevante Signatur, Lizenz, letzte Version, Advisory-Status, Doku-URL.
- Subagents **bauen nicht** und ändern keine Manifeste/Lockfiles (Race-Gefahr, Build-Last).
- Der Main-Loop führt die gemeldete CLI-Zeile aus, committet Manifest + Lockfile gemeinsam und baut/testet.

## Pin-Policy

- **Anwendungen**: exakte Auflösung über das committete Lockfile; Manifest mit kompatiblen Ranges ist ok.
- **Libraries**: möglichst **weite** kompatible Ranges im Manifest (sonst Konflikte beim Konsumenten); Lockfile nur für eigene Tests/CI.
- Exakte Pins im Manifest nur mit **Begründungskommentar** (bekannter Bug, API-Bruch in Minor).
- CI installiert **aus dem Lockfile** (`npm ci`, `uv sync --locked`, `poetry sync`, `cargo build --locked`, `go mod verify`).

## Fallstricke

- Version aus Gedächtnis/Blogpost übernehmen — veraltet oder erfunden.
- `latest`-Doku lesen, aber eine ältere Version installiert haben.
- Lockfile nicht committen oder bei Merge-Konflikten von Hand editieren → neu generieren.
- Global statt projektlokal installieren (`pip install` ohne venv, `npm install -g`).
- Advisory-Tools nur lokal statt in CI.

## Checkliste

1. Braucht es die Dependency wirklich?
2. Richtiger Name, Lizenz ok, gewartet, keine offenen Advisories?
3. Per Paketmanager-CLI ins richtige Paket/Workspace hinzugefügt — keine handgetippte Version?
4. Manifest + Lockfile gemeinsam committet?
5. API gegen Registry-Doku **und** lokale Quelle der aufgelösten Version geprüft?
6. Konflikte durch Hochziehen gelöst, Overrides begründet?
7. Build/Tests nach dem Hinzufügen im Main-Loop grün?
8. Rust zusätzlich: `rust-cargo-add-and-dep-research`.

## Quelle

- https://docs.npmjs.com/cli/commands/npm-install
- https://docs.astral.sh/uv/concepts/projects/dependencies/
- https://go.dev/ref/mod#go-get
- https://docs.gradle.org/current/userguide/version_catalogs.html
- https://learn.microsoft.com/en-us/dotnet/core/tools/dotnet-add-package
- https://doc.rust-lang.org/cargo/commands/cargo-add.html
- https://osv.dev/
