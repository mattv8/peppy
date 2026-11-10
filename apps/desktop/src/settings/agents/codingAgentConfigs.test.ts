import { describe, expect, it } from "vitest";
import { buildClientConfig, buildCursorInstallLink, PEPPY_MCP_SERVER_NAME } from "./codingAgentConfigs";

const mcpUrl = "https://peppy.example.test/mcp?vault=one";

describe("coding agent configuration templates", () => {
  it("builds OAuth-native configurations without headers", () => {
    expect(buildClientConfig("claude-code", mcpUrl)).toContain('"type": "http"');
    expect(buildClientConfig("cline", mcpUrl)).toContain('"streamableHttp"');
    expect(buildClientConfig("continue", mcpUrl)).toContain("streamable-http");
    expect(buildClientConfig("opencode", mcpUrl)).toContain('"servers"');
    expect(buildClientConfig("hermes", mcpUrl)).toContain("auth: oauth");
    expect(buildClientConfig("codex", mcpUrl)).toContain("[mcp_servers.peppy]");
    expect(buildClientConfig("chatgpt", mcpUrl)).toBeNull();
    expect(buildClientConfig("cline", mcpUrl)).not.toMatch(/header|bearer|credential/i);
  });

  it("encodes the Cursor URL in a single server entry", () => {
    const link = new URL(buildCursorInstallLink(mcpUrl));
    expect(link.protocol).toBe("cursor:");
    expect(link.searchParams.get("name")).toBe(PEPPY_MCP_SERVER_NAME);
    expect(JSON.parse(atob(link.searchParams.get("config")!))).toEqual({ url: mcpUrl });
  });
});
