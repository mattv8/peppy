import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { McpDeviceState, McpDeviceStatus } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "../bridge/bridge";
import { McpConnectHelp } from "./McpConnectHelp";
import { publishMcpStatus } from "./mcpStatusStore";

vi.mock("../bridge/bridge", { spy: true });
vi.mock("./agents/CodingAgentClientGuide", () => ({
  CodingAgentClientGuide: ({ clientId, mcpUrl, onOpenExternal }: { clientId: string; mcpUrl: string; onOpenExternal?: (url: string) => void }) => <div data-testid="agent-guide">{clientId} {mcpUrl}{onOpenExternal && <button type="button" onClick={() => onOpenExternal("https://agent.example/setup")}>Open setup</button>}</div>,
}));

const READY = { vaultId: "11111111-1111-4111-8111-111111111111", available: true, serverOrigin: "https://vault.example", hosted: true } as const;
const status = (state: McpDeviceState, cleanupPending = false): McpDeviceStatus => ({
  ...READY,
  device: { deviceId: "mcp-device", name: "Peppy MCP", state, lastSyncedAt: null, cleanupPending },
});

async function settle() {
  await act(async () => { await Promise.resolve(); });
}

function renderHelp(sessionKey: string, browserHost = false) {
  return render(<McpConnectHelp sessionKey={sessionKey} browserHost={browserHost} />);
}

describe("McpConnectHelp", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(bridge.mcp_status).mockResolvedValue(status("online"));
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    Reflect.deleteProperty(navigator, "clipboard");
  });

  it.each([
    ["no device", { ...READY, device: null }],
    ["removed device", status("revoked")],
    ["cleanup pending", status("revoked", true)],
  ])("hides when there is %s", async (_name, response) => {
    const key = `hidden-${_name}`;
    renderHelp(key);
    expect(await screen.findByText("https://vault.example/mcp")).toBeInTheDocument();
    await act(async () => publishMcpStatus(key, response));
    expect(document.getElementById("mcp-connect")).not.toBeInTheDocument();
  });

  it.each(["online", "offline"] as const)("shows the endpoint while %s", async state => {
    vi.mocked(bridge.mcp_status).mockResolvedValue(status(state));
    renderHelp(`shown-${state}`);
    expect(await screen.findByText("https://vault.example/mcp")).toBeInTheDocument();
  });

  it("opens the Claude Cowork / Desktop setup modal from help", async () => {
    renderHelp("help-modal");
    fireEvent.click(await screen.findByRole("button", { name: "How to connect a coding agent" }));
    const dialog = screen.getByRole("dialog", { name: "Choose your coding agent" });
    expect(within(dialog).getByText(/Coding agents can connect to Peppy MCP/)).toBeInTheDocument();
    expect(within(dialog).getByTestId("agent-guide")).toHaveTextContent("claude-desktop");
  });

  it("opens Claude Cowork / Desktop from the text trigger and returns focus on close", async () => {
    renderHelp("text-trigger");
    const trigger = await screen.findByRole("button", { name: "Connect your agent" });
    fireEvent.click(trigger);
    const dialog = screen.getByRole("dialog", { name: "Choose your coding agent" });
    expect(within(dialog).getByTestId("agent-guide")).toHaveTextContent("claude-desktop");
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    expect(trigger).toHaveFocus();
  });

  it("opens an icon's setup tab and switches tabs", async () => {
    renderHelp("agent-tabs");
    fireEvent.click(await screen.findByRole("button", { name: "Set up Cursor" }));
    const dialog = screen.getByRole("dialog", { name: "Choose your coding agent" });
    expect(within(dialog).getByTestId("agent-guide")).toHaveTextContent("cursor");
    fireEvent.click(within(dialog).getByRole("button", { name: "Codex" }));
    expect(within(dialog).getByTestId("agent-guide")).toHaveTextContent("codex");
    expect(within(dialog).getByRole("button", { name: "Codex" })).toHaveAttribute("aria-pressed", "true");
  });

  it("copies the endpoint and announces success briefly", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    renderHelp("copy-success");
    const copy = await screen.findByRole("button", { name: "Copy" });
    vi.useFakeTimers();
    fireEvent.click(copy);
    await settle();
    expect(writeText).toHaveBeenCalledWith("https://vault.example/mcp");
    expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Endpoint copied.");
    await act(async () => { vi.advanceTimersByTime(2_000); });
    expect(screen.getByRole("button", { name: "Copy" })).toBeInTheDocument();
  });

  it("shows selection guidance when copying fails", async () => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: vi.fn().mockRejectedValue(new Error("denied")) } });
    renderHelp("copy-failure");
    fireEvent.click(await screen.findByRole("button", { name: "Copy" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Couldn't copy. Select the endpoint above.");
  });

  it("updates when device settings publish a new status", async () => {
    renderHelp("published-status");
    expect(await screen.findByText("https://vault.example/mcp")).toBeInTheDocument();
    await act(async () => publishMcpStatus("published-status", { ...status("online"), serverOrigin: "https://updated.example" }));
    expect(screen.getByText("https://updated.example/mcp")).toBeInTheDocument();
  });

  it("drops stale fetches, changes sessions without showing the old endpoint, and unsubscribes on unmount", async () => {
    let resolve!: (next: McpDeviceStatus) => void;
    vi.mocked(bridge.mcp_status).mockReturnValue(new Promise<McpDeviceStatus>(complete => { resolve = complete; }));
    const { rerender, unmount } = renderHelp("race-a");
    await act(async () => publishMcpStatus("race-a", { ...status("online"), serverOrigin: "https://new.example" }));
    expect(screen.getByText("https://new.example/mcp")).toBeInTheDocument();
    await act(async () => resolve(status("online")));
    expect(screen.getByText("https://new.example/mcp")).toBeInTheDocument();
    rerender(<McpConnectHelp sessionKey="race-b" browserHost={false} />);
    expect(screen.queryByText("https://new.example/mcp")).not.toBeInTheDocument();
    unmount();
    expect(() => publishMcpStatus("race-a", status("online"))).not.toThrow();
  });

  it("wires native external opens and reports a failure, but omits the callback in a browser host", async () => {
    const openExternal = bridge.open_external_url!;
    vi.mocked(openExternal).mockRejectedValue(new Error("denied"));
    renderHelp("native-link");
    fireEvent.click(await screen.findByRole("button", { name: "How to connect a coding agent" }));
    fireEvent.click(screen.getByRole("button", { name: "Open setup" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't open the link.");
    expect(openExternal).toHaveBeenCalledWith("https://agent.example/setup");
    cleanup();
    renderHelp("browser-link", true);
    fireEvent.click(await screen.findByRole("button", { name: "How to connect a coding agent" }));
    expect(screen.queryByRole("button", { name: "Open setup" })).not.toBeInTheDocument();
  });
});
