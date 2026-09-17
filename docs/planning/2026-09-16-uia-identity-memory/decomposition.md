# Decomposition — UIA-Identität, Memory-Root und Pflege-Werkzeuge

**Datum:** 2026-09-16
**Auftrag:** Ausbau der UIA-Agenten-Identität um `identity.md`, ein
agenten-eigenes `memory/`-Verzeichnis und drei Pflege-Werkzeuge
(`identity.md`, `USER.md`, `Personality.md`). Reine Planung, keine Umsetzung.

## Zerlegung in Teilprobleme

1. **Bestandsaufnahme bestehender Speicher-/Identitäts-Infrastruktur**
   (siehe `dependency-research.md`) — Pflicht vor jeder Struktur-Entscheidung,
   da mindestens drei überlappende Systeme existieren (`harw-memory`,
   `harw-knowledge`, UIA-Bundle-Dateien).
2. **Abgrenzung `identity.md` vs. `Personality.md`** — inhaltlich, nicht nur
   nominell, mit Beispielen.
3. **Abgrenzung Memory (`memory/`) vs. Personality/Identity** — Memory ist
   gelernter, projekt- und zeitbezogener Inhalt; Personality/Identity sind
   vom Menschen (oder mit Bestätigung vom Agenten selbst) kuratierte,
   stabile Selbstbeschreibung.
4. **Architektur-Entscheidung**: bestehendes `harw-memory`-Crate um eine
   Pro-Agent-Root + projekt-/datumsbezogene Organisationsebene erweitern,
   vs. komplett neu bauen.
5. **Verzeichnis-/Dateistruktur** für `~/.harw/agents/<uia>/identity.md` und
   `~/.harw/agents/<uia>/memory/`.
6. **Drei Pflege-Werkzeuge** — Signatur, Rollen-/Freigabe-Bindung, Verhältnis
   zu den bereits bestehenden `agents.write_uia`/`agents.write_definition`
   (Steward-Werkzeuge) und der `DefaultApprovalPolicy`.
7. **Migrationspfad** für bereits bestehende UIA-Verzeichnisse (z. B.
   `emily-ui`) ohne `identity.md`/`memory/`.
8. **Test- und Fehlerplan** für eine spätere Umsetzung.

## Nicht Teil dieses Auftrags

- Keine Implementierung von Code, Tests oder Migrationen.
- Keine Änderung an `dream_reflection`/`DreamReport` selbst — nur Klärung,
  was real existiert und wie es sich zum neuen Memory-Root verhält.
- Keine Entscheidung über LLM-basierte Konsolidierungsinhalte (Prompts) —
  nur die Struktur, die eine spätere Konsolidierung befüllen würde.
