# HARW Tunnel Policy — v1

> Status: planned contract · Scope: managed SSH tunnels in `harw-tool-tunnel`.

This document defines the security and policy boundary for the first version of the HARW tunnel tool. The contract is intentionally narrower than general SSH forwarding: v1 creates only local forwards, binds them only to loopback, and permits remote destinations only through an explicit target allowlist.

## 1. Zweck und Geltungsbereich

`harw-tool-tunnel` verwaltet SSH-Portforwards im HARW-Job-/WorkDriver-Lifecycle. v1 unterstützt ausschließlich lokale SSH-Forwardings (`-L`): ein lokaler Loopback-Port wird über eine SSH-Verbindung zu einem Zielhost und Zielport auf der entfernten Seite weitergeleitet.

Das Werkzeug muss Policy-Prüfung, Freigabe, Secret-Redaktion und kontrollierte Wiederverbindung in den verwalteten Lifecycle integrieren. Dieses Dokument legt die verbindlichen Grenzen fest; es beschreibt keine konkrete CLI- oder Konfigurationssyntax.

## 2. Ziel-Allowlist: Hosts und Ports

Jeder Forward muss einem expliziten Allowlist-Eintrag entsprechen. Ein Eintrag definiert mindestens:

- den Zielhost auf der entfernten Seite (DNS-Name oder IP-Adresse; Namen werden als vollständige Hostnamen, nicht als implizite Wildcards, behandelt),
- den erlaubten Zielport oder eine explizit konfigurierte Menge/einen Bereich von Zielports,
- ob das Ziel als privat eingestuft ist und dafür eine gesonderte Erlaubnis vorliegt.

Vor Start und vor jeder Wiederverbindung muss das Werkzeug Host und Port gegen die wirksame Allowlist prüfen. Eine Übereinstimmung erfordert, dass Host und Port beide zugelassen sind; fehlende, mehrdeutige oder ungültige Einträge führen zur Ablehnung. Wildcards dürfen nicht stillschweigend aus einem Hostnamen abgeleitet werden.

Ziele im privaten Adressraum (RFC 1918) sowie Loopback- bzw. localhost-Ziele, die vom Fernhost aus erreichbar sind, sind standardmäßig verboten. Sie dürfen nur verwendet werden, wenn die Policy sie für das konkrete Ziel ausdrücklich erlaubt. Eine allgemeine Freigabe des Tunnels oder des SSH-Servers hebt dieses Default-Verbot nicht auf.

### 2.1 Auflösung von Hostnamen (Implementierungsstand)

`ssh -L` löst einen Ziel-Hostnamen auf dem **SSH-Server** auf. Ob ein erlaubter
Name dort auf eine private oder Loopback-Adresse zeigt (auch per DNS-Rebinding),
ist lokal weder prüfbar noch festnagelbar. Deshalb gilt in der Umsetzung
(`harw-tool-tunnel/src/policy.rs`): Literal-IP-Ziele werden gegen das
Default-Verbot geprüft (inklusive IPv4-mapped IPv6); ein Hostname-Ziel ist nur
mit ausdrücklicher Zustimmung `allow_remote_resolution` im Allowlist-Eintrag
zulässig, und diese Zustimmung bedeutet ausdrücklich, dass das Default-Verbot für
private Ziele für diesen Namen **nicht** durchgesetzt wird. Eine
durchgesetzte Prüfung bräuchte ein vertrauenswürdiges Egress-Gate auf der
Gegenseite.

## 3. Bindungs-Regel

Der lokale Listen-Endpunkt eines Forwards muss ausschließlich an `127.0.0.1` oder `::1` gebunden sein. Jede andere Bind-Adresse ist unzulässig. Insbesondere darf der Listener niemals an `0.0.0.0`, `::` oder einer LAN-/öffentlichen Adresse lauschen. Eine fehlende Bind-Adresse darf nicht zu einer breiteren Bindung führen; sie muss auf eine der beiden Loopback-Adressen festgelegt oder abgelehnt werden.

