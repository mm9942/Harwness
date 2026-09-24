# Skill: Schematron-Beschaffung für europäische e-Invoice-Standards

## Zweck

Dieser Skill wird aufgerufen wenn:
- Schematron/XSLT-Artefakte für ein bestimmtes e-Invoice-Format beschafft werden sollen
- Die aktuellen Versionen in `resources/` gegen offizielle Releases geprüft werden sollen
- Neue Formate/Profile in die Validierungs-Pipeline aufgenommen werden sollen
- Das `resources/manifests/routes.json` aktualisiert werden soll

## Schritt 1 — Format und Umfang klären

Frage (wenn nicht angegeben):
- Welches Format? (EN16931, Factur-X, XRechnung, PEPPOL, ...)
- Welche Syntax? (CII / UBL / beide)
- Welche Profile? (bei Factur-X: MINIMUM / BASIC-WL / BASIC / EN16931 / EXTENDED)
- Nur prüfen oder auch herunterladen?

## Schritt 2 — Offizielle Quellen (kanonische Release-URLs)

### CEN EN16931 — Kern-Schematron (UBL + CII)
- **Repo**: https://github.com/ConnectingEurope/eInvoicing-EN16931
- **Aktuelle Version**: v1.3.16 (April 2026)
- **Release-Seite**: https://github.com/ConnectingEurope/eInvoicing-EN16931/releases/latest
- **Artefakte**:
  - CII XSLT: `cii/xslt/EN16931-CII-validation.xslt`
  - UBL Invoice XSLT: `ubl/xslt/EN16931-UBL-validation.xslt`
  - UBL CreditNote XSLT: `ubl/xslt/EN16931-UBL-validation.xslt` (gleiche Datei)
- **Download-Befehl**:
  ```bash
  TAG=$(curl -s https://api.github.com/repos/ConnectingEurope/eInvoicing-EN16931/releases/latest | jq -r .tag_name)
  curl -L "https://github.com/ConnectingEurope/eInvoicing-EN16931/archive/refs/tags/${TAG}.zip" -o en16931-${TAG}.zip
  ```

### Factur-X 1.08 / ZUGFeRD 2.4 (alle Profile, CII D22B)
- **Offizielle Seite FR**: https://fnfe-mpe.org/factur-x/
- **Offizielle Seite DE**: https://www.ferd-net.de/ZUGFeRD-Download
- **Aktuelle Version**: 1.08 / ZUGFeRD 2.4 (in Kraft ab 15. Januar 2026)
- **Profile und ihre Schematron/XSLT-Artefakte** (im offiziellen Paket unter `Schema/`):
  | Profil | Ordner | XSLT-Pfad |
  |--------|--------|-----------|
  | MINIMUM | `1_Factur-X_1.08_MINIMUM/` | `_XSLT_MINIMUM/FACTUR-X_MINIMUM.xslt` |
  | BASIC-WL | `2_Factur-X_1.08_BASIC-WL/` | `_XSLT_BASIC-WL/FACTUR-X_BASIC-WL.xslt` |
  | BASIC | `2a_Factur-X_1.08_BASIC/` | `_XSLT_BASIC/FACTUR-X_BASIC.xslt` |
  | EN16931 | `3_Factur-X_1.08_EN16931/` | `_XSLT_EN16931/FACTUR-X_EN16931.xslt` |
  | EXTENDED | `4_Factur-X_1.08_EXTENDED/` | `_XSLT_EXTENDED/FACTUR-X_EXTENDED.xslt` |
  | XRECHNUNG | `5_Factur-X_1.08_XRECHNUNG/` | `_XSLT_XRECHNUNG/FACTUR-X_XRECHNUNG.xslt` |
- **Hinweis**: Jedes Profil hat auch eine `.sch`-Quelldatei und eine `_codedb.xml` die von der XSLT per `document()` geladen wird — beide müssen in `resources/` liegen.
- **Download**: ZIP-Paket manuell von fnfe-mpe.org herunterladen (kein direkter API-Zugriff)

### XRechnung Schematron (KoSIT)
- **Repo**: https://github.com/itplr-kosit/xrechnung-schematron
- **Aktuelle Version**: v2.5.0 für XRechnung 3.0.2 (Februar 2026)
- **Release-Seite**: https://github.com/itplr-kosit/xrechnung-schematron/releases/latest
- **Artefakte** (im Release-ZIP):
  - CII: `XRechnung-CII-validation.xslt`
  - UBL: `XRechnung-UBL-validation.xslt`
