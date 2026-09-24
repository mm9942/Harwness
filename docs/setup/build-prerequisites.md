# Build-Voraussetzungen

Status: Ist (ab Welle W0b)

Dieses Dokument listet, was auf einer Maschine installiert und passend
konfiguriert sein muss, bevor `cargo build`/`cargo test`/`xtask` in diesem
Workspace überhaupt starten können — Werkzeugversionen und, für den
Raspberry Pi 5 als Zielplattform, Kernel-Eigenschaften, die kein
`Cargo.toml`-Eintrag erzwingen kann. Es ersetzt keine Installationsanleitung
(siehe dafür `harw-install`/`docs/design/CONTRACT-setup-install.md`) — es
hält fest, **welche** Voraussetzungen gelten und **warum**, damit ein
Abweichen sichtbar und nicht stillschweigend ist.

## 1. Rust-Toolchain: `1.98.1` (gepinnt)

Die Toolchain ist in **`rust-toolchain.toml`** im Repo-Wurzelverzeichnis
gepinnt (`channel = "1.98.1"`, Komponenten `rustfmt` und `clippy`, Profil
`minimal`). rustup liest die Datei automatisch – auch im eigenständigen
`dod/`-Workspace, der deshalb keine eigene Datei hat – und installiert die
Version beim ersten `cargo`-Aufruf nach. Die CI (`.github/workflows/ci.yml`,
`release.yml`) installiert genau diese Datei per `rustup toolchain install`;
lokal und in der CI prüfen damit derselbe Compiler, dasselbe `rustfmt` und
dasselbe `clippy`. Neue Versionen kommen bewusst über Dependabot
(Ökosystem `rust-toolchain`), nicht stillschweigend über `stable`.

Davon getrennt setzt die Workspace-Root-`Cargo.toml` `rust-version = "1.85"`
als MSRV-Boden – die *niedrigste* Version, gegen die dieser Workspace laut
Manifest kompilieren muss, keine Aussage über die geprüfte Toolchain.

```
$ rustup show active-toolchain
1.98.1-x86_64-unknown-linux-gnu (overridden by '…/rust-toolchain.toml')
```

## 2. `crypt_guard` `3.0.1` (crates.io, Hybrid-KEM)

`harw-secrets` hängt von `crypt_guard` in der **crates.io**-Version `3.0.1`
ab — nicht von einer Pfad-Abhängigkeit auf ein Nachbar-Repository, das auf
keiner bekannten Maschine existiert. Diese Version deckt reines ML-KEM für
die hier gebrauchte deterministische Seed→KEK-Ableitung nicht ab, weshalb
`harw-secrets` stattdessen eine Hybrid-KEM-Konstruktion einsetzt. Die
vollständige Begründung, die betroffenen Module
(`harw-secrets/src/{policy.rs,kek.rs,envelope.rs}`) und die Migrations-
entscheidung stehen in **`docs/setup/crypt-guard.md`** — dieses Dokument
verweist nur darauf, statt sie zu duplizieren.

## 3. Bubblewrap (`bwrap`)

`harw-sandbox`/`harw-tool-shell` isolieren Kindprozesse über Bubblewrap
(siehe `harw-sandbox/src/bwrap.rs`); `harw-install`s Doctor-Prüfung
`sandbox/bwrap` (`harw-install/src/doctor.rs:126-146`) sucht das Binary im
`PATH`. Auf Debian-artigen Systemen (Debian, Raspberry Pi OS) installiert
das Paket `bubblewrap` es unter dem festen Pfad `/usr/bin/bwrap`:

```
$ apt install bubblewrap
$ /usr/bin/bwrap --version
```

Fehlt `bwrap`, meldet der Doctor-Check das als
„Bubblewrap-Isolation nicht verfügbar" statt eines harten Abbruchs — die
Sandbox-Isolation ist damit aber tatsächlich nicht aktiv, kein bloßer
Warnhinweis ohne Konsequenz.

## 4. `prlimit` (aus `util-linux`)

Ressourcenlimits für Kindprozesse werden über `prlimit` gesetzt/gelesen,
nicht über eine reimplementierte `setrlimit`-Bibliothek. `prlimit` ist Teil
des `util-linux`-Pakets, das auf praktisch jeder Linux-Distribution ohnehin
zur Grundausstattung gehört (auch Raspberry Pi OS); es ist hier trotzdem
explizit als Voraussetzung aufgeführt, weil ein minimales Container- oder
Chroot-Image es auslassen kann.