## 4. Approval-Regel beim Start

Eine explizite Freigabe ist beim ersten Start eines Tunnels erforderlich. Eine erneute Freigabe ist ebenfalls erforderlich, sobald sich die wirksame Ziel-Allowlist ändert; eine frühere Freigabe gilt dann nicht als Zustimmung zu den geänderten Zielen oder Ports.

Ohne gültige Freigabe darf der Tunnel nicht gestartet oder nach einem Abbruch automatisch wiederhergestellt werden. Die Freigabe muss sich auf die wirksame Tunnelkonfiguration einschließlich erlaubter Ziele und lokaler Loopback-Bindung beziehen. Ein Reconnect darf eine erteilte Freigabe nur weiterverwenden, solange diese Konfiguration unverändert und weiterhin policy-konform ist.

## 5. Netzwerkpfad und Relay: Egress-Policy

Der SSH-Verbindungsweg und jeder Relay-/Host-Kontext müssen mit der HARW-Egress-Policy vereinbar sein. Das Werkzeug darf Egress-Regeln nicht umgehen, indem es einen Tunnel als generischen Relay-Kanal oder den entfernten SSH-Host als unbeschränkten Stellvertreter verwendet.

Die Policy-Prüfung muss den tatsächlichen Netzwerkpfad berücksichtigen: den ausgehenden SSH-Endpunkt im lokalen Ausführungskontext sowie das durch den Forward erreichte Ziel im Kontext des Fernhosts. Wo ein Relay eingesetzt wird, muss auch dessen Identität bzw. Kontext in der Egress-Policy zugelassen sein; ein Relay darf keine weiter gefasste Ziel-Allowlist begründen. Nicht klassifizierte oder durch die Egress-Policy nicht freigegebene Pfade sind abzulehnen. Dieselben Prüfungen gelten bei Start und Reconnect.

## 6. Layer-Zuordnung

Für das geplante Crate `harw-tool-tunnel` ist ein Paket-Eintrag in `xtask/arch-policy.toml` vorzusehen. Die Layer-Zuordnung muss dessen Rolle als verwaltetes Laufzeit-/Job-Werkzeug abbilden und die dort geltenden Abhängigkeitsregeln einhalten. Dieser Vertrag fordert die spätere Klassifikation; `xtask/arch-policy.toml` wird durch dieses Dokument nicht geändert.

## 7. Schlüsselmaterial: Keyfile-Referenz statt Inhalt

Konfiguration und Prozessübergabe dürfen nur eine Referenz auf eine Schlüsseldatei enthalten, niemals den Inhalt eines privaten Schlüssels. Privater Schlüsselinhalt darf weder als Prozessargument noch in Logs, Fehlermeldungen, Approval-Anzeigen, Artefakten oder sonstigen persistenten bzw. diagnostischen Ausgaben erscheinen.

Das Werkzeug muss sensible Werte vor jeder Ausgabe redigieren. Die Referenz selbst ist kein Schlüsselinhalt; Pfade oder Metadaten, die ihrerseits als sensibel eingestuft sind, müssen entsprechend der Secret- und Audit-Policy geschützt bzw. redigiert werden. Schlüsseldateien dürfen nicht in Tunnel-Artefakte kopiert oder eingebettet werden.

## 8. Grenzen von v1

v1 umfasst ausschließlich lokale Forwards (`-L`). Nicht unterstützt und nicht stillschweigend zuzulassen sind:

- Remote-Forwardings (`-R`),
- SOCKS-Forwards (`-D`),
- ein eigener, vom verwalteten HARW-Lifecycle unabhängiger Daemon.

Jede spätere Erweiterung dieser Grenzen erfordert eine eigene Policy- und Approval-Prüfung; sie ist nicht durch diesen Vertrag freigegeben.
