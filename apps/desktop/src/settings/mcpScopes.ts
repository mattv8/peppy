import type { McpScope } from "@peppy/desktop-ui/bridge-contracts";

export const MCP_READ_LABEL = "Read conversations, messages, contacts, and received images.";

export const consentScopeLabels: Record<McpScope, string> = {
  "conversations:read": MCP_READ_LABEL,
  "messages:send": "Send real text messages without per-message prompts",
  "media:send": "Send real images without per-image prompts",
};

export const grantScopeLabels: Record<McpScope, string> = {
  "conversations:read": MCP_READ_LABEL,
  "messages:send": "Send messages",
  "media:send": "Send images",
};
