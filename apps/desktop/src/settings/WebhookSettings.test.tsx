import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WebhookSettings, type WebhookSettingsProps } from "./WebhookSettings";
import { MAX_SETUP_CHECKS, SETUP_POLL_INTERVAL_MS, SLOW_CHECK_EVERY_TICKS } from "./useServerClientWatch";
import { bridge } from "../bridge/bridge";
import { WEBHOOK_LIMIT_MESSAGE } from "./webhookDisplay";
import {
  homeAssistant,
  n8n,
  noDeviceStatus,
  offlineStatus,
  readyStatus,
  revokedStatus,
  zapier,
} from "./webhookTestFixtures";
import type { WebhookCreated, WebhookList, WebhookView } from "@peppy/desktop-ui/bridge-contracts";

vi.mock("../bridge/bridge", { spy: true });

const readyList = (webhooks: WebhookView[] = [homeAssistant, zapier, n8n]): WebhookList => ({ available: true, webhooks });

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

function rowFor(id: string): HTMLElement {
  const element = document.getElementById(`webhook-${id}`);
  if (!element) throw new Error(`No row for ${id}`);
  return element;
}

const noticeText = () => document.getElementById("webhooks-notice")?.textContent ?? "";

function renderSettings(props: Partial<WebhookSettingsProps> = {}) {
  const onOpenDevices = vi.fn();
  const result = render(
    <WebhookSettings sessionKey="vault-a" unlocked serverOffline={false} onOpenDevices={onOpenDevices} {...props} />,
  );
  return { ...result, onOpenDevices };
}

function setVisibility(state: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
}