```
$ prlimit --version
```

## 5. `git` ≥ `2.40`

Wird für Werkzeuge gebraucht, die auf modernere Git-Fähigkeiten setzen
(u. a. `git worktree`-Nutzung in der Remediation-Tooling-Kette und
partielle/sparse Checkouts an anderer Stelle im Ausbauprogramm). Ältere
`git`-Versionen (insbesondere die auf manchen Langzeit-Support-Distros
vorinstallierten 2.3x-Stände) unterstützen einzelne dieser Flags nicht oder
verhalten sich dabei abweichend.

```
$ git --version
git version 2.40.0 (oder neuer)
```

## 6. Raspberry Pi 5 (aarch64) — abweichendes Kernel-Verhalten

Der Raspberry Pi 5 ist eine Zielplattform dieses Projekts (siehe
`user-sway-setup`/Mia-TV-Desktop-Kontext), aber sein Standard-Kernel weicht
in drei sicherheitsrelevanten Punkten von einem gewöhnlichen
x86_64-Server-Kernel ab. Alle drei wirken sich auf privilegierte Warden-/
Sentinel-Binaries aus, nicht auf einen gewöhnlichen `cargo build`:

- **Kein Landlock.** Der auf Raspberry Pi OS ausgelieferte Kernel bringt
  standardmäßig keine Landlock-Unterstützung mit. Der Verifikationsplan
  behandelt das bereits als eigenen Fall (`docs/aw-plan.md:921`,
  Entscheidung Nr. 4 „Landlock ohne Kernelunterstützung"): **asymmetrisch**
  behandelt — harter Startfehler für die drei privilegierten Binaries,
  Degradation mit `SensorDegraded` für den Sentinel. Ein Build auf einem
  Pi 5 mit Standard-Kernel muss also mit genau diesem Verhalten rechnen,
  nicht mit einem stillschweigenden Fallback.
- **Kein BTF** (BPF Type Format) im Standard-Kernel-Image. Jede
  BPF-gestützte Beobachtung (`harw-probe-bpf`, `harw-dod-bpf`), die auf
  `CO-RE` (Compile Once – Run Everywhere) über BTF setzt, braucht entweder
  einen eigens gebauten Kernel mit `CONFIG_DEBUG_INFO_BTF=y` oder einen
  Verzicht auf die BTF-gestützten Pfade zugunsten der jeweiligen
  Degradations-Strategie.
- **16K-Seitengröße.** Aktuelle Raspberry-Pi-OS-Kernel bieten (teils als
  Standard, teils als Option) eine Seitengröße von 16 KiB statt der auf den
  meisten Linux-Systemen üblichen 4 KiB. Code, der Seitengröße annimmt statt
  sie über `sysconf(_SC_PAGESIZE)`/das Rust-Äquivalent zu erfragen, oder der
  mit gemappten Speicherbereichen in 4-KiB-Schritten rechnet, liefert auf
  dieser Plattform falsche Ergebnisse, ohne dass der Build selbst
  fehlschlägt.
- **`cgroup_disable=memory`.** Manche Raspberry-Pi-OS-Images setzen dieses
  Boot-Argument standardmäßig (ursprünglich, um dem Pi angesichts
  begrenzten RAMs das Cgroup-Memory-Accounting zu ersparen). Jede Komponente,
  die Speicherlimits oder -zähler über die Memory-Cgroup liest
  (`harw-dod-cgroup`, `harw-dod-memory`), sieht auf einer so konfigurierten
  Maschine keine Daten — das ist von einem echten Lesefehler zu
  unterscheiden. Wer auf dem Pi 5 mit aktivem Memory-Cgroup-Accounting
  arbeiten will, muss `cgroup_enable=memory cgroup_memory=1` explizit in
  `/boot/firmware/cmdline.txt` ergänzen und neu starten.

Keiner dieser vier Punkte verhindert einen `cargo build` auf dem Pi 5 selbst
— sie verändern das **Laufzeitverhalten** der privilegierten Binaries und
der DoD-Sensoren gegenüber einem gewöhnlichen Entwicklungsrechner.
