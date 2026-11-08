import "@testing-library/jest-dom/vitest";
import type { ComponentProps } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { McpDeviceSettings } from "./McpDeviceSettings";
import { MCP_READ_LABEL } from "./mcpScopes";
import { bridge, type DeviceRosterView } from "../bridge/bridge";
import type { McpDeviceState, McpDeviceStatus, McpGrantView } from "@peppy/desktop-ui/bridge-contracts";

vi.mock("../bridge/bridge", { spy: true });

const owner: DeviceRosterView = { currentDeviceId: "owner", devices: [{ id: "owner", name: "This Mac", kind: "desktop", role: "owner", revoked: false }] };
const viewer: DeviceRosterView = { currentDeviceId: "viewer", devices: [{ id: "viewer", name: "Shared browser", kind: "browser", role: "device", revoked: false }] };
const VAULT_ID = "11111111-1111-4111-8111-111111111111";
const unavailable = { vaultId: VAULT_ID, available: false, serverOrigin: "https://vault.example", hosted: false, device: null } as const;
const ready = { vaultId: VAULT_ID, available: true, serverOrigin: "https://vault.example", hosted: true, device: null } as const;
const device = (state: McpDeviceState, cleanupPending = false): McpDeviceStatus => ({
  ...ready,
  device: { deviceId: "mcp-device", name: "Peppy MCP", state, lastSyncedAt: "now", cleanupPending },
});
const cleanupStatus = device("revoked", true);
const onlineStatus = device("online");
const GRANT: McpGrantView = { id: "0f1e2d3c-4b5a-4968-8777-665544332211", clientName: "Claude", scopes: ["conversations:read", "messages:send"], createdAt: "2026-01-02", lastUsedAt: null };
const OFFLINE_HINT = "Peppy MCP is offline. Checking for it to connect.";
const MAX_POLLS = 24;
const ROW_STILL_WAITING = "Still offline. Open Details to check again.";
const CLEANUP_HINT = "Cleanup is running. Status updates automatically.";
const STILL_WAITING = "Still offline. Select Refresh status to check again.";
const STALE_STATUS = "Couldn't refresh status. Showing the last known status.";
const COPY_FAILED = "Couldn't copy. Select the endpoint below.";
const fresh = (template: McpDeviceStatus) => () => Promise.resolve({ ...template, device: template.device ? { ...template.device } : null });
const CONFIRM_CONSEQUENCE = "Stops new access for this authorization. Existing external copies remain.";

function deferred<T>() { let resolve!: (value: T) => void; let reject!: (reason: unknown) => void; return { promise: new Promise<T>((complete, fail) => { resolve = complete; reject = fail; }), resolve, reject }; }

/** Drains pending promise continuations inside act so mocked bridge results reach React. */
async function settle() {
  await act(async () => { for (let index = 0; index < 10; index += 1) await Promise.resolve(); });
}

function renderSettings(props: Partial<ComponentProps<typeof McpDeviceSettings>> = {}) {
  return render(<McpDeviceSettings roster={owner} serverOffline={false} accountReadOnly={false} sessionKey="vault-a" unlocked {...props} />);
}

async function openDetailsDialog() {
  fireEvent.click(await screen.findByRole("button", { name: /Peppy MCP details/i }));
  return screen.findByRole("dialog", { name: "Peppy MCP" });
}

