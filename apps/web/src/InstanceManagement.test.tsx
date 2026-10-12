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

afterEach(cleanup);

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

  it("shows only the signed-in identity to a non-admin owner", async () => {
    render(<InstanceManagement bridge={bridge({ enrollment_management: vi.fn(async () => ({ ...management, isAdmin: false, username: "sasha" })) })} />);
    expect(await screen.findByText("sasha")).toBeTruthy();
    expect(document.querySelector("#family-vaults")).toBeNull();
  });
});
