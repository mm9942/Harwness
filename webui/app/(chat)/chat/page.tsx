// Master-Chat — die Sitzung, die an keinen Auftrag gebunden ist (Knoten UI-02).
//
// Rendert unter `/chat`. Die Route-Gruppe `(chat)` selbst erzeugt kein
// URL-Segment; das eigentliche Segment ist `chat/`.
import type { JSX } from "react";

import { ChatView } from "./_components/ChatView";

export default function MasterChatPage(): JSX.Element {
  return <ChatView scope={{ kind: "master" }} title="Master-Chat" />;
}
