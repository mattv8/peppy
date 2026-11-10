import type { CodingAgentClientId } from "./codingAgentClients";

export const PEPPY_MCP_SERVER_NAME = "peppy";

export function buildMcpServersConfig(server: Record<string, unknown>): string {
  return JSON.stringify({ mcpServers: { [PEPPY_MCP_SERVER_NAME]: server } }, null, 2);
}

export function buildClaudeCodeConfig(mcpUrl: string): string {
  return buildMcpServersConfig({ type: "http", url: mcpUrl });
}

export function buildCursorConfig(mcpUrl: string): string {
  return buildMcpServersConfig({ url: mcpUrl });
}

export function buildClineConfig(mcpUrl: string): string {
  return buildMcpServersConfig({ type: "streamableHttp", url: mcpUrl, disabled: false, autoApprove: [] });
}

export function buildContinueConfig(mcpUrl: string): string {
  return `name: ${PEPPY_MCP_SERVER_NAME}
version: 0.0.1
schema: v1
mcpServers:
  - name: ${PEPPY_MCP_SERVER_NAME}
    type: streamable-http
    url: ${JSON.stringify(mcpUrl)}`;
}

export function buildOpenCodeConfig(mcpUrl: string): string {
  return JSON.stringify({ mcp: { servers: { [PEPPY_MCP_SERVER_NAME]: { type: "remote", url: mcpUrl } } } }, null, 2);
}

export function buildHermesConfig(mcpUrl: string): string {
  return `mcp_servers:
  ${PEPPY_MCP_SERVER_NAME}:
    url: ${JSON.stringify(mcpUrl)}
    auth: oauth`;
}

export function buildCodexConfig(mcpUrl: string): string {
  return `[mcp_servers.${PEPPY_MCP_SERVER_NAME}]
url = ${JSON.stringify(mcpUrl)}`;
}

export function buildCursorInstallLink(mcpUrl: string): string {
  const config = JSON.stringify({ url: mcpUrl });
  const encodedConfig = btoa(String.fromCharCode(...new TextEncoder().encode(config)));
  return `cursor://anysphere.cursor-deeplink/mcp/install?${new URLSearchParams({ name: PEPPY_MCP_SERVER_NAME, config: encodedConfig })}`;
}

export function buildClientConfig(clientId: CodingAgentClientId, mcpUrl: string): string | null {
  switch (clientId) {
    case "claude-code": return buildClaudeCodeConfig(mcpUrl);
    case "cursor": return buildCursorConfig(mcpUrl);
    case "cline": return buildClineConfig(mcpUrl);
    case "continue": return buildContinueConfig(mcpUrl);
    case "opencode": return buildOpenCodeConfig(mcpUrl);
    case "hermes": return buildHermesConfig(mcpUrl);
    case "codex": return buildCodexConfig(mcpUrl);
    case "claude-desktop":
    case "chatgpt": return null;
  }
}
