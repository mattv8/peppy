import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { CodingAgentOAuthConnection } from "./CodingAgentOAuthConnection";

describe("CodingAgentOAuthConnection", () => {
  afterEach(cleanup);

  it("uses the Peppy OAuth-native OpenCode setup", () => {
    render(<CodingAgentOAuthConnection clientId="opencode" mcpUrl="https://peppy.example.test/mcp" />);
    expect(screen.getByText(/opencode mcp add peppy --global --url/)).toBeTruthy();
    expect(screen.getAllByText("/mcps")).toHaveLength(2);
  });

  it.each(["claude-desktop", "chatgpt"] as const)("keeps %s setup free of retired connection details", (clientId) => {
    const { container } = render(<CodingAgentOAuthConnection clientId={clientId} mcpUrl="https://peppy.example.test/mcp" />);
    expect(container.textContent!.replaceAll("vault credential", "")).not.toMatch(/Bearer|Ragtime|workspace|credential/i);
  });
});
