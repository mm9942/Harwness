// Sicherheitszentrale (Knoten UI-05).
//
// Rendert unter `/security`. Die Route-Gruppe `(security)` selbst erzeugt
// kein URL-Segment; das eigentliche Segment ist `security/`.
import type { JSX } from "react";

import { SecurityView } from "./_components/SecurityView";

export default function SecurityPage(): JSX.Element {
  return <SecurityView />;
}
