import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { InstanceManagement } from "./InstanceManagement";

const management = {
  vault_id: "00000000-0000-0000-0000-000000000001",
  isAdmin: true,
  username: "admin",
  devices: [{ device_id: "00000000-0000-0000-0000-000000000002", name: "Owner laptop", kind: "desktop" as const, role: "owner" as const, revoked: false, gateway_online: false }],
  storage_bytes: 1024,
  quota_bytes: 4096,
  replay_retention_days: 30,
};

function bridge(overrides: Partial<Parameters<typeof InstanceManagement>[0]["bridge"]> = {}) {
  return {
    enrollment_discover: vi.fn(async () => ({ mode: "community" as const, stage: "signin" as const })),
    enrollment_sign_in: vi.fn(async () => ({ mode: "community" as const, stage: "locked" as const })),
    enrollment_management: vi.fn(async () => management),
    enrollment_revoke_device: vi.fn(async () => management),
    enrollment_list_vaults: vi.fn(async () => []),
    enrollment_create_invite: vi.fn(),
    enrollment_list_invites: vi.fn(async () => []),
    enrollment_revoke_invite: vi.fn(),
    enrollment_list_vault_requests: vi.fn(async () => []),
    enrollment_approve_vault_request: vi.fn(),
    enrollment_deny_vault_request: vi.fn(),
    ...overrides,
  };
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("InstanceManagement", () => {
  it("hides the roster when the instance session is unauthorized", async () => {
    render(<InstanceManagement bridge={bridge({ enrollment_management: vi.fn(async () => { throw { code: "http-401" }; }) })} />);
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeTruthy();
    expect(screen.queryByText("Owner laptop")).toBeNull();
  });

  it("shows an unavailable state when management fails without an explicit access code", async () => {
    render(<InstanceManagement bridge={bridge({ enrollment_management: vi.fn(async () => { throw {}; }) })} />);
    expect(await screen.findByRole("heading", { name: "Instance management is unavailable" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Sign in" })).toBeNull();
  });

  it("forwards a non-admin username when signing in", async () => {
    const mockedBridge = bridge({ enrollment_management: vi.fn(async () => { throw { code: "http-401" }; }) });
    render(<InstanceManagement bridge={mockedBridge} />);
    fireEvent.change(await screen.findByLabelText("Username"), { target: { value: "sasha" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(mockedBridge.enrollment_sign_in).toHaveBeenCalledWith({ username: "sasha" }));
  });

  it("keeps the username and shows feedback for a resolved sign-in error stage", async () => {
    const mockedBridge = bridge({ enrollment_management: vi.fn(async () => { throw { code: "http-401" }; }), enrollment_sign_in: vi.fn(async () => ({ mode: "community" as const, stage: "error" as const })) });
    render(<InstanceManagement bridge={mockedBridge} />);
    fireEvent.change(await screen.findByLabelText("Username"), { target: { value: "sasha" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect((await screen.findByRole("alert")).textContent).toContain("could not sign in");
    expect((screen.getByLabelText("Username") as HTMLInputElement).value).toBe("sasha");
  });

  it("keeps the displayed roster when revocation fails", async () => {
    render(<InstanceManagement bridge={bridge({ enrollment_revoke_device: vi.fn(async () => { throw new Error("network"); }) })} />);
    expect(await screen.findByText("Owner laptop")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Revoke" }));
    fireEvent.click(screen.getByRole("button", { name: "Revoke device" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("No device access was changed"));
    expect(screen.getByText("Owner laptop")).toBeTruthy();
  });

  it("does not expose instance administration for hosted accounts", async () => {
    const hosted = bridge({ enrollment_discover: vi.fn(async () => ({ mode: "hosted" as const, stage: "ready" as const })) });
    render(<InstanceManagement bridge={hosted} accountUrl="https://accounts.example.test/account" />);
    expect((await screen.findByRole("link", { name: /Open account portal/ })).getAttribute("href")).toBe("https://accounts.example.test/account");
    expect(hosted.enrollment_management).not.toHaveBeenCalled();
    expect(screen.queryByText("Devices")).toBeNull();
  });

  it("renders quota and retention as read-only status", async () => {
    render(<InstanceManagement bridge={bridge()} />);
    expect((await screen.findByText("Storage", { selector: "#instance-management-status dt" })).textContent).toBe("Storage");
    expect(screen.getAllByText("Replay retention")).toHaveLength(1);
    expect(document.querySelector("#instance-management-status input, #instance-management-status select, #instance-management-status button")).toBeNull();
  });

  it("uses phone wording for the gateway role and health column", async () => {
    render(<InstanceManagement bridge={bridge({ enrollment_management: vi.fn(async () => ({ ...management, devices: [{ ...management.devices[0], kind: "android" as const, role: "gateway" as const, gateway_online: true, presence: { state: "connected" as const, last_seen_age_seconds: 0 } }] })) })} />);
    expect(await screen.findByRole("columnheader", { name: "Phone health" })).toBeTruthy();
    expect(screen.getByRole("cell", { name: "Phone" })).toBeTruthy();
    expect(screen.getByRole("cell", { name: "Connected" })).toBeTruthy();
    expect(screen.queryByText(/gateway/i)).toBeNull();
  });

  it("does not treat legacy gateway_online as proof that a phone is connected", async () => {
    render(<InstanceManagement bridge={bridge({ enrollment_management: vi.fn(async () => ({ ...management, devices: [{ ...management.devices[0], kind: "android" as const, role: "gateway" as const, gateway_online: true }] })) })} />);
    expect(await screen.findByRole("cell", { name: "Status unknown" })).toBeTruthy();
  });

  it("shows the reported age for a seen phone", async () => {
    render(<InstanceManagement bridge={bridge({ enrollment_management: vi.fn(async () => ({ ...management, devices: [{ ...management.devices[0], kind: "android" as const, presence: { state: "seen" as const, last_seen_age_seconds: 300 } }] })) })} />);
    expect(await screen.findByRole("cell", { name: "Last seen 5m ago" })).toBeTruthy();
  });

  it("shows only the signed-in identity to a non-admin owner", async () => {
    render(<InstanceManagement bridge={bridge({ enrollment_management: vi.fn(async () => ({ ...management, isAdmin: false, username: "sasha" })) })} />);
    expect(await screen.findByText("sasha")).toBeTruthy();
    expect(document.querySelector("#family-vaults")).toBeNull();
  });

  it("opens revoke as a modal and returns focus to the row button on cancel", async () => {
    render(<InstanceManagement bridge={bridge()} />);
    const revokeButton = await screen.findByRole("button", { name: "Revoke" });
    fireEvent.click(revokeButton);
    expect(screen.getByRole("dialog", { name: "Revoke device?" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Revoke device?" })).toBeNull());
    expect(document.activeElement).toBe(revokeButton);
  });

  it("shows a failed revoke inside the still-open modal", async () => {
    render(<InstanceManagement bridge={bridge({ enrollment_revoke_device: vi.fn(async () => { throw new Error("network"); }) })} />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke" }));
    fireEvent.click(screen.getByRole("button", { name: "Revoke device" }));
    const dialog = screen.getByRole("dialog", { name: "Revoke device?" });
    await waitFor(() => expect(dialog.querySelector('[role="alert"]')?.textContent).toContain("No device access was changed"));
    expect(screen.getAllByRole("alert")).toHaveLength(1);
  });

  it("moves focus to the Devices heading after a successful revoke", async () => {
    const revoked = { ...management, devices: management.devices.map((device) => ({ ...device, revoked: true })) };
    render(<InstanceManagement bridge={bridge({ enrollment_revoke_device: vi.fn(async () => revoked) })} />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke" }));
    fireEvent.click(screen.getByRole("button", { name: "Revoke device" }));
    const devicesHeading = screen.getByRole("heading", { name: "Devices" });
    await waitFor(() => expect(document.activeElement).toBe(devicesHeading));
    expect(screen.queryByRole("dialog", { name: "Revoke device?" })).toBeNull();
  });

  it("closes revoke on Escape without calling revoke", async () => {
    const mockedBridge = bridge();
    render(<InstanceManagement bridge={mockedBridge} />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke" }));
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Revoke device?" })).toBeNull());
    expect(mockedBridge.enrollment_revoke_device).not.toHaveBeenCalled();
  });

  describe("sign-in outcomes (M23)", () => {
    const signInWith = async (enrollment_sign_in: ReturnType<typeof bridge>["enrollment_sign_in"]) => {
      const mockedBridge = bridge({ enrollment_management: vi.fn(async () => { throw { code: "http-401" }; }), enrollment_sign_in });
      render(<InstanceManagement bridge={mockedBridge} />);
      fireEvent.change(await screen.findByLabelText("Username"), { target: { value: "sasha" } });
      fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
      await waitFor(() => expect(mockedBridge.enrollment_sign_in).toHaveBeenCalledWith({ username: "sasha" }));
      return mockedBridge;
    };

    it.each([
      ["a resolved error stage", vi.fn(async () => ({ mode: "community" as const, stage: "error" as const }))],
      ["a thrown sign-in failure", vi.fn(async () => { throw new Error("network"); })],
    ])("keeps the sign-in screen, username, and alert without loading management for %s", async (_outcome, signIn) => {
      const mockedBridge = await signInWith(signIn);

      expect(await screen.findByRole("alert")).toBeTruthy();
      expect((screen.getByLabelText("Username") as HTMLInputElement).value).toBe("sasha");
      expect(mockedBridge.enrollment_management).toHaveBeenCalledOnce();
    });

    it("stays quiet when a cancelled sign-in returns the existing sign-in status", async () => {
      const mockedBridge = await signInWith(vi.fn(async () => ({ mode: "community" as const, stage: "signin" as const })));

      expect(await screen.findByLabelText("Username")).toBeTruthy();
      expect(screen.queryByRole("alert")).toBeNull();
      expect((screen.getByLabelText("Username") as HTMLInputElement).value).toBe("sasha");
      expect(mockedBridge.enrollment_management).toHaveBeenCalledTimes(2);
    });

    it("loads management after a successful sign-in", async () => {
      const managementLoad = vi.fn<() => Promise<typeof management>>()
        .mockRejectedValueOnce({ code: "http-401" })
        .mockResolvedValue(management);
      const mockedBridge = bridge({ enrollment_management: managementLoad });
      render(<InstanceManagement bridge={mockedBridge} />);
      fireEvent.change(await screen.findByLabelText("Username"), { target: { value: "admin" } });
      fireEvent.click(screen.getByRole("button", { name: "Sign in" }));

      expect(await screen.findByText("Owner laptop")).toBeTruthy();
      expect(managementLoad).toHaveBeenCalledTimes(2);
      expect(screen.queryByRole("alert")).toBeNull();
    });
  });
});
