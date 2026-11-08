import { describe, expect, it } from "vitest";
import { mcpErrorMessage, mcpServerErrorCode } from "./mcpDisplay";

describe("mcpErrorMessage", () => {
  it.each([
    ["mcp_enable_in_progress", "Peppy MCP setup is already running. Wait a few minutes, then try again."],
    ["mcp_device_conflict", "Peppy MCP is already set up. Close this to see its current status."],
    ["mcp_operation_not_found", "This setup attempt expired or was replaced. Try enabling again."],
    ["mcp_device_not_enrolled", "Peppy MCP setup didn't finish. Try again."],
    ["mcp_unavailable", "Peppy MCP isn't reachable right now. Try again in a moment."],
    ["owner_required", "Only an owner can enable Peppy MCP."],
    ["session-changed", "Your session changed. Close this and try again."],
    ["offline", "Couldn't reach the server. Check the connection and try again."],
  ])("uses actionable copy for %s", (code, expected) => {
    expect(mcpErrorMessage({ code, message: "ignored" }, "fallback")).toBe(expected);
  });

  it("recognizes a server code embedded in a native-shaped message", () => {
    expect(mcpErrorMessage({ code: "server-rejected", message: "The server rejected the request (HTTP 409, mcp_enable_in_progress)." }, "fallback"))
      .toBe("Peppy MCP setup is already running. Wait a few minutes, then try again.");
  });

  it("recognizes direct browser and embedded native grant-not-found codes exactly", () => {
    expect(mcpServerErrorCode({ code: "mcp_grant_not_found", message: "The MCP server could not complete the request." })).toBe("mcp_grant_not_found");
    expect(mcpServerErrorCode({ code: "server-rejected", message: "The server rejected the request (HTTP 404, mcp_grant_not_found)." })).toBe("mcp_grant_not_found");
    expect(mcpServerErrorCode({ code: "server-rejected", message: "The server rejected the request (HTTP 404)." })).not.toBe("mcp_grant_not_found");
  });

  it("uses plain-object and Error messages before the fallback", () => {
    expect(mcpErrorMessage({ message: "Could not reach the configured server." }, "fallback")).toBe("Could not reach the configured server.");
    expect(mcpErrorMessage(new Error("Request failed"), "fallback")).toBe("Request failed");
  });

  it("uses the fallback for missing, empty, or unusual errors without rendering an object", () => {
    const circular: { self?: unknown } = {};
    circular.self = circular;
    for (const value of [undefined, null, {}, { message: "" }, 12, circular]) {
      expect(mcpErrorMessage(value, "fallback")).toBe("fallback");
    }
  });
});