- **Abhängigkeit**: Erfordert CEN EN16931 CII-XSLT im Stack (wird vorher ausgeführt)
- **Download-Befehl**:
  ```bash
  TAG=$(curl -s https://api.github.com/repos/itplr-kosit/xrechnung-schematron/releases/latest | jq -r .tag_name)
  curl -L "https://github.com/itplr-kosit/xrechnung-schematron/releases/download/${TAG}/xrechnung-schematron-${TAG}.zip" -o xrechnung-schematron-${TAG}.zip
  ```

### PEPPOL BIS Billing 3.0 (OpenPEPPOL)
- **Repo**: https://github.com/OpenPEPPOL/peppol-bis-invoice-3
- **Aktuelle Version**: 3.0.20-hotfix, Validierungsartefakte v1.3.15 (Oktober 2025)
- **Verpflichtend ab**: 23. Februar 2026
- **Release-Seite**: https://github.com/OpenPEPPOL/peppol-bis-invoice-3/releases/latest
- **Artefakte** (wichtig: ZWEI Schematrons pro Syntax):
  | Artefakt | Zweck | Regeln-Präfix |
  |----------|-------|---------------|
  | `PEPPOL-EN16931-UBL.xslt` | PEPPOL-spezifische Regeln | `PEPPOL-BIS-R*` |
  | `CEN-EN16931-UBL.xslt` | CEN-Basisregeln | `BR-*` |
  | `PEPPOL-EN16931-CII.xslt` | PEPPOL-CII | `PEPPOL-BIS-R*` |
  | `CEN-EN16931-CII.xslt` | CEN-CII | `BR-*` |
- **Download-Befehl**:
  ```bash
  TAG=$(curl -s https://api.github.com/repos/OpenPEPPOL/peppol-bis-invoice-3/releases/latest | jq -r .tag_name)
  curl -L "https://github.com/OpenPEPPOL/peppol-bis-invoice-3/archive/refs/tags/${TAG}.zip" -o peppol-bis-${TAG}.zip
  ```

### AGID PEPPOL (Italien)
- **Repo**: https://github.com/AgID/agid-invoice-pa
- **Artefakt**: `AGID-EN16931-UBL-PEPPOL-ITA.xsl` (ergänzt PEPPOL-UBL)
- **Stack**: PEPPOL-EN16931-UBL.xslt → AGID-ITA.xsl (in dieser Reihenfolge)

## Schritt 3 — Versionsabgleich mit aktuellem Stand

Prüfe die aktuell installierten Versionen:
```bash
# XSLT-Header lesen (enthält meist Versionsinformation)
head -20 resources/cen-en16931/EN16931-CII-validation.xslt
head -20 resources/xrechnung/XRechnung-CII-validation.xslt
head -20 resources/peppol/xslt/PEPPOL-EN16931-UBL.xsl

# SHA256 der aktuellen Dateien
sha256sum resources/cen-en16931/EN16931-CII-validation.xslt
sha256sum resources/xrechnung/XRechnung-CII-validation.xslt
```

Vergleiche mit den SHA256-Werten in `resources/manifests/routes.json` (Feld `sha256`).

## Schritt 4 — Neue Artefakte einbinden

### 4a. Datei platzieren

Zielstruktur in `resources/`:
```
resources/
  cen-en16931/
    EN16931-CII-validation.xslt      ← CEN CII
    EN16931-UBL-validation.xslt      ← CEN UBL (NEU)
  factur-x/
    minimum/
      FACTUR-X_MINIMUM.xsd
      FACTUR-X_MINIMUM.xslt
      FACTUR-X_MINIMUM_codedb.xml
    basic-wl/  ...
    basic/     ...
    en16931/   (vorhanden)
    extended/  (vorhanden)
    xrechnung/
      FACTUR-X_XRECHNUNG.xslt
      FACTUR-X_XRECHNUNG_codedb.xml
  xrechnung/
    XRechnung-CII-validation.xslt   (vorhanden)
    XRechnung-UBL-validation.xslt   (vorhanden)
  peppol/
    xslt/
      PEPPOL-EN16931-UBL.xsl        (vorhanden)
      CEN-EN16931-UBL.xslt          ← NEU (falls separates Artefakt benötigt)
      PEPPOL-EN16931-CII.xslt       ← NEU
      AGID-EN16931-UBL-PEPPOL-ITA.xsl (vorhanden)
    schema/ubl-2.1/  (vorhanden)
```

