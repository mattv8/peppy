import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FamilyVaults, type FamilyVaultsBridge } from "./FamilyVaults";
import type { VaultRequestView } from "./browser-bridge";
import { peppyCopy } from "../../desktop/src/generated/peppyCopy";

vi.mock("qrcode", () => ({ default: { toDataURL: vi.fn(async () => "data:image/png;base64,qr") } }));

class FakeEventSource {
  public onopen: (() => void) | null = null;
  public onerror: (() => void) | null = null;
  public closed = false;
  private readonly listeners = new Map<string, (event: MessageEvent<string>) => void>();
  public addEventListener(type: string, listener: (event: MessageEvent<string>) => void) { this.listeners.set(type, listener); }
  public close() { this.closed = true; }
  public snapshot(data: string) { this.listeners.get("snapshot")?.(new MessageEvent("snapshot", { data })); }
}

const sources: FakeEventSource[] = [];

const bridge = () => ({
  enrollment_list_vaults: vi.fn<FamilyVaultsBridge["enrollment_list_vaults"]>(async () => []), enrollment_create_invite: vi.fn(), enrollment_list_invites: vi.fn<FamilyVaultsBridge["enrollment_list_invites"]>(async () => []), enrollment_revoke_invite: vi.fn(),
  enrollment_list_vault_requests: vi.fn<FamilyVaultsBridge["enrollment_list_vault_requests"]>(async () => []), enrollment_approve_vault_request: vi.fn(), enrollment_deny_vault_request: vi.fn(),
});

