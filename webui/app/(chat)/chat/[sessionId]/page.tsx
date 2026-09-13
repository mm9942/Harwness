// Session-Chat — der Verlauf einer bestimmten Sitzung (Knoten UI-02).
//
// Rendert unter `/chat/[sessionId]`. Anders als der Master-Chat ist jeder
// Aufruf hier an eine konkrete `sessionId` gebunden — sie geht in jeden
// Operationsaufruf ein (siehe `_lib/chatOperations.ts::buildSendBody`).
// Next.js liefert `params` seit Version 15 als Promise; `await` auf einen
// bereits aufgelösten Wert ist unschädlich, falls eine ältere Next-Version
// zum Einsatz kommt.
import type { JSX } from "react";

import { ChatView } from "../_components/ChatView";

interface SessionChatPageProps {
  readonly params: Promise<{ readonly sessionId: string }>;
}

export default async function SessionChatPage({ params }: SessionChatPageProps): Promise<JSX.Element> {
  const { sessionId } = await params;
  return <ChatView scope={{ kind: "session", sessionId }} title={`Session-Chat: ${sessionId}`} />;
}
