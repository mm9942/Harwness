# Harwness — Entwicklungs-Targets.
#
# `clippy-tests` ist das kanonische Verifikationskommando dieses Repos: es führt
# Clippy über den gesamten Workspace inklusive Test-Targets mit
# `-D warnings` aus und lässt anschließend die Testsuite laufen. Kein direkter
# `cargo`-Aufruf außerhalb dieser Targets.
#
# `gates` ruft `xtask gates` auf — die Schreibbereichs- und Struktur-Gates
# des Ausbauprogramms, die auf `WorkspaceGraph::load` aufbauen. Es hängt
# NICHT an `check`: `check` soll ein schneller Typecheck bleiben, den man
# beliebig oft am Stück laufen lässt, und ein Gate, das jeden `check`
# verlangsamt, wird in der Praxis umgangen (z. B. mit `cargo check` direkt).
# Es hängt an `clippy-tests`, dem kanonischen Verifikationskommando vor einem
# Commit/einer PR — dort ist ein zusätzlicher Lauf akzeptabel und genau dort
# darf ein rotes Gate nicht mehr übersehen werden. `gates` bleibt zusätzlich
# einzeln aufrufbar, denn ein Gate, das man extra aufrufen muss, wird nicht
# aufgerufen — es muss auch im Normalbetrieb mitlaufen.

CARGO ?= cargo
BINDIR ?= $(HOME)/.local/bin

.PHONY: clippy-tests clippy tests fmt check build install service gates

## Kanonische Verifikation: Clippy (inkl. Tests, warnings = Fehler) + Testsuite + Gates.
clippy-tests:
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings
	$(CARGO) test --workspace --all-features
	$(MAKE) gates

## Schreibbereichs- und Struktur-Gates des Ausbauprogramms (`xtask gates`).
gates:
	$(CARGO) run -q -p xtask -- gates

## Nur Clippy (inkl. Test-Targets), warnings als Fehler.
clippy:
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

## Nur die Testsuite.
tests:
	$(CARGO) test --workspace --all-features

## Formatprüfung ohne Änderung.
fmt:
	$(CARGO) fmt --all --check

## Schneller Typecheck des gesamten Workspace.
check:
	$(CARGO) check --workspace --all-features

## Release-Build des `harw`-Binaries.
build:
	$(CARGO) build --release --bin harw

## Baut `harw` und installiert es nach $(BINDIR) (default ~/.local/bin).
install: build
	install -Dm755 target/release/harw $(BINDIR)/harw
	@echo "harw installiert nach $(BINDIR)/harw"

## Installiert die systemd-User-Unit (harw gateway als Hintergrund-Daemon).
service: install
	$(BINDIR)/harw service install
	systemctl --user daemon-reload
	systemctl --user enable --now harw.service
	@echo "harw.service (gateway) aktiviert."