describe("McpDeviceSettings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(bridge.mcp_status).mockResolvedValue(ready);
    vi.mocked(bridge.mcp_grants).mockResolvedValue([]);
    vi.mocked(bridge.mcp_enable).mockResolvedValue({ ...ready, device: { deviceId: "mcp-device", name: "Peppy MCP", state: "online", lastSyncedAt: "now", cleanupPending: false } });
    vi.mocked(bridge.mcp_revoke_grant).mockResolvedValue(undefined);
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    Reflect.deleteProperty(navigator, "clipboard");
  });

  describe("enablement and custody consent", () => {
    it("does not offer enablement to a non-owner", async () => {
      renderSettings({ roster: viewer });
      expect(await screen.findByText(/only an owner/i)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /enable Peppy MCP/i })).not.toBeInTheDocument();
    });

    it("does not offer custody consent while a session remains locked", async () => {
      renderSettings({ sessionKey: "still-present-session", unlocked: false });
      expect(await screen.findByText(/unlock this vault/i)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /review custody consent/i })).not.toBeInTheDocument();
      expect(bridge.mcp_enable).not.toHaveBeenCalled();
    });

    it("keeps enable disabled until custody consent is selected", async () => {
      renderSettings();
      fireEvent.click(await screen.findByRole("button", { name: /review custody consent/i }));
      const enable = screen.getByRole("button", { name: /enable Peppy MCP/i });
      expect(enable).toBeDisabled();
      fireEvent.click(screen.getByRole("checkbox", { name: /Peppy can decrypt this vault/i }));
      expect(enable).toBeEnabled();
    });

    it("identifies the authenticated vault in custody consent", async () => {
      renderSettings({ sessionKey: "malformed-session" });
      fireEvent.click(await screen.findByRole("button", { name: /review custody consent/i }));
      expect(screen.getByRole("dialog")).toHaveTextContent(VAULT_ID);
      expect(screen.getByRole("dialog")).not.toHaveTextContent("malformed-session");
    });

    it("refuses enablement when the status omits an authenticated vault ID", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue({ available: true, serverOrigin: "https://vault.example", hosted: true, device: null } as unknown as McpDeviceStatus);
      renderSettings();
      expect(await screen.findByText(/authenticated vault context is unavailable/i)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /review custody consent/i })).not.toBeInTheDocument();
    });

    it("does not hand off keys when consent is cancelled", async () => {
      renderSettings();
      fireEvent.click(await screen.findByRole("button", { name: /review custody consent/i }));
      fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
      expect(bridge.mcp_enable).not.toHaveBeenCalled();
    });

    it("keeps enable failures actionable", async () => {
      vi.mocked(bridge.mcp_enable).mockRejectedValue(new Error("Unlock this vault to approve MCP."));
      renderSettings();
      fireEvent.click(await screen.findByRole("button", { name: /review custody consent/i }));
      fireEvent.click(screen.getByRole("checkbox", { name: /Peppy can decrypt this vault/i }));
      fireEvent.click(screen.getByRole("button", { name: /enable Peppy MCP/i }));
      await waitFor(() => expect(screen.getByRole("dialog")).toHaveTextContent("Unlock this vault to approve MCP."));
    });

    it("clears an enable error after cancel and does not show it on reopen", async () => {
      vi.mocked(bridge.mcp_enable).mockRejectedValueOnce({ code: "unknown", message: "Enable failed" });
      renderSettings();
      fireEvent.click(await screen.findByRole("button", { name: /review custody consent/i }));
      fireEvent.click(screen.getByRole("checkbox", { name: /Peppy can decrypt this vault/i }));
      fireEvent.click(screen.getByRole("button", { name: /enable Peppy MCP/i }));
      expect(await within(screen.getByRole("dialog")).findByRole("alert")).toHaveTextContent("Enable failed");
      fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
      expect(screen.queryByText("Enable failed")).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: /review custody consent/i }));
      expect(screen.queryByText("Enable failed")).not.toBeInTheDocument();
    });

    it("disables Enable after a conflict refresh shows the existing device", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(ready).mockResolvedValueOnce(onlineStatus);
      vi.mocked(bridge.mcp_enable).mockRejectedValueOnce({ code: "mcp_device_conflict", message: "already configured" });
      renderSettings();
      fireEvent.click(await screen.findByRole("button", { name: /review custody consent/i }));
      fireEvent.click(screen.getByRole("checkbox", { name: /Peppy can decrypt this vault/i }));
      fireEvent.click(screen.getByRole("button", { name: /enable Peppy MCP/i }));
      const dialog = screen.getByRole("dialog");
      expect(await within(dialog).findByRole("alert")).toHaveTextContent("Peppy MCP is already set up. Close this to see its current status.");
      expect(within(dialog).getByRole("button", { name: /enable Peppy MCP/i })).toBeDisabled();
    });

    it("keeps custody consent open for a native setup-in-progress error and silently refreshes status", async () => {
      vi.mocked(bridge.mcp_enable).mockRejectedValue({ code: "server-rejected", message: "The server rejected the request (HTTP 409, mcp_enable_in_progress)." });
      renderSettings();
      fireEvent.click(await screen.findByRole("button", { name: /review custody consent/i }));
      fireEvent.click(screen.getByRole("checkbox", { name: /Peppy can decrypt this vault/i }));
      fireEvent.click(screen.getByRole("button", { name: /enable Peppy MCP/i }));
      expect(await within(screen.getByRole("dialog")).findByRole("alert")).toHaveTextContent("Peppy MCP setup is already running. Wait a few minutes, then try again.");
      expect(screen.getByRole("button", { name: /enable Peppy MCP/i })).toBeEnabled();
      expect(bridge.mcp_status).toHaveBeenCalledTimes(2);
    });

    it("ignores an enable completion after the vault context changes", async () => {
      const enable = deferred<McpDeviceStatus>();
      vi.mocked(bridge.mcp_enable).mockReturnValue(enable.promise);
      const { rerender } = renderSettings();
      fireEvent.click(await screen.findByRole("button", { name: /review custody consent/i }));
      fireEvent.click(screen.getByRole("checkbox", { name: /Peppy can decrypt/i }));
      fireEvent.click(screen.getByRole("button", { name: /enable Peppy MCP/i }));
      rerender(<McpDeviceSettings roster={owner} serverOffline={false} accountReadOnly={false} sessionKey="vault-b" unlocked />);
      await act(async () => enable.resolve({ ...ready, device: { deviceId: "stale", name: "Peppy MCP", state: "online", lastSyncedAt: null, cleanupPending: false } }));
      expect(screen.queryByText("Online")).not.toBeInTheDocument();
    });
  });

  describe("status row", () => {
    it("shows the returned runtime state rather than assuming online", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue({ ...ready, device: { deviceId: "mcp-device", name: "Not the lookup key", state: "offline", lastSyncedAt: null, cleanupPending: false } });
      const { container } = renderSettings();
      expect(await screen.findByText("Offline")).toBeInTheDocument();
      expect(container.querySelector('[data-device-id="mcp-device"]')).toBeInTheDocument();
    });

    it("renders stable MCP settings hooks from durable device and grant identifiers", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(onlineStatus);
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      renderSettings();
      expect(await screen.findByRole("button", { name: /Peppy MCP details/i })).toHaveAttribute("id", "mcp-details-open");
      expect(document.getElementById("mcp-device-mcp-device")).toBeInTheDocument();
      const dialog = await openDetailsDialog();
      expect(dialog.querySelector("#mcp-details-summary")).toBeInTheDocument();
      expect(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" })).toHaveAttribute("id", `mcp-grant-revoke-${GRANT.id}`);
      expect(document.getElementById(`mcp-grant-${GRANT.id}`)).toBeInTheDocument();
    });

    it("shows an initial status failure in the row and retries it", async () => {
      vi.mocked(bridge.mcp_status).mockRejectedValueOnce(new Error("Server unavailable")).mockResolvedValue(ready);
      renderSettings();
      expect(await screen.findByRole("alert")).toHaveTextContent("Server unavailable");
      fireEvent.click(screen.getByRole("button", { name: "Retry MCP status" }));
      await waitFor(() => expect(screen.getByRole("button", { name: /review custody consent/i })).toBeInTheDocument());
    });

    it("does not fetch or revoke grants for a nonowner or while offline", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue({ ...ready, device: { deviceId: "mcp", name: "Peppy MCP", state: "online", lastSyncedAt: null, cleanupPending: false } });
      const { rerender } = renderSettings({ roster: viewer });
      await openDetailsDialog();
      expect(bridge.mcp_grants).not.toHaveBeenCalled();
      rerender(<McpDeviceSettings roster={owner} serverOffline accountReadOnly={false} sessionKey="vault-a" unlocked />);
      expect(bridge.mcp_revoke_grant).not.toHaveBeenCalled();
    });
  });

  describe("details modal status", () => {
    it("renders the endpoint, connection steps, and safe verify prompt", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(onlineStatus);
      renderSettings();
      const dialog = await openDetailsDialog();
      expect(within(dialog).getByText("https://vault.example/mcp")).toBeInTheDocument();
      expect(within(dialog).getByRole("heading", { name: "Connect an AI client" })).toBeInTheDocument();
      expect(within(dialog).getByText("List the available Peppy tools and their permissions. Do not read messages or send anything.")).toBeInTheDocument();
    });

    it("refreshes status on open and from the Refresh status button", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(onlineStatus);
      renderSettings();
      const dialog = await openDetailsDialog();
      await waitFor(() => expect(bridge.mcp_status).toHaveBeenCalledTimes(2));
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      await waitFor(() => expect(bridge.mcp_status).toHaveBeenCalledTimes(3));
    });

    it("keeps the last known status and shows an alert when a refresh fails", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(onlineStatus).mockResolvedValueOnce(onlineStatus).mockRejectedValueOnce(new Error("offline"));
      renderSettings();
      const dialog = await openDetailsDialog();
      await settle();
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      expect(await within(dialog).findByRole("alert")).toHaveTextContent(STALE_STATUS);
      expect(within(dialog).getByText("https://vault.example/mcp")).toBeInTheDocument();
    });

    it.each([
      ["timeout", "MCP status check timed out. Try again."],
      ["network", "Peppy could not connect to the MCP server. Try again."],
      ["server", "The MCP server could not complete the request. Try again."],
    ])("appends the sanitized %s message to a failed refresh", async (_kind, message) => {
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(onlineStatus).mockResolvedValueOnce(onlineStatus).mockRejectedValueOnce(new Error(message));
      renderSettings();
      const dialog = await openDetailsDialog();
      await settle();
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      expect(await within(dialog).findByRole("alert")).toHaveTextContent(`${STALE_STATUS} ${message}`);
      expect(within(dialog).getByText("https://vault.example/mcp")).toBeInTheDocument();
    });

    it("clears the stale notice after a later successful refresh", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(onlineStatus).mockResolvedValueOnce(onlineStatus).mockRejectedValueOnce(new Error("MCP status check timed out. Try again.")).mockResolvedValue(onlineStatus);
      renderSettings();
      const dialog = await openDetailsDialog();
      await settle();
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      expect(await within(dialog).findByRole("alert")).toHaveTextContent("MCP status check timed out.");
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      await waitFor(() => expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument());
    });

    it("restores Refresh status focus after the request settles", async () => {
      const refresh = deferred<McpDeviceStatus>();
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(onlineStatus).mockResolvedValueOnce(onlineStatus).mockReturnValueOnce(refresh.promise);
      renderSettings();
      const dialog = await openDetailsDialog();
      await settle();
      const button = within(dialog).getByRole("button", { name: "Refresh status" });
      button.focus();
      fireEvent.click(button);
      await act(async () => {});
      button.blur();
      await act(async () => refresh.resolve(onlineStatus));
      expect(button).toHaveFocus();
    });

    it("shows the cleanup hint while cleanup is pending", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(device("revoked", true));
      renderSettings();
      const dialog = await openDetailsDialog();
      expect(within(dialog).getByText(CLEANUP_HINT)).toBeInTheDocument();
    });
  });

  describe("status polling", () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });

    async function advance(milliseconds: number) {
      await act(async () => { vi.advanceTimersByTime(milliseconds); });
    }

    it("polls cleanup status every 5 seconds, exactly 24 times, then reports that it is still waiting", async () => {
      vi.mocked(bridge.mcp_status).mockImplementation(fresh(cleanupStatus));
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      const opened = vi.mocked(bridge.mcp_status).mock.calls.length;
      expect(screen.getByText(CLEANUP_HINT)).toBeInTheDocument();
      for (let tick = 1; tick <= 24; tick += 1) {
        await advance(5_000);
        expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(opened + tick);
      }
      await advance(120_000);
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(opened + 24);
      await settle();
      expect(screen.getByText("Cleanup is still running. Select Refresh status to check again.")).toBeInTheDocument();
      expect(screen.queryByText(CLEANUP_HINT)).not.toBeInTheDocument();
    });

    it("keeps a 5 second cadence when status responses land between ticks", async () => {
      vi.mocked(bridge.mcp_status).mockImplementation(fresh(cleanupStatus));
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      const opened = vi.mocked(bridge.mcp_status).mock.calls.length;
      // Flushing between uneven steps lets each response re-render between ticks; a restart on that re-render would shift the next tick.
      for (let elapsed = 0; elapsed < 10_500; elapsed += 1_500) {
        await advance(1_500);
        await settle();
      }
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(opened + 2);
    });

    it("stops polling once cleanup finishes and announces the change", async () => {
      vi.mocked(bridge.mcp_status).mockImplementationOnce(fresh(cleanupStatus)).mockImplementationOnce(fresh(cleanupStatus)).mockImplementation(fresh(onlineStatus));
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      await advance(5_000);
      await settle();
      expect(screen.getByText("Status is now Online.")).toBeInTheDocument();
      const calls = vi.mocked(bridge.mcp_status).mock.calls.length;
      await advance(60_000);
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(calls);
    });

    it("stops polling when the details close", async () => {
      vi.mocked(bridge.mcp_status).mockImplementation(fresh(cleanupStatus));
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      fireEvent.click(screen.getByRole("button", { name: "Close" }));
      const calls = vi.mocked(bridge.mcp_status).mock.calls.length;
      await advance(60_000);
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(calls);
    });

    it("restarts a fresh 24-poll budget when details are reopened", async () => {
      vi.mocked(bridge.mcp_status).mockImplementation(fresh(cleanupStatus));
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      await advance(120_000);
      await settle();
      expect(screen.getByText("Cleanup is still running. Select Refresh status to check again.")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Close" }));
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      expect(screen.queryByText(STILL_WAITING)).not.toBeInTheDocument();
      const calls = vi.mocked(bridge.mcp_status).mock.calls.length;
      await advance(5_000);
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(calls + 1);
    });

    it("reactivates cleanup polling with a fresh budget after a spent run settles", async () => {
      let next = cleanupStatus;
      vi.mocked(bridge.mcp_status).mockImplementation(() => Promise.resolve(next));
      const { rerender } = renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      for (let tick = 0; tick < MAX_POLLS; tick += 1) {
        await advance(5_000);
        await settle();
      }
      next = onlineStatus;
      fireEvent.click(screen.getByRole("button", { name: "Refresh status" }));
      await settle();
      next = cleanupStatus;
      rerender(<McpDeviceSettings roster={owner} serverOffline={false} accountReadOnly={false} sessionKey="vault-a" unlocked refreshToken={1} />);
      await settle();
      expect(screen.queryByText("Cleanup is still running. Select Refresh status to check again.")).not.toBeInTheDocument();
      const calls = vi.mocked(bridge.mcp_status).mock.calls.length;
      await advance(5_000);
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(calls + 1);
    });

    it("drops a late poll result that finishes after a newer manual refresh", async () => {
      const late = deferred<McpDeviceStatus>();
      vi.mocked(bridge.mcp_status)
         .mockImplementationOnce(fresh(cleanupStatus))
         .mockImplementationOnce(fresh(cleanupStatus))
        .mockReturnValueOnce(late.promise)
        .mockImplementation(fresh(onlineStatus));
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      await advance(5_000);
      fireEvent.click(screen.getByRole("button", { name: "Refresh status" }));
      await settle();
      expect(screen.getByText("Status is now Online.")).toBeInTheDocument();
      await act(async () => late.resolve(cleanupStatus));
      await settle();
      expect(screen.queryByText(CLEANUP_HINT)).not.toBeInTheDocument();
      expect(screen.getByText("Status is now Online.")).toBeInTheDocument();
    });

    it("does not invalidate an active poll when later ticks are skipped", async () => {
      const poll = deferred<McpDeviceStatus>();
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(cleanupStatus).mockResolvedValueOnce(cleanupStatus).mockReturnValueOnce(poll.promise);
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      await advance(5_000);
      await advance(10_000);
      await act(async () => poll.resolve(onlineStatus));
      expect(screen.getByText("Status is now Online.")).toBeInTheDocument();
    });

    it("does not invalidate an active manual refresh when polling ticks are skipped", async () => {
      const manual = deferred<McpDeviceStatus>();
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(cleanupStatus).mockResolvedValueOnce(cleanupStatus).mockReturnValueOnce(manual.promise);
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      const dialog = screen.getByRole("dialog", { name: "Peppy MCP" });
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      await advance(10_000);
      await act(async () => manual.resolve(onlineStatus));
      expect(screen.getByText("Status is now Online.")).toBeInTheDocument();
    });

    it("declares final poll exhaustion after a superseding manual refresh settles", async () => {
      const finalPoll = deferred<McpDeviceStatus>();
      let statusCalls = 0;
      vi.mocked(bridge.mcp_status).mockImplementation(() => {
        statusCalls += 1;
        return statusCalls === MAX_POLLS + 2 ? finalPoll.promise : Promise.resolve(cleanupStatus);
      });
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      const dialog = screen.getByRole("dialog", { name: "Peppy MCP" });
      for (let tick = 0; tick < MAX_POLLS - 1; tick += 1) {
        await advance(5_000);
        await settle();
      }
      await advance(5_000);
      expect(screen.queryByText("Cleanup is still running. Select Refresh status to check again.")).not.toBeInTheDocument();
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      await settle();
      expect(screen.getByText("Cleanup is still running. Select Refresh status to check again.")).toBeInTheDocument();
      await act(async () => finalPoll.resolve(onlineStatus));
    });

    it("announces a finished cleanup of a removed device through the removed notice only", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(device("revoked", true)).mockResolvedValueOnce(device("revoked", true)).mockResolvedValue(device("revoked", false));
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      await advance(5_000);
      await settle();
      const dialog = screen.getByRole("dialog", { name: "Peppy MCP" });
      expect(dialog.querySelector("#mcp-removed-notice")).toHaveTextContent("Peppy MCP was removed.");
      expect(dialog.querySelector("#mcp-status-notice")).not.toHaveTextContent("Cleanup finished.");
    });
  });

  describe("copy endpoint", () => {
    it("announces a successful copy of the endpoint", async () => {
      const writeText = vi.fn().mockResolvedValue(undefined);
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
      vi.mocked(bridge.mcp_status).mockResolvedValue(onlineStatus);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(within(dialog).getByRole("button", { name: "Copy endpoint" }));
      expect(await within(dialog).findByText("Endpoint copied.")).toBeInTheDocument();
      expect(writeText).toHaveBeenCalledWith("https://vault.example/mcp");
    });

    it("tells the owner to select the endpoint when the clipboard rejects", async () => {
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: vi.fn().mockRejectedValue(new Error("denied")) } });
      vi.mocked(bridge.mcp_status).mockResolvedValue(onlineStatus);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(within(dialog).getByRole("button", { name: "Copy endpoint" }));
      expect(await within(dialog).findByText(COPY_FAILED)).toBeInTheDocument();
    });

    it("tells the owner to select the endpoint when the clipboard is unavailable", async () => {
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
      vi.mocked(bridge.mcp_status).mockResolvedValue(onlineStatus);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(within(dialog).getByRole("button", { name: "Copy endpoint" }));
      expect(await within(dialog).findByText(COPY_FAILED)).toBeInTheDocument();
    });
  });

  describe("authorized clients", () => {
    beforeEach(() => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(onlineStatus);
    });

    it("shows a loading state while grants load", async () => {
      vi.mocked(bridge.mcp_grants).mockReturnValue(deferred<McpGrantView[]>().promise);
      renderSettings();
      const dialog = await openDetailsDialog();
      expect(within(dialog).getByText("Loading authorized clients…")).toHaveAttribute("role", "status");
    });

    it("shows the empty state when no clients are authorized", async () => {
      renderSettings();
      const dialog = await openDetailsDialog();
      expect(await within(dialog).findByText(/No authorized clients yet/)).toBeInTheDocument();
    });

    it("shows a load error with a retry that loads the list", async () => {
      vi.mocked(bridge.mcp_grants).mockRejectedValueOnce(new Error("grants unavailable")).mockResolvedValueOnce([GRANT]);
      renderSettings();
      const dialog = await openDetailsDialog();
      expect(await within(dialog).findByRole("alert")).toHaveTextContent("grants unavailable");
      fireEvent.click(within(dialog).getByRole("button", { name: "Retry loading clients" }));
      expect(await within(dialog).findByRole("list", { name: "Authorized MCP clients" })).toBeInTheDocument();
    });

    it("shows permissions, creation, Never used, and a short ID for each grant", async () => {
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      renderSettings();
      const dialog = await openDetailsDialog();
      const row = (await within(dialog).findByText("Claude")).closest("li") as HTMLElement;
      expect(within(row).getByRole("list")).toHaveTextContent(MCP_READ_LABEL);
      expect(within(row).getByText(MCP_READ_LABEL)).toBeInTheDocument();
      expect(within(row).getByText("Send messages")).toBeInTheDocument();
      expect(within(row).getByText("2026-01-02")).toBeInTheDocument();
      expect(within(row).getByText("Never used")).toBeInTheDocument();
      expect(within(row).getByText("0f1e2d3c")).toBeInTheDocument();
    });

    it("gives two same-name grants distinct accessible revoke names", async () => {
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT, { ...GRANT, id: "aaaaaaaa-1111-4111-8111-111111111111" }]);
      renderSettings();
      const dialog = await openDetailsDialog();
      expect(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" })).toBeInTheDocument();
      expect(within(dialog).getByRole("button", { name: "Revoke Claude (aaaaaaaa)" })).toBeInTheDocument();
    });

    it("confirms a revoke with its consequence, focusing Cancel first", async () => {
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      expect(within(dialog).getByText(CONFIRM_CONSEQUENCE)).toBeInTheDocument();
      expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
      expect(bridge.mcp_revoke_grant).not.toHaveBeenCalled();
    });

    it("keeps an open revoke confirmation while a background grant refresh runs", async () => {
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      expect(within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" })).toBeInTheDocument();
      expect(document.getElementById("mcp-grants")).toHaveAttribute("aria-busy", "true");
    });

    it("returns focus to that row's Revoke button when Cancel is chosen", async () => {
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
      expect(within(dialog).getByRole("button", { name: "Revoke Claude (0f1e2d3c)" })).toHaveFocus();
      expect(bridge.mcp_revoke_grant).not.toHaveBeenCalled();
    });

    it("moves focus to the section heading and announces a completed revoke once", async () => {
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" }));
      await waitFor(() => expect(bridge.mcp_revoke_grant).toHaveBeenCalledWith(GRANT.id));
      expect(within(dialog).getByRole("heading", { name: "Authorized clients" })).toHaveFocus();
      expect(document.getElementById("mcp-grant-status")).toHaveTextContent("Revoked Claude (0f1e2d3c).");
      expect(await within(dialog).findByText(/No authorized clients yet/)).toBeInTheDocument();
    });

    it("keeps the row and confirmation when revoke fails", async () => {
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      vi.mocked(bridge.mcp_revoke_grant).mockRejectedValue(new Error("network down"));
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" }));
      expect(await within(dialog).findByRole("alert")).toHaveTextContent("network down");
      expect(within(dialog).getByText("Claude")).toBeInTheDocument();
      expect(within(dialog).getByText(CONFIRM_CONSEQUENCE)).toBeInTheDocument();
    });

    it("sends one revoke for a double confirm", async () => {
      const revoke = deferred<void>();
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      vi.mocked(bridge.mcp_revoke_grant).mockReturnValue(revoke.promise);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      const confirm = within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" });
      fireEvent.click(confirm);
      fireEvent.click(confirm);
      expect(bridge.mcp_revoke_grant).toHaveBeenCalledTimes(1);
      await act(async () => revoke.resolve());
    });

    it("drops a delayed grant load that finishes after the session changes", async () => {
      const stale = deferred<McpGrantView[]>();
      vi.mocked(bridge.mcp_grants).mockReturnValueOnce(stale.promise).mockResolvedValueOnce([{ ...GRANT, clientName: "Current client" }]);
      const { rerender } = renderSettings();
      await openDetailsDialog();
      rerender(<McpDeviceSettings roster={owner} serverOffline={false} accountReadOnly={false} sessionKey="vault-b" unlocked />);
      fireEvent.click(await screen.findByRole("button", { name: /Peppy MCP details/i }));
      expect(await screen.findByText("Current client")).toBeInTheDocument();
      await act(async () => stale.resolve([{ ...GRANT, clientName: "Stale client" }]));
      expect(screen.queryByText("Stale client")).not.toBeInTheDocument();
    });

    it("drops a delayed revoke that finishes after the session changes", async () => {
      const revoke = deferred<void>();
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      vi.mocked(bridge.mcp_revoke_grant).mockReturnValue(revoke.promise);
      const { rerender } = renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" }));
      rerender(<McpDeviceSettings roster={owner} serverOffline={false} accountReadOnly={false} sessionKey="vault-b" unlocked />);
      await act(async () => revoke.resolve());
      expect(screen.queryByText(/Revoked Claude/)).not.toBeInTheDocument();
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });

    it("clears a delayed revoke after Details closes and reopens successfully", async () => {
      const revoke = deferred<void>();
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      vi.mocked(bridge.mcp_revoke_grant).mockReturnValue(revoke.promise);
      renderSettings();
      let dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
      dialog = await openDetailsDialog();
      expect(within(dialog).getByRole("button", { name: "Revoke Claude (0f1e2d3c)" })).toBeDisabled();
      await act(async () => revoke.resolve());
      await settle();
      expect(within(dialog).queryByText("Claude")).not.toBeInTheDocument();
      expect(document.getElementById("mcp-grant-status")).toHaveTextContent("");
    });

    it("clears a delayed revoke after Details closes and reopens on failure", async () => {
      const revoke = deferred<void>();
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      vi.mocked(bridge.mcp_revoke_grant).mockReturnValue(revoke.promise);
      renderSettings();
      let dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
      dialog = await openDetailsDialog();
      await act(async () => revoke.reject({ code: "network", message: "native revoke failed" }));
      await settle();
      expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
      expect(within(dialog).getByRole("button", { name: "Revoke Claude (0f1e2d3c)" })).toBeEnabled();
    });

    it.each([
      ["browser", { code: "mcp_grant_not_found", message: "The MCP server could not complete the request." }],
      ["native", { code: "server-rejected", message: "The server rejected the request (HTTP 404, mcp_grant_not_found)." }],
    ])("treats a direct %s grant-not-found response as an already revoked success", async (_host, reason) => {
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      vi.mocked(bridge.mcp_revoke_grant).mockRejectedValue(reason);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" }));
      expect(await within(dialog).findByText("Claude (0f1e2d3c) was already revoked.")).toBeInTheDocument();
      expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
    });

    it("drops a stale delayed grant result after revoke succeeds", async () => {
      const staleGrants = deferred<McpGrantView[]>();
      const revoke = deferred<void>();
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      vi.mocked(bridge.mcp_revoke_grant).mockReturnValue(revoke.promise);
      renderSettings();
      const dialog = await openDetailsDialog();
      await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" });
      fireEvent.click(within(dialog).getByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" }));
      vi.mocked(bridge.mcp_grants).mockReturnValueOnce(staleGrants.promise);
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      await act(async () => revoke.resolve());
      await act(async () => staleGrants.resolve([GRANT]));
      await settle();
      expect(within(dialog).queryByText("Claude")).not.toBeInTheDocument();
      expect(document.getElementById("mcp-grant-status")).toHaveTextContent("Revoked Claude (0f1e2d3c).");
    });

    it("restores Confirm focus after a native-shaped revoke failure", async () => {
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      vi.mocked(bridge.mcp_revoke_grant).mockRejectedValue({ code: "server-rejected", message: "Could not reach the configured server." });
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      const confirm = within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" });
      fireEvent.click(confirm);
      expect(await within(dialog).findByRole("alert")).toHaveTextContent("Could not reach the configured server.");
      await waitFor(() => expect(confirm).toHaveFocus());
    });

    it("announces a completed revoke, not progress, when a refresh removed the grant first", async () => {
      const revoke = deferred<void>();
      vi.mocked(bridge.mcp_grants).mockResolvedValue([GRANT]);
      vi.mocked(bridge.mcp_revoke_grant).mockReturnValue(revoke.promise);
      renderSettings();
      const dialog = await openDetailsDialog();
      fireEvent.click(await within(dialog).findByRole("button", { name: "Revoke Claude (0f1e2d3c)" }));
      fireEvent.click(within(dialog).getByRole("button", { name: "Confirm revoke Claude (0f1e2d3c)" }));
      vi.mocked(bridge.mcp_grants).mockResolvedValueOnce([]);
      fireEvent.click(within(dialog).getByRole("button", { name: "Refresh status" }));
      expect(await within(dialog).findByText(/No authorized clients yet/)).toBeInTheDocument();
      await act(async () => revoke.resolve());
      expect(document.getElementById("mcp-grant-status")).toHaveTextContent("Revoked Claude (0f1e2d3c).");
      expect(document.getElementById("mcp-grant-status")).not.toHaveTextContent("Revoking");
    });
  });

  describe("settling after enablement", () => {
    const offlineStatus = device("offline");
    const rowNotice = () => document.querySelector("[data-mcp-row-notice]");

    beforeEach(() => {
      vi.useFakeTimers();
    });

    async function advance(milliseconds: number) {
      await act(async () => { vi.advanceTimersByTime(milliseconds); });
    }

    /** Opens the consent from the unenrolled row, consents, and enables; the enable result is whatever the bridge mock returns. */
    async function enableFromRow() {
      fireEvent.click(screen.getByRole("button", { name: /review custody consent/i }));
      fireEvent.click(screen.getByRole("checkbox", { name: /Peppy can decrypt/i }));
      fireEvent.click(screen.getByRole("button", { name: /enable Peppy MCP/i }));
      await settle();
    }

    it("polls after an offline enable result until the worker comes online, then stops", async () => {
      vi.mocked(bridge.mcp_enable).mockResolvedValue(offlineStatus);
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(ready).mockResolvedValueOnce(offlineStatus).mockImplementation(fresh(onlineStatus));
      renderSettings();
      await settle();
      await enableFromRow();
      expect(screen.getByText("Offline")).toBeInTheDocument();
      expect(rowNotice()).toHaveTextContent(OFFLINE_HINT);

      await advance(5_000);
      await settle();
      expect(screen.getByText("Offline")).toBeInTheDocument();

      await advance(5_000);
      await settle();
      expect(screen.getByText("Online")).toBeInTheDocument();
      expect(rowNotice()).toBeNull();

      const calls = vi.mocked(bridge.mcp_status).mock.calls.length;
      await advance(60_000);
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(calls);
    });

    it("points the row to Details after 24 polls without an online state", async () => {
      vi.mocked(bridge.mcp_enable).mockResolvedValue(offlineStatus);
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(ready).mockImplementation(fresh(offlineStatus));
      renderSettings();
      await settle();
      await enableFromRow();
      const afterEnable = vi.mocked(bridge.mcp_status).mock.calls.length;
      for (let tick = 0; tick < MAX_POLLS; tick += 1) await advance(5_000);
      await advance(120_000);
      await settle();
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(afterEnable + MAX_POLLS);
      expect(rowNotice()).toHaveTextContent("Still offline. Open Details to check again.");
    });

    it("stops settling polls when the session changes mid-poll and drops the notice", async () => {
      vi.mocked(bridge.mcp_enable).mockResolvedValue(offlineStatus);
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(ready).mockImplementation(fresh(offlineStatus));
      const { rerender } = renderSettings();
      await settle();
      await enableFromRow();
      await advance(5_000);
      rerender(<McpDeviceSettings roster={owner} serverOffline={false} accountReadOnly={false} sessionKey="vault-b" unlocked />);
      await settle();
      expect(rowNotice()).toBeNull();
      const calls = vi.mocked(bridge.mcp_status).mock.calls.length;
      await advance(60_000);
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(calls);
    });

    it("does not poll a device that was already offline before the user acted", async () => {
      vi.mocked(bridge.mcp_status).mockImplementation(fresh(offlineStatus));
      renderSettings();
      await settle();
      expect(screen.getByText("Offline")).toBeInTheDocument();
      const calls = vi.mocked(bridge.mcp_status).mock.calls.length;
      await advance(120_000);
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(calls);
      expect(rowNotice()).toBeNull();
    });

    it("restarts a fresh 24-poll budget when Details opens after the settling budget is exhausted", async () => {
      vi.mocked(bridge.mcp_enable).mockResolvedValue(offlineStatus);
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(ready).mockImplementation(fresh(offlineStatus));
      renderSettings();
      await settle();
      await enableFromRow();
      for (let tick = 0; tick < MAX_POLLS; tick += 1) await advance(5_000);
      await settle();
      expect(rowNotice()).toHaveTextContent(ROW_STILL_WAITING);

      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      const dialog = screen.getByRole("dialog", { name: "Peppy MCP" });
      expect(dialog.querySelector("#mcp-status-notice")).toHaveTextContent(OFFLINE_HINT);
      const reopened = vi.mocked(bridge.mcp_status).mock.calls.length;
      for (let tick = 1; tick <= MAX_POLLS; tick += 1) {
        await advance(5_000);
        expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(reopened + tick);
      }
      await advance(120_000);
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(reopened + MAX_POLLS);
      await settle();
      expect(dialog.querySelector("#mcp-status-notice")).toHaveTextContent(STILL_WAITING);
    });

    it("keeps exactly one polling interval when Details opens during settling", async () => {
      vi.mocked(bridge.mcp_enable).mockResolvedValue(offlineStatus);
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(ready).mockImplementation(fresh(offlineStatus));
      renderSettings();
      await settle();
      await enableFromRow();
      await advance(5_000);
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      const opened = vi.mocked(bridge.mcp_status).mock.calls.length;
      for (let tick = 1; tick <= 3; tick += 1) {
        await advance(5_000);
        expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(opened + tick);
      }
    });
  });

  describe("removed and cleanup presentation", () => {
    const removedNotice = "Peppy MCP was removed. Close this and select Re-enable to set it up again.";

    it("tells the owner a removed device must be re-enabled, without endpoint actions, steps, or grants", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(device("revoked"));
      renderSettings();
      const dialog = await openDetailsDialog();
      const notice = dialog.querySelector("#mcp-removed-notice");
      expect(notice).toHaveAttribute("role", "status");
      expect(notice).toHaveTextContent(removedNotice);
      expect(within(dialog).queryByRole("button", { name: "Copy endpoint" })).toBeNull();
      expect(within(dialog).queryByRole("button", { name: "Refresh status" })).toBeNull();
      expect(within(dialog).queryByRole("heading", { name: "Connect an AI client" })).toBeNull();
      expect(within(dialog).queryByRole("heading", { name: "Authorized clients" })).toBeNull();
      expect(within(dialog).queryByText("https://vault.example/mcp")).toBeNull();
      expect(bridge.mcp_grants).not.toHaveBeenCalled();
      expect(screen.getByRole("button", { name: "Review custody consent for Peppy MCP" })).toHaveTextContent("Re-enable");
    });

    it("drops the re-enable instruction when re-enable is unavailable", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(device("revoked"));
      renderSettings({ unlocked: false });
      const dialog = await openDetailsDialog();
      expect(dialog.querySelector("#mcp-removed-notice")).toHaveTextContent(/^Peppy MCP was removed\.$/);
      expect(within(dialog).queryByText(/select Re-enable/)).toBeNull();
    });

    it("keeps Refresh and the cleanup hint, and hides endpoint actions and steps, while cleanup runs", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(device("revoked", true));
      renderSettings();
      const dialog = await openDetailsDialog();
      expect(dialog.querySelector("#mcp-status-notice")).toHaveTextContent(CLEANUP_HINT);
      expect(within(dialog).getByRole("button", { name: "Refresh status" })).toBeInTheDocument();
      expect(within(dialog).queryByRole("button", { name: "Copy endpoint" })).toBeNull();
      expect(within(dialog).queryByRole("heading", { name: "Connect an AI client" })).toBeNull();
      expect(dialog.querySelector("#mcp-removed-notice")).toBeNull();
      expect(within(dialog).getByRole("heading", { name: "Authorized clients" })).toBeInTheDocument();
      await waitFor(() => expect(bridge.mcp_grants).toHaveBeenCalled());
    });

    it("moves a live cleanup-to-removed transition to the notice, stops polling and grant requests, and keeps focus", async () => {
      vi.useFakeTimers();
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(device("revoked", true)).mockResolvedValueOnce(device("revoked", true)).mockImplementation(fresh(device("revoked")));
      renderSettings();
      await settle();
      fireEvent.click(screen.getByRole("button", { name: /Peppy MCP details/i }));
      await settle();
      const dialog = screen.getByRole("dialog", { name: "Peppy MCP" });
      within(dialog).getByRole("button", { name: "Refresh status" }).focus();
      expect(dialog.querySelector("#mcp-status-notice")).toHaveTextContent(CLEANUP_HINT);
      const grantCalls = vi.mocked(bridge.mcp_grants).mock.calls.length;

      await act(async () => { vi.advanceTimersByTime(5_000); });
      await settle();
      expect(dialog.querySelector("#mcp-removed-notice")).toHaveTextContent("Peppy MCP was removed.");
      expect(within(dialog).queryByRole("button", { name: "Refresh status" })).toBeNull();
      expect(within(dialog).queryByRole("button", { name: "Copy endpoint" })).toBeNull();
      expect(within(dialog).getByRole("button", { name: "Close" })).toHaveFocus();
      expect(dialog.querySelector("#mcp-status-notice")).not.toHaveTextContent("Cleanup finished.");
      expect(bridge.mcp_grants).toHaveBeenCalledTimes(grantCalls);

      const statusCalls = vi.mocked(bridge.mcp_status).mock.calls.length;
      await act(async () => { vi.advanceTimersByTime(60_000); });
      expect(vi.mocked(bridge.mcp_status).mock.calls.length).toBe(statusCalls);
      expect(bridge.mcp_grants).toHaveBeenCalledTimes(grantCalls);
    });

    it("keeps the endpoint actions, steps, and grants for an online device", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(onlineStatus);
      renderSettings();
      const dialog = await openDetailsDialog();
      expect(within(dialog).getByRole("button", { name: "Copy endpoint" })).toBeInTheDocument();
      expect(within(dialog).getByRole("button", { name: "Refresh status" })).toBeInTheDocument();
      expect(within(dialog).getByRole("heading", { name: "Connect an AI client" })).toBeInTheDocument();
      expect(within(dialog).getByRole("heading", { name: "Authorized clients" })).toBeInTheDocument();
      expect(dialog.querySelector("#mcp-removed-notice")).toBeNull();
      await waitFor(() => expect(bridge.mcp_grants).toHaveBeenCalled());
    });
  });
});
