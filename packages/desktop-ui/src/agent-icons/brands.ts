export const AGENT_BRAND_IDS = [
  "claude-desktop",
  "claude-code",
  "cursor",
  "chatgpt",
  "cline",
  "continue",
  "opencode",
  "hermes",
  "codex",
] as const;

export type AgentBrandId = (typeof AGENT_BRAND_IDS)[number];
