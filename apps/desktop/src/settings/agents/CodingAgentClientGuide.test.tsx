import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CodingAgentClientGuide } from "./CodingAgentClientGuide";
import type { CodingAgentClientId } from "./codingAgentClients";

const mcpUrl = "https://peppy.example.test/mcp";
const clients: Array<[CodingAgentClientId, RegExp]> = [
  ["claude-desktop", /Claude Cowork/], ["claude-code", /claude mcp add/], ["cursor", /Install in Cursor/],
  ["chatgpt", /ChatGPT Plugins/], ["cline", /streamableHttp/], ["continue", /streamable-http/],
  ["opencode", /opencode mcp list/], ["hermes", /hermes mcp test/], ["codex", /codex mcp login/],
];

describe("CodingAgentClientGuide", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it.each(clients)("renders the distinct %s guide", (clientId, expected) => {
    render(<CodingAgentClientGuide clientId={clientId} mcpUrl={mcpUrl} />);
    expect(screen.getByLabelText(/setup guide/i).dataset.clientGuide).toBe(clientId);
    expect(screen.getAllByText(expected).length).toBeGreaterThan(0);
  });

  it("uses the approved verification prompt and no retired connection details", () => {
    for (const [clientId] of clients) {
      const { unmount, container } = render(<CodingAgentClientGuide clientId={clientId} mcpUrl={mcpUrl} />);
      const text = container.textContent!.replaceAll("vault credential", "");
      expect(text).toContain("Call list_conversations with limit 5");
      expect(text).not.toMatch(/Bearer|Ragtime|workspace|credential/i);
      unmount();
    }
  });

  it("routes documentation links through the native opener when provided", () => {
    const onOpenExternal = vi.fn();
    render(<CodingAgentClientGuide clientId="cursor" mcpUrl={mcpUrl} onOpenExternal={onOpenExternal} />);
    fireEvent.click(screen.getByRole("link", { name: /current client documentation/i }));
    expect(onOpenExternal).toHaveBeenCalledWith("https://cursor.com/docs/context/mcp");
  });

  it("uses safe browser anchors when no native opener is provided", () => {
    render(<CodingAgentClientGuide clientId="cursor" mcpUrl={mcpUrl} />);
    expect(screen.getByRole("link", { name: /current client documentation/i }).getAttribute("target")).toBe("_blank");
    expect(screen.getByRole("link", { name: /install in cursor/i }).getAttribute("target")).toBe("_blank");
  });

  it("includes the Claude Code OAuth sign-in step", () => {
    render(<CodingAgentClientGuide clientId="claude-code" mcpUrl={mcpUrl} />);
    const signInStep = screen.getByText(/Open Claude Code, run/);
    expect(signInStep.textContent).toContain("select peppy, and sign in.");
    expect(screen.getByText("/mcp").tagName).toBe("CODE");
    expect(signInStep.closest("ol")?.getAttribute("start")).toBe("2");
  });

  it("makes snippets keyboard reachable and resets copied feedback", async () => {
    vi.useFakeTimers();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(<CodingAgentClientGuide clientId="cursor" mcpUrl={mcpUrl} />);
    const snippet = screen.getByLabelText("Cursor configuration");
    expect(snippet.tagName).toBe("PRE");
    expect(snippet.getAttribute("tabindex")).toBe("0");
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Copy Cursor configuration" })); });
    expect(screen.getAllByRole("status").some((status) => status.textContent === "Cursor configuration copied.")).toBe(true);
    act(() => { vi.advanceTimersByTime(1_500); });
    expect(screen.getAllByRole("status").some((status) => status.textContent === "Cursor configuration copied.")).toBe(false);
    vi.useRealTimers();
  });

  it("announces snippet copy failures", async () => {
    Object.assign(navigator, { clipboard: { writeText: vi.fn().mockRejectedValue(new Error("denied")) } });
    render(<CodingAgentClientGuide clientId="cursor" mcpUrl={mcpUrl} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Copy Cursor configuration" })); });
    await waitFor(() => expect(screen.getAllByRole("status").some((status) => status.textContent === "Couldn't copy. Copy it manually.")).toBe(true));
  });
});
