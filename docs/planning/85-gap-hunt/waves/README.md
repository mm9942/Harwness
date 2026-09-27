# Wellen-Manifeste

Ein Manifest pro gemergter Fix-Welle, geschrieben von
[`kit/wave_manifest.py`](../kit/wave_manifest.py) und auf der Branch der Welle
über ihrem Inhalts-Commit committet. Es hält fest:

- `base_sha`: der Commit, von dem die Branch der Welle geschnitten wurde;
- `branch` und `head_sha`: die Branch und ihr Inhalts-Commit;
- `files`: die genaue Dateimenge, die die Welle ändern durfte;
- `findings`: stabile IDs (`F-` + SHA-1 von `datei:zeile:titel`, 12 Zeichen)
  mit Datei, Zeile, Schwere, Muster und Titel;
- `workflow_runs`: die Run-IDs aller Workflows, die geschrieben oder geprüft
  haben;
- `disposition`: was der Workflow zurückgab (`complete`, offene Dateien,
  Ripple-Status und Ripple-IDs, Cluster-Status);
- `central_build`: die Befehle, die der zentrale Build über den finalen
  Integrations-Stand laufen lassen muss.

Manifeste werden nie geändert. Wird eine Welle neu geschnitten, bekommt sie
einen neuen Namen. Die Manifeste der Wellen `wa-egress`, `wa-authz`,
`contract-a`, `contract-b` und `wa-web` wurden nach dem Merge nachgetragen
(Feld `note`); die ihrer Ripple-Befunde liegen bei den Folgewellen.
