import type { ConversationView } from "@peppy/desktop-ui/bridge-contracts";

export const conversationWireDefaults = {
  recipientlessDraft: false,
  draftOnly: false,
  pinned: false,
  folder: "inbox",
  canUnblock: false,
} satisfies Partial<ConversationView>;