describe("WebhookSettings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(bridge.mcp_status).mockResolvedValue(readyStatus);
    vi.mocked(bridge.webhooks_list).mockResolvedValue(readyList());
  });

  afterEach(() => {
    cleanup();
  });

  describe("states", () => {
    it("asks for an unlock and loads nothing while the vault is locked", () => {
      renderSettings({ unlocked: false });

      expect(screen.getByText("Unlock this vault to manage webhooks.")).toBeInTheDocument();
      expect(bridge.webhooks_list).not.toHaveBeenCalled();
      expect(bridge.mcp_status).not.toHaveBeenCalled();
    });

    it("shows a loading state until the list arrives", () => {
      vi.mocked(bridge.webhooks_list).mockReturnValue(new Promise(() => {}));
      renderSettings();

      expect(screen.getByText("Loading webhooks…")).toHaveAttribute("role", "status");
    });

    it("shows a load error with a retry that reloads the webhooks", async () => {
      vi.mocked(bridge.webhooks_list).mockRejectedValueOnce({ code: "offline", message: "offline" });
      renderSettings();

      expect(await screen.findByRole("alert")).toHaveTextContent("Something went wrong. Try again.");
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));

      expect(await screen.findByRole("heading", { name: "Webhooks" })).toBeInTheDocument();
      expect(bridge.webhooks_list).toHaveBeenCalledTimes(2);
    });

    it("explains that only vault owners can manage webhooks", async () => {
      vi.mocked(bridge.webhooks_list).mockRejectedValue({ code: "owner_required", message: "owner_required" });
      renderSettings();

      expect(await screen.findByText("Only vault owners can manage webhooks.")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Add webhook" })).not.toBeInTheDocument();
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    });

    it.each([
      ["no server client is enabled", noDeviceStatus],
      ["the server client was removed", revokedStatus],
    ])("shows the header and an Enable Peppy MCP callout when %s", async (_label, status) => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(status);
      const { onOpenDevices, container } = renderSettings();

      const callout = await waitFor(() => {
        const element = container.querySelector("#webhooks-needs-server-client");
        if (!element) throw new Error("callout not rendered yet");
        return element as HTMLElement;
      });
      expect(screen.getByRole("heading", { level: 3, name: "Webhooks" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Add webhook" })).toBeDisabled();
      expect(screen.getByRole("button", { name: "Add webhook" })).toHaveAttribute("title", "Enable Peppy MCP in Devices first.");
      expect(callout).toHaveTextContent("Enable Peppy MCP in Devices");
      expect(callout).toHaveTextContent("once Peppy MCP shows Online");
      expect(screen.queryByRole("button", { name: /Webhooks guide/i })).not.toBeInTheDocument();
      fireEvent.click(within(callout).getByRole("button", { name: "Open Devices" }));

      expect(onOpenDevices).toHaveBeenCalledTimes(1);
    });

    it("says webhooks are off when the server has them disabled", async () => {
      vi.mocked(bridge.webhooks_list).mockResolvedValue({ available: false, webhooks: [] });
      const { container } = renderSettings();

      expect(await screen.findByText("Webhooks aren't available because this server doesn't offer the Peppy server client.")).toBeInTheDocument();
      expect(container.querySelector("#webhooks-needs-server-client")).not.toBeInTheDocument();
    });

    it("explains when the server client is being removed", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue({ ...readyStatus, device: { ...readyStatus.device!, cleanupPending: true } });
      renderSettings();

      expect(await screen.findByText("The Peppy server client is being removed. Try again shortly.")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Add webhook" })).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Check again" }));
      expect(bridge.webhooks_list).toHaveBeenCalledTimes(2);
    });
  });

  describe("ready", () => {
    it("shows the header, the online status and one row per webhook", async () => {
      const { container } = renderSettings();

      expect(await screen.findByRole("heading", { level: 3, name: "Webhooks" })).toBeInTheDocument();
      expect(screen.getByText(/Runs on your server, even when your devices are offline/)).toBeInTheDocument();
      expect(container.querySelector("#webhooks-status")).toHaveTextContent("Server client online · synced");
      expect(screen.getByRole("list", { name: "Webhooks" })).toBeInTheDocument();
      expect(container.querySelectorAll("li[data-webhook]")).toHaveLength(3);
    });

    it("opens the Webhooks guide through the bridge and clears its fixed error on retry", async () => {
      vi.mocked(bridge.open_external_url!).mockRejectedValueOnce(new Error("app-locked")).mockResolvedValue(undefined);
      renderSettings();

      fireEvent.click(await screen.findByRole("button", { name: /Webhooks guide/i }));

      expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't open the guide.");
      expect(screen.queryByText("app-locked")).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: /Webhooks guide/i }));
      await waitFor(() => expect(bridge.open_external_url).toHaveBeenLastCalledWith("https://peppy.pro/docs/webhooks"));
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    });

    it("shows a fixed Webhooks guide error when the bridge has no opener", async () => {
      const opener = bridge.open_external_url;
      (bridge as { open_external_url?: typeof opener }).open_external_url = undefined;
      renderSettings();

      fireEvent.click(await screen.findByRole("button", { name: /Webhooks guide/i }));

      expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't open the guide.");
      (bridge as { open_external_url?: typeof opener }).open_external_url = opener;
    });

    it("uses a safe browser link for the Webhooks guide", async () => {
      renderSettings({ browserHost: true });

      const link = await screen.findByRole("link", { name: /Webhooks guide/i });
      expect(link).toHaveAttribute("href", "https://peppy.pro/docs/webhooks");
      expect(link).toHaveAttribute("target", "_blank");
      expect(link).toHaveAttribute("rel", "noopener noreferrer");
    });

    it("reports an offline server client", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(offlineStatus);
      renderSettings();

      expect(await screen.findByText(/Server client offline/)).toBeInTheDocument();
    });

    it("shows an empty state with no webhooks", async () => {
      vi.mocked(bridge.webhooks_list).mockResolvedValue(readyList([]));
      renderSettings();

      expect(await screen.findByText("No webhooks yet. Add one to send incoming messages to your own apps.")).toBeInTheDocument();
      expect(screen.queryByRole("list", { name: "Webhooks" })).not.toBeInTheDocument();
    });

    it("disables Add webhook at the limit and explains why", async () => {
      const many = Array.from({ length: 10 }, (_, index) => ({
        ...homeAssistant,
        id: `00000000-0000-4000-8000-0000000001${String(index).padStart(2, "0")}`,
      }));
      vi.mocked(bridge.webhooks_list).mockResolvedValue(readyList(many));
      renderSettings();

      const add = await screen.findByRole("button", { name: "Add webhook" });
      expect(add).toBeDisabled();
      expect(add).toHaveAttribute("title", WEBHOOK_LIMIT_MESSAGE);
    });

    it("disables Add webhook while the server is offline and explains why", async () => {
      renderSettings({ serverOffline: true });

      const add = await screen.findByRole("button", { name: "Add webhook" });
      expect(add).toBeDisabled();
      expect(add).toHaveAttribute("title", "Reconnect to the server to add webhooks.");
    });

    it("opens the create editor from Add webhook", async () => {
      renderSettings();

      fireEvent.click(await screen.findByRole("button", { name: "Add webhook" }));

      expect(await screen.findByRole("dialog", { name: "Add webhook" })).toBeInTheDocument();
    });

    it("reloads when the session changes", async () => {
      const { rerender } = renderSettings();
      await screen.findByRole("list", { name: "Webhooks" });

      rerender(<WebhookSettings sessionKey="vault-b" unlocked serverOffline={false} onOpenDevices={vi.fn()} />);

      await waitFor(() => expect(bridge.webhooks_list).toHaveBeenCalledTimes(2));
      expect(bridge.mcp_status).toHaveBeenCalledTimes(2);
    });

    it("ignores a save from an earlier session without closing a newer editor", async () => {
      const save = deferred<WebhookCreated>();
      vi.mocked(bridge.webhook_create).mockReturnValue(save.promise);
      const { rerender } = renderSettings();

      fireEvent.click(await screen.findByRole("button", { name: "Add webhook" }));
      fireEvent.change(screen.getByLabelText("Endpoint URL"), { target: { value: "https://a.example.test/hooks" } });
      fireEvent.click(screen.getByRole("button", { name: "Create" }));

      rerender(<WebhookSettings sessionKey="vault-b" unlocked serverOffline={false} onOpenDevices={vi.fn()} />);
      fireEvent.click(await screen.findByRole("button", { name: "Add webhook" }));
      expect(screen.getByRole("dialog", { name: "Add webhook" })).toBeInTheDocument();

      await act(async () => {
        save.resolve({
          webhook: { ...homeAssistant, id: "00000000-0000-4000-8000-000000000099", name: "Session A webhook" },
          secret: "whsec_fixture_session_a",
        });
      });

      expect(screen.queryByText("Session A webhook")).not.toBeInTheDocument();
      expect(screen.getByRole("dialog", { name: "Add webhook" })).toBeInTheDocument();
    });
  });

  describe("row actions", () => {
    it("saves a pause change and shows the server's saved state", async () => {
      vi.mocked(bridge.webhook_update).mockResolvedValue({ ...homeAssistant, paused: true });
      renderSettings();

      fireEvent.click(await screen.findByRole("switch", { name: "Home Assistant active" }));

      expect(bridge.webhook_update).toHaveBeenCalledWith(homeAssistant.id, {
        name: null,
        url: homeAssistant.url,
        events: ["message.received"],
        filterMode: "all",
        filter: [],
        paused: true,
      });
      await waitFor(() => expect(screen.getByRole("switch", { name: "Home Assistant active" })).not.toBeChecked());
      expect(within(rowFor(homeAssistant.id)).getByText("Paused")).toBeInTheDocument();
    });

    it("rolls a failed pause change back and reports it on the row", async () => {
      vi.mocked(bridge.webhook_update).mockRejectedValue({ code: "webhook_not_found", message: "gone" });
      renderSettings();

      fireEvent.click(await screen.findByRole("switch", { name: "Home Assistant active" }));

      const rowAlert = await within(rowFor(homeAssistant.id)).findByRole("alert");
      expect(rowAlert).toHaveTextContent("Couldn't pause Home Assistant. Try again.");
      expect(screen.getByRole("switch", { name: "Home Assistant active" })).toBeChecked();
      expect(within(rowFor(homeAssistant.id)).queryByText("Paused")).not.toBeInTheDocument();
    });

    it("deletes after the inline confirmation, removes the row and announces it", async () => {
      vi.mocked(bridge.webhook_delete).mockResolvedValue(undefined);
      const { container } = renderSettings();

      fireEvent.click(await screen.findByRole("button", { name: "Delete n8n triage" }));
      fireEvent.click(within(rowFor(n8n.id)).getByRole("button", { name: "Delete" }));

      await waitFor(() => expect(container.querySelector(`#webhook-${n8n.id}`)).not.toBeInTheDocument());
      expect(bridge.webhook_delete).toHaveBeenCalledWith(n8n.id);
      expect(noticeText()).toBe("Deleted n8n triage.");
      expect(screen.getByRole("heading", { name: "Webhooks" })).toHaveFocus();
    });

    it("keeps a webhook and reports the error when its deletion fails", async () => {
      vi.mocked(bridge.webhook_delete).mockRejectedValue({ code: "internal", message: "nope" });
      renderSettings();

      fireEvent.click(await screen.findByRole("button", { name: "Delete n8n triage" }));
      fireEvent.click(within(rowFor(n8n.id)).getByRole("button", { name: "Delete" }));

      expect(await within(rowFor(n8n.id)).findByRole("alert")).toHaveTextContent("Something went wrong. Try again.");
      expect(rowFor(n8n.id)).toBeInTheDocument();
    });
  });

  describe("server client set-up watch", () => {
    beforeEach(() => {
      vi.useFakeTimers();
      setVisibility("visible");
    });

    afterEach(() => {
      vi.useRealTimers();
      Reflect.deleteProperty(document, "visibilityState");
    });

    /** Lets resolved promises and React state updates settle under fake timers. */
    const settle = () => act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    const statusCalls = () => vi.mocked(bridge.mcp_status).mock.calls.length;

    it("re-checks the server client while it is missing and loads the list once it appears", async () => {
      // The initial load sees no server client; the first interval tick sees it set up.
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(noDeviceStatus).mockResolvedValue(readyStatus);
      const { container } = renderSettings();
      await settle();
      expect(container.querySelector("#webhooks-needs-server-client")).toBeInTheDocument();
      expect(bridge.webhooks_list).toHaveBeenCalledTimes(1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(SETUP_POLL_INTERVAL_MS);
      });
      await settle();

      expect(container.querySelector("#webhooks-needs-server-client")).not.toBeInTheDocument();
      expect(container.querySelectorAll("li[data-webhook]")).toHaveLength(3);
      expect(bridge.webhooks_list).toHaveBeenCalledTimes(2);
    });

    it("slows polling after the fast budget is spent", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(noDeviceStatus);
      renderSettings();
      await settle();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(SETUP_POLL_INTERVAL_MS * (MAX_SETUP_CHECKS + SLOW_CHECK_EVERY_TICKS * 2));
      });

      // One load, one check per fast tick, then one check per slow period.
      expect(statusCalls()).toBe(1 + MAX_SETUP_CHECKS + 2);
    });

    it("still notices setup finished after the fast budget is spent", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(noDeviceStatus);
      vi.mocked(bridge.webhooks_list).mockResolvedValue(readyList());
      const { container } = renderSettings();
      await settle();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(SETUP_POLL_INTERVAL_MS * (MAX_SETUP_CHECKS + 1));
      });

      vi.mocked(bridge.mcp_status).mockResolvedValue(readyStatus);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(SETUP_POLL_INTERVAL_MS * SLOW_CHECK_EVERY_TICKS);
      });
      await settle();

      expect(container.querySelector("#webhooks-needs-server-client")).not.toBeInTheDocument();
      expect(container.querySelectorAll("li[data-webhook]")).toHaveLength(3);
    });

    it("restarts the budget when the window regains focus", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(noDeviceStatus);
      renderSettings();
      await settle();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(SETUP_POLL_INTERVAL_MS * (MAX_SETUP_CHECKS + 10));
      });
      const exhausted = statusCalls();

      await act(async () => {
        window.dispatchEvent(new Event("focus"));
      });
      await settle();

      expect(statusCalls()).toBe(exhausted + 1);
    });

    it("does not check while the page is hidden and checks again when it becomes visible", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(noDeviceStatus);
      renderSettings();
      await settle();
      const before = statusCalls();

      setVisibility("hidden");
      await act(async () => {
        await vi.advanceTimersByTimeAsync(SETUP_POLL_INTERVAL_MS * 3);
      });
      expect(statusCalls()).toBe(before);

      setVisibility("visible");
      await act(async () => {
        document.dispatchEvent(new Event("visibilitychange"));
      });
      await settle();
      expect(statusCalls()).toBe(before + 1);
    });

    it("stops checking once the server client is set up", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValueOnce(noDeviceStatus).mockResolvedValue(readyStatus);
      renderSettings();
      await settle();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(SETUP_POLL_INTERVAL_MS);
      });
      const afterReady = statusCalls();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(SETUP_POLL_INTERVAL_MS * 5);
      });
      expect(statusCalls()).toBe(afterReady);
    });

    it("cleans up its timer and listeners on unmount", async () => {
      vi.mocked(bridge.mcp_status).mockResolvedValue(noDeviceStatus);
      const { unmount } = renderSettings();
      await settle();
      const before = statusCalls();

      unmount();
      await act(async () => {
        window.dispatchEvent(new Event("focus"));
        await vi.advanceTimersByTimeAsync(SETUP_POLL_INTERVAL_MS * 3);
      });

      expect(statusCalls()).toBe(before);
    });
  });
});