afterEach(() => { cleanup(); sources.splice(0); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

function installEventSource() {
  vi.stubGlobal("EventSource", class extends FakeEventSource { public constructor() { super(); sources.push(this); } });
}

describe("FamilyVaults", () => {
  it("renders live snapshots and closes its stream on unmount", async () => {
    installEventSource();
    const view = render(<FamilyVaults bridge={bridge()} />);
    await waitFor(() => expect(sources).toHaveLength(1));
    sources[0]?.snapshot(JSON.stringify({ type: "snapshot", requests: [{ request_id: "request-1", display_name: "Avery", client: "browser", state: "pending", created_at: "2026-01-01T00:00:00Z", expires_in_seconds: 60 }] }));
    expect(await screen.findByText("Avery")).toBeTruthy();
    view.unmount();
    expect(sources[0]?.closed).toBe(true);
  });

  it("pauses after an SSE error only after checking the request list", async () => {
    installEventSource();
    const mockedBridge = bridge();
    render(<FamilyVaults bridge={mockedBridge} />);
    sources[0]?.onerror?.();
    await waitFor(() => expect(mockedBridge.enrollment_list_vault_requests).toHaveBeenCalled());
    expect(screen.getByRole("status").textContent).toContain("Live updates are paused");
  });

  it("creates only one invite while the create request is pending", async () => {
    installEventSource();
    let resolveInvite!: (value: { inviteId: string; inviteLink: string; expiresInSeconds: number }) => void;
    const invite = new Promise<{ inviteId: string; inviteLink: string; expiresInSeconds: number }>((resolve) => { resolveInvite = resolve; });
    const mockedBridge = bridge();
    mockedBridge.enrollment_create_invite.mockReturnValue(invite);
    render(<FamilyVaults bridge={mockedBridge} />);

    const create = screen.getByRole("button", { name: "Create invite link" });
    fireEvent.click(create);
    fireEvent.click(create);

    expect(mockedBridge.enrollment_create_invite).toHaveBeenCalledOnce();
    expect(create).toHaveProperty("disabled", true);
    resolveInvite({ inviteId: "invite-1", inviteLink: "https://example.test/#invite=token", expiresInSeconds: 60 });
    expect(await screen.findByText("Share this invite link")).toBeTruthy();
  });

  it("shows the invite limit guidance for the structured invite_limit response", async () => {
    installEventSource();
    const mockedBridge = bridge();
    mockedBridge.enrollment_create_invite.mockRejectedValueOnce(new Error("invite_limit"));
    render(<FamilyVaults bridge={mockedBridge} />);

    fireEvent.click(screen.getByRole("button", { name: "Create invite link" }));

    expect((await screen.findByRole("alert")).textContent).toBe(peppyCopy.family_vaults_error_invite_limit);
  });

  it("shows an em dash when a vault creation timestamp is nonpositive", async () => {
    installEventSource();
    const mockedBridge = bridge();
    mockedBridge.enrollment_list_vaults.mockResolvedValue([{
      vaultId: "vault-1",
      username: "Avery",
      isAdmin: false,
      deviceCount: 1,
      storageBytes: 0,
      createdAt: "-1",
    }]);
    render(<FamilyVaults bridge={mockedBridge} />);
    const vault = await screen.findByText("Avery");
    expect(vault.closest("tr")?.textContent).toContain("—");
  });

  it("keeps a created invite link when its follow-up refresh fails", async () => {
    installEventSource();
    const mockedBridge = bridge();
    mockedBridge.enrollment_create_invite.mockResolvedValue({ inviteId: "invite-1", inviteLink: "https://example.test/#invite=token", expiresInSeconds: 60 });
    render(<FamilyVaults bridge={mockedBridge} />);
    await waitFor(() => expect(mockedBridge.enrollment_list_invites).toHaveBeenCalled());
    mockedBridge.enrollment_list_invites.mockRejectedValueOnce(new Error("offline"));

    fireEvent.click(screen.getByRole("button", { name: "Create invite link" }));

    expect(await screen.findByText("https://example.test/#invite=token")).toBeTruthy();
    expect((await screen.findByRole("alert")).textContent).toBe("Peppy could not refresh family vaults.");
  });

  it("offers a refresh-only retry after a connected create succeeds but list refresh fails", async () => {
    installEventSource();
    const mockedBridge = bridge();
    mockedBridge.enrollment_create_invite.mockResolvedValue({ inviteId: "invite-1", inviteLink: "https://example.test/#invite=token", expiresInSeconds: 60 });
    render(<FamilyVaults bridge={mockedBridge} />);
    await waitFor(() => expect(mockedBridge.enrollment_list_invites).toHaveBeenCalledOnce());
    mockedBridge.enrollment_list_invites.mockRejectedValueOnce(new Error("offline"));

    fireEvent.click(screen.getByRole("button", { name: "Create invite link" }));
    expect(await screen.findByText("https://example.test/#invite=token")).toBeTruthy();
    const refresh = await screen.findByRole("button", { name: "Refresh family vaults" });
    expect(sources[0]?.closed).toBe(false);

    fireEvent.click(refresh);
    await waitFor(() => expect(mockedBridge.enrollment_list_invites).toHaveBeenCalledTimes(3));
    expect(mockedBridge.enrollment_create_invite).toHaveBeenCalledOnce();
    expect(sources[0]?.closed).toBe(false);
  });

  it("uses the server's request_not_pending decision without treating access failures as a decision", async () => {
    installEventSource();
    const mockedBridge = bridge();
    mockedBridge.enrollment_list_vault_requests.mockResolvedValue([{
      requestId: "request-1", displayName: "Avery", client: "browser", state: "pending", createdAt: "2026-01-01T00:00:00Z", expiresInSeconds: 60,
    }] satisfies VaultRequestView[]);
    mockedBridge.enrollment_approve_vault_request.mockRejectedValueOnce(new Error("http-403"));
    render(<FamilyVaults bridge={mockedBridge} />);

    fireEvent.click(await screen.findByRole("button", { name: peppyCopy.family_vaults_approve_request }));
    expect((await screen.findByRole("alert")).textContent).toBe("Peppy could not approve this request. Check your access and try again.");

    mockedBridge.enrollment_approve_vault_request.mockRejectedValueOnce(new Error("request_not_pending"));
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.family_vaults_approve_request }));
    expect((await screen.findByRole("alert")).textContent).toBe(peppyCopy.family_vaults_error_request_not_pending);
  });

  it.each(["network", "http-401", "http-403"])("does not describe deny %s failures as no longer pending", async (code) => {
    installEventSource();
    const mockedBridge = bridge();
    mockedBridge.enrollment_list_vault_requests.mockResolvedValue([{
      requestId: "request-1", displayName: "Avery", client: "browser", state: "pending", createdAt: "2026-01-01T00:00:00Z", expiresInSeconds: 60,
    }] satisfies VaultRequestView[]);
    mockedBridge.enrollment_deny_vault_request.mockRejectedValueOnce(new Error(code));
    render(<FamilyVaults bridge={mockedBridge} />);

    fireEvent.click(await screen.findByRole("button", { name: peppyCopy.family_vaults_deny_request }));
    fireEvent.click(document.getElementById("family-vaults-deny-confirm-yes-request-1")!);
    expect((await screen.findByRole("alert")).textContent).not.toBe(peppyCopy.family_vaults_error_request_not_pending);
  });

  it("renders the shared hour-relative age in the request row", async () => {
    installEventSource();
    const now = Date.now();
    const mockedBridge = bridge();
    mockedBridge.enrollment_list_vault_requests.mockResolvedValue([{
      requestId: "request-1", displayName: "Avery", client: "browser", state: "pending", createdAt: new Date(now - 5 * 60 * 60_000).toISOString(), expiresInSeconds: 60,
    }] satisfies VaultRequestView[]);
    render(<FamilyVaults bridge={mockedBridge} />);

    const expected = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" }).format(-5, "hour");
    expect((await screen.findByText("Avery")).closest("article")?.textContent).toContain(expected);
  });

  describe("SSE reconnect backoff (M22)", () => {
    const flush = () => act(async () => { for (let tick = 0; tick < 5; tick += 1) await Promise.resolve(); });
    const advance = (ms: number) => act(() => { vi.advanceTimersByTime(ms); });
    const failSource = async (index: number) => {
      act(() => sources[index]?.onerror?.());
      await flush();
    };

    async function renderLive(mockedBridge = bridge()) {
      installEventSource();
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
      const view = render(<FamilyVaults bridge={mockedBridge} />);
      await flush();
      expect(sources).toHaveLength(1);
      return { view, mockedBridge };
    }

    it("pauses, reports, and still reconnects after the fallback request refresh fails", async () => {
      const { mockedBridge } = await renderLive();
      mockedBridge.enrollment_list_vault_requests.mockRejectedValueOnce(new Error("offline"));

      await failSource(0);

      expect(screen.getByRole("status")).toBeTruthy();
      expect(screen.getByRole("alert")).toBeTruthy();
      expect(sources[0]?.closed).toBe(true);
      await advance(1_999);
      expect(sources).toHaveLength(1);
      await advance(1);
      expect(sources).toHaveLength(2);
    });

    it("reconnects after a successful fallback refresh with a doubled backoff", async () => {
      await renderLive();

      await failSource(0);
      await advance(2_000);
      expect(sources).toHaveLength(2);
      await failSource(1);
      await advance(3_999);
      expect(sources).toHaveLength(2);
      await advance(1);
      expect(sources).toHaveLength(3);
    });

    it("keeps reconnecting after the live effect re-runs while a backoff is pending", async () => {
      const { view } = await renderLive();
      await failSource(0);

      view.rerender(<FamilyVaults bridge={bridge()} />);
      await flush();
      expect(sources).toHaveLength(2);
      await failSource(1);
      await advance(60_000);

      expect(sources).toHaveLength(3);
    });

    it("keeps reconnecting after a manual refresh replaces a pending backoff", async () => {
      await renderLive();
      await failSource(0);

      fireEvent.click(screen.getByRole("button", { name: peppyCopy.family_vaults_live_refresh }));
      await flush();
      expect(sources).toHaveLength(2);
      await failSource(1);
      await advance(60_000);

      expect(sources).toHaveLength(3);
    });

    it("cancels a pending reconnect on unmount", async () => {
      const { view } = await renderLive();
      await failSource(0);
      expect(vi.getTimerCount()).toBe(1);

      view.unmount();
      await advance(60_000);

      expect(vi.getTimerCount()).toBe(0);
      expect(sources).toHaveLength(1);
    });

    it("schedules no reconnect when the fallback response arrives after unmount", async () => {
      let resolveLate!: (requests: never[]) => void;
      const { view, mockedBridge } = await renderLive();
      mockedBridge.enrollment_list_vault_requests.mockReturnValueOnce(new Promise((resolve) => { resolveLate = resolve; }));
      act(() => sources[0]?.onerror?.());

      view.unmount();
      await act(async () => resolveLate([]));
      await advance(60_000);

      expect(vi.getTimerCount()).toBe(0);
      expect(sources).toHaveLength(1);
    });
  });
});
