# H18 — Authentifizierter Participant-Beitritt: Contract-Draft

Status: Completed (Vertrag abgeschlossen; Implementierung h19 offene Punkte am Ende).

## Befundene Fakten (mit Belegen)

1. **resolve_session mit NoMatch/Ambiguous**: Sessions-Auflösung liefert diskrete Ergebnisse `NoMatch` und `Ambiguous` — fail-closed, kein „irgendeine“ Session. Beleg: `attach.rs:100-129`.
2. **Attach über SessionPort**: Der Beitritt läuft über den `SessionPort`; keine direkten internen Zustandsgriffe. Beleg: `attach.rs:518-555`.
3. **ClientIdentity**: Identität umfasst Principal, Tenant und Capabilities; Capabilities sind getrennt verwaltet. Belege: `identity.rs:252-269`, `identity.rs:336-346`, `identity.rs:349-362`.
4. **Attach benötigt `observe`**: Ohne die Capability `observe` wird der Attach verweigert. Beleg: `host.rs:1051-1057`.
5. **detach_sync + Presence**: Abmelden ist synchronisiert; Presence-Verwaltung gehört zum Host. Belege: `host.rs:1193-1207`, `host.rs:680-683`.

## Lücken (nicht belegt, daher im Vertrag als Lücken markiert)
- **Explizite Zielbestätigung fehlt im Attach-Loop**: Es gibt keine verifizierte Nutzerbestätigung der Zielsession vor dem Beitritt.
- **Kein Remote-Attach in der CLI**: `session_cmd.rs` arbeitet auf lokalen Transcripts; Remote-Attach ist über die CLI nicht belegt.
- **Telegram-Pairing ist getrennt**: `connect.rs` implementiert ein eigenes Pairing, das nicht Teil des Attach-Flows ist.
- **Reconnect**: nur als logischer Re-Attach belegt — kein dedizierter Session-Wiederverbindungsmechanismus.

## Vertragsspezifikation

### Discovery / Auswahl
- Sessions werden aufgelöst über `resolve_session`-Semantik: `NoMatch` und `Ambiguous` sind harte Fehler mit eindeutiger Nutzerführung (NeuverSuche/Disambiguierung), kein stiller Fallback. (Befund 1)

### Explizite Session-Zielbestätigung (schließt Lücke 1)
- Vor dem Attach muss die ausgewählte Zielsession dem Nutzer eindeutig angezeigt und bestätigt werden (z. B. Session-ID/Kurzname + Host). Ohne Bestätigung: kein Attach. Dies ist ein Vertragszusatz, der im aktuellen Code noch nicht implementiert ist.

### Attach / Presence
- Attach ausschließlich über `SessionPort` (Befund 2), nie über interne Zustandsgriffe.
- Identität: Client weist sich mit Principal/Tenant/Capabilities aus (Befund 3); der Host validiert alle drei.
- Autorisierung: `observe`-Capability ist notwendig (Befund 4); weitere Capability-Prüfungen je Operation.
- Nach erfolgreichem Attach ist der Client präsent (Host-managed Presence, Befund 5).

### Autorisiertes Detach / Reconnect
- Detach läuft über `detach_sync` — synchron, mit Bestätigung des Abschlusszustands (Befund 5). Detach ohne Identity/ohne vorherigen Attach wird abgelehnt.
- Reconnect ist als logischer Re-Attach definiert: gleicher Client-Identity-Prüfpfad, gleicher Capability-Check; kein Batchfahren von Session-Handles.

### Fail-closed
- Bei unbekannter, mehrdeutiger, falscher oder nicht autorisierter Zielsession: kein Beitritt, Fehlermeldung mit Ursache, keine Teil-Präsenz, kein Fallback auf „nächste beste“ Session.

## Offene Punkte
- Explizite Zielbestätigung im Attach-Loop (Vertragszusatz, Implementierung offen).
- Remote-Attach in der CLI (aktuell nur lokale Transcripts).
- Abgrenzung/Verhältnis zum Telegram-Pairing in `connect.rs`.
- Reconnect-Mechanik jenseits des logischen Re-Attach.

## Tests
- `resolve_session`: NoMatch-, Ambiguous-, eindeutiger Treffer.
- Attach ohne `observe`-Capability → Abweisung.
- Attach mit ungültigem Principal/Tenant → Abweisung, keine Präsenz.
- Detach ohne vorherigen Attach → Abweisung; Detach mit Präsenz-Abbau synchron.
- Reattach nach Detach mit denselben Prüfungen wie Erst-Attach.
- Zielbestätigung: kein Attach ohne Bestätigung (Test, sobald implementiert).
