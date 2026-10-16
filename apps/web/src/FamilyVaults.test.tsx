import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FamilyVaults, type FamilyVaultsBridge } from "./FamilyVaults";

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
vi.stubGlobal("EventSource", class extends FakeEventSource { public constructor() { super(); sources.push(this); } });

const bridge = () => ({
  enrollment_list_vaults: vi.fn<FamilyVaultsBridge["enrollment_list_vaults"]>(async () => []), enrollment_create_invite: vi.fn(), enrollment_list_invites: vi.fn(async () => []), enrollment_revoke_invite: vi.fn(),
  enrollment_list_vault_requests: vi.fn(async () => []), enrollment_approve_vault_request: vi.fn(), enrollment_deny_vault_request: vi.fn(),
});

afterEach(() => { cleanup(); sources.splice(0); });

describe("FamilyVaults", () => {
  it("renders live snapshots and closes its stream on unmount", async () => {
    const view = render(<FamilyVaults bridge={bridge()} />);
    await waitFor(() => expect(sources).toHaveLength(1));
    sources[0]?.snapshot(JSON.stringify({ type: "snapshot", requests: [{ request_id: "request-1", display_name: "Avery", client: "browser", state: "pending", created_at: "2026-01-01T00:00:00Z", expires_in_seconds: 60 }] }));
    expect(await screen.findByText("Avery")).toBeTruthy();
    view.unmount();
    expect(sources[0]?.closed).toBe(true);
  });

  it("pauses after an SSE error only after checking the request list", async () => {
    const mockedBridge = bridge();
    render(<FamilyVaults bridge={mockedBridge} />);
    sources[0]?.onerror?.();
    await waitFor(() => expect(mockedBridge.enrollment_list_vault_requests).toHaveBeenCalled());
    expect(screen.getByRole("status").textContent).toContain("Live updates are paused");
  });

  it("creates only one invite while the create request is pending", async () => {
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

  it("shows an em dash when a vault creation timestamp is nonpositive", async () => {
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
});