### 4b. SHA256 berechnen
```bash
sha256sum resources/factur-x/minimum/FACTUR-X_MINIMUM.xslt
```

### 4c. routes.json aktualisieren

Neuen Eintrag in `artifacts[]` hinzufügen:
```json
{
  "artifact_id": "facturx-1.08-minimum-xslt",
  "kind": "schematron_xslt",
  "path": "resources/factur-x/minimum/FACTUR-X_MINIMUM.xslt",
  "sha256": "<SHA256>"
}
```

Neuen Eintrag in `routes[]` hinzufügen:
```json
{
  "route_id": "cii.facturx.minimum",
  "syntax": "cii",
  "profile": "factur_x_minimum",
  "family": "invoice",
  "match": {
    "guideline_contains": ["minimum"],
    "customization_contains": [],
    "profile_contains": []
  },
  "xsd": ["facturx-1.08-minimum-xsd"],
  "schematron_xslt": ["facturx-1.08-minimum-xslt"],
  "semantic_rule_sets": ["minimum-arithmetic-v1"]
}
```

## Schritt 5 — Verifikation

```bash
# Cargo check (kein cargo build/test direkt!)
# → make check im Projektstamm
# XSLT-Syntax prüfen:
xmllint --noout resources/factur-x/minimum/FACTUR-X_MINIMUM.xslt 2>&1
```

## Aktueller Lückenstatus (Stand Juli 2026)

| Artefakt | Status | Hinweis |
|----------|--------|---------|
| CEN EN16931 UBL-XSLT | ✅ vorhanden | v1.3.16, ConnectingEurope GitHub |
| CEN EN16931 CII-XSLT | ✅ vorhanden | v1.3.16, ConnectingEurope GitHub |
| Factur-X 1.09 MINIMUM (xsd + xslt + codedb) | ✅ vorhanden | LandrixSoftware/validator-configuration-zugferd |
| Factur-X 1.09 BASIC-WL (xsd + xslt + codedb) | ✅ vorhanden | LandrixSoftware/validator-configuration-zugferd |
| Factur-X 1.09 BASIC (xsd + xslt + codedb) | ✅ vorhanden | LandrixSoftware/validator-configuration-zugferd |
| Factur-X 1.08 EN16931 (xsd + xslt + codedb) | ✅ vorhanden | ZF24/Schema |
| Factur-X 1.08 EXTENDED (xsd + xslt + codedb) | ✅ vorhanden | ZF24/Schema |
| PEPPOL BIS UBL-XSLT | ✅ vorhanden | v3.0.20-hotfix, OpenPEPPOL |
| PEPPOL BIS CII-XSLT | ❌ fehlend | Nur .sch-Quelle; Saxon-Build für XSLT nötig |
| Factur-X XRECHNUNG XSLT | ❌ fehlend | Nicht in LandrixSoftware-ZIP; FeRD-Paket nötig |

### Route-Matching-Besonderheiten (CRITICAL)
- BASIC-WL guideline URI: `urn:factur-x.eu:1p0:basicwl` (KEIN Bindestrich!)  
  → routes.json muss `"basicwl"` nicht `"basic-wl"` matchen
- BASIC guideline URI: `urn:cen.eu:en16931:2017#compliant#urn:factur-x.eu:1p0:basic`  
  → routes.json muss `["compliant", "factur-x.eu:1p0:basic"]` verwenden — sonst matcht BASIC-WL fälschlicherweise als BASIC

### Test-Fixtures (Stand Juli 2026)
Synthetische CII-Fixtures unter `xmlinvoice/tests/fixtures/`:
- `facturx_minimum_cii.xml` — guideline `urn:factur-x.eu:1p0:minimum`
- `facturx_basicwl_cii.xml` — guideline `urn:factur-x.eu:1p0:basicwl`
- `facturx_basic_cii.xml` — guideline `urn:cen.eu:en16931:2017#compliant#urn:factur-x.eu:1p0:basic`
- `peppol_bis_ubl_base.xml` — OpenPEPPOL base-example.xml v3.0.20 (echt)

## Constraint

Änderungen nur in `xmlinvoice/` — niemals in `../apps/acme-app/` oder anderen Workspace-Crates.
