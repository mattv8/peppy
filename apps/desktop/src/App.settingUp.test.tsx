import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { GatewayView } from "@peppy/desktop-ui/bridge-contracts";
import { App, headRoutePresentation, ownerWithoutPhone, routeProblem } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";
import { resolveStoredRoute, settingUpGatewayFor } from "./setup/settingUpRoute";

const noMmsPlan = { kind: "none" } as never;

const settingUpPhone = (overrides: Partial<GatewayView> = {}): GatewayView => ({
  id: "pixel",
  name: "Pixel 8",
  simId: "",
  online: true,
  simulated: false,
  supportsSms: false,
  supportsMms: false,
  availability: "setting_up",
  settingUp: true,
  ...overrides,
});

const readyPhone: GatewayView = {
  id: "galaxy",
  name: "Galaxy S24",
  simId: "sim-2",
  online: true,
  simulated: false,
  supportsSms: true,
  supportsMms: true,
  mmsContentVersion: 2,
  availability: "connected",
};

describe("routeProblem for a phone that is setting up", () => {
  it("names the setting-up phone before the missing SMS support reason", () => {
    const phone = settingUpPhone();
    expect(routeProblem({ kind: "route", gateway: phone, implicit: true }, [phone], false, 1, noMmsPlan))
      .toBe("Pixel 8 is still setting up. It can send when that finishes.");
  });

  it("points to another phone when another route can send", () => {
    const phone = settingUpPhone();
    expect(routeProblem({ kind: "route", gateway: phone, implicit: false }, [phone, readyPhone], false, 1, noMmsPlan))
      .toBe("Pixel 8 is still setting up. Choose another phone, or wait for it to finish.");
  });

  it("treats a stale route to a setting-up phone as that phone, never as no longer reported", () => {
    const phone = settingUpPhone();
    expect(routeProblem({ kind: "stale", gatewayId: "pixel", simId: "gone" }, [phone], false, 1, noMmsPlan))
      .toBe("Pixel 8 is still setting up. It can send when that finishes.");
  });

  it("keeps the no-longer-reported reason for a stale route to a phone that is not setting up", () => {
    expect(routeProblem({ kind: "stale", gatewayId: "galaxy", simId: "gone" }, [readyPhone], false, 1, noMmsPlan))
      .toBe("the selected phone/SIM is no longer reported; choose another route");
  });

  it("keeps the send refusal for an importing phone whose presence is offline or delayed, with live wording", () => {
    const importingOffline = settingUpPhone({ availability: "offline", settingUp: true });
    const importingDelayed = settingUpPhone({ availability: "delayed", settingUp: true, lastSeenAgeSeconds: 120 });
    const route = (gateway: GatewayView) => ({ kind: "route" as const, gateway, implicit: false });

    expect(routeProblem(route(importingOffline), [importingOffline], false, 1, noMmsPlan))
      .toBe("Pixel 8 is still setting up. It can send when that finishes.");
    expect(routeProblem(route(importingDelayed), [importingDelayed], false, 1, noMmsPlan))
      .toBe("Pixel 8 is still setting up. It can send when that finishes.");
    expect(headRoutePresentation(route(importingOffline), [importingOffline]).state).toBe("Not responding");
    expect(headRoutePresentation(route(importingDelayed), [importingDelayed]).state).toBe("Seen 2m ago");
  });

  it("presents the head pill as setting up with no SIM, for a direct or stale route", () => {
    const phone = settingUpPhone();
    expect(headRoutePresentation({ kind: "route", gateway: phone, implicit: false }, [phone]))
      .toEqual({ state: "Setting up", label: "Setting up" });
    expect(headRoutePresentation({ kind: "stale", gatewayId: "pixel", simId: "gone" }, [phone]))
      .toEqual({ state: "Setting up", label: "Setting up" });
  });
});

describe("setting-up placeholder is presentation only", () => {
  it("presents a stored pair to a vanished SIM as the setting-up phone without rewriting the pair", () => {
    const phone = settingUpPhone();
    const stored = { gatewayId: "pixel", simId: "sim-1" };
    const route = resolveStoredRoute(stored, [phone]);
    expect(route).toEqual({ kind: "route", gateway: phone, implicit: false });
    expect(settingUpGatewayFor(route, [phone])?.id).toBe("pixel");
    expect(stored).toEqual({ gatewayId: "pixel", simId: "sim-1" });
  });

  it("does not present a stored pair to a phone that is not setting up as setting up", () => {
    expect(resolveStoredRoute({ gatewayId: "galaxy", simId: "gone" }, [readyPhone]).kind).toBe("stale");
  });
});

describe("App phone that is setting up", () => {
  let snapshot: DesktopSnapshot;
  let stateListener: (() => void) | undefined;

  beforeEach(async () => {
    vi.restoreAllMocks();
    snapshot = { ...structuredClone(await fixtureBridge.load_state()), mode: "browser" };
    vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
      ...structuredClone(snapshot),
      activeConversationId: conversationId ?? snapshot.activeConversationId,
    }));
    vi.spyOn(bridge, "subscribe").mockImplementation((listener) => {
      stateListener = listener;
      return () => undefined;
    });
    vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
    vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
    vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
    vi.spyOn(bridge, "mark_seen").mockResolvedValue();
    vi.spyOn(bridge, "search_local").mockImplementation(fixtureBridge.search_local);
  });

  afterEach(() => {
    cleanup();
    stateListener = undefined;
    vi.restoreAllMocks();
  });

  it("blocks the composer with the setting-up reason and marks the route", async () => {
    snapshot = { ...snapshot, gateways: [settingUpPhone()] };
    render(<App hostKind="browser" />);

    await waitFor(() => expect(document.querySelector('[data-route-block="setting-up"]')).toHaveTextContent("Pixel 8 is still setting up. It can send when that finishes."));
    expect(document.getElementById("gateway-selector")).toHaveAttribute("data-route-reason", "setting-up");
  });

  it("announces the finished setup once, when the selected phone becomes sendable", async () => {
    snapshot = { ...snapshot, gateways: [settingUpPhone()] };
    render(<App hostKind="browser" />);
    await waitFor(() => expect(document.getElementById("gateway-selector")).toHaveAttribute("data-route-reason", "setting-up"));

    snapshot = { ...snapshot, gateways: [settingUpPhone({ simId: "sim-1", supportsSms: true, availability: "connected", settingUp: false })] };
    await act(async () => stateListener?.());
    await waitFor(() => expect(document.getElementById("gateway-presence-hint")).toHaveTextContent("Pixel 8 finished setting up. You can send now."));
    expect(document.getElementById("gateway-selector")).not.toHaveAttribute("data-route-reason");

    await act(async () => stateListener?.());
    expect(document.getElementById("gateway-presence-hint")).toHaveTextContent("Pixel 8 finished setting up. You can send now.");
  });

  it("shows a stored route to a setting-up phone as that phone, never as an unavailable route", async () => {
    const conversationId = snapshot.activeConversationId ?? "conv-aurora";
    snapshot = {
      ...snapshot,
      gateways: [settingUpPhone()],
      draft: { id: `draft-${conversationId}`, conversationId, text: "", recipientIds: [], attachmentIds: [], revision: "1", gatewayId: "pixel", simId: "sim-9" },
    };
    render(<App hostKind="browser" />);
    await waitFor(() => expect(document.getElementById("gateway-selector")).toBeInTheDocument());
    const selector = document.getElementById("gateway-selector")!;
    expect(selector).not.toHaveTextContent("Unavailable route");
    expect(selector).not.toHaveTextContent("sim-9");
    expect(selector).toHaveTextContent("Pixel 8");
    expect(selector).toHaveTextContent("Setting up");
  });

  it("keeps a stored sendable route when the setting-up phone is activated, and saves the stored pair", async () => {
    const conversationId = snapshot.activeConversationId ?? "conv-aurora";
    snapshot = {
      ...snapshot,
      gateways: [readyPhone, settingUpPhone()],
      draft: { id: `draft-${conversationId}`, conversationId, text: "", recipientIds: [], attachmentIds: [], revision: "1", gatewayId: "galaxy", simId: "sim-2" },
    };
    const save = vi.spyOn(bridge, "save_draft").mockImplementation(async (input) => ({
      id: `draft-${conversationId}`,
      conversationId,
      text: input.text,
      recipientIds: input.recipientIds,
      attachmentIds: input.attachmentIds,
      gatewayId: input.gatewayId,
      simId: input.simId,
      revision: "2",
    }));
    render(<App hostKind="browser" />);
    const combobox = await screen.findByRole("combobox", { name: "Phone" });
    await waitFor(() => expect(combobox).toHaveTextContent("Galaxy S24"));

    fireEvent.click(combobox);
    const blocked = screen.getByRole("option", { name: /Pixel 8/ });
    expect(blocked).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(blocked);
    expect(combobox).toHaveTextContent("Galaxy S24");
    expect(combobox).not.toHaveTextContent("Pixel 8");

    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "hello" } });
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ text: "hello", gatewayId: "galaxy", simId: "sim-2" })));
    expect(save.mock.calls.every(([input]) => input.simId !== "")).toBe(true);
  });

  it("renders the picker for a stored setting-up route as that phone's selected setting-up option", async () => {
    const conversationId = snapshot.activeConversationId ?? "conv-aurora";
    snapshot = {
      ...snapshot,
      gateways: [settingUpPhone(), readyPhone],
      draft: { id: `draft-${conversationId}`, conversationId, text: "", recipientIds: [], attachmentIds: [], revision: "1", gatewayId: "pixel", simId: "sim-9" },
    };
    render(<App hostKind="browser" />);
    await waitFor(() => expect(document.querySelector('[data-route-block="setting-up"]')).toBeInTheDocument());
    const trigger = screen.getByRole("combobox", { name: "Phone" });
    expect(trigger).toHaveTextContent("Pixel 8");
    expect(trigger).not.toHaveTextContent("Unavailable route");
    expect(trigger.querySelector('[data-route-badge="sim"]')).toBeNull();
    expect(trigger.querySelector('[data-route-badge="status"][data-gateway-availability="setting_up"]')).toHaveTextContent("Setting up");
  });

  it("announces the finished setup only when the phone becomes sendable, and clears it when it stops being sendable", async () => {
    snapshot = { ...snapshot, gateways: [settingUpPhone()] };
    render(<App hostKind="browser" />);
    await waitFor(() => expect(document.getElementById("gateway-selector")).toHaveAttribute("data-route-reason", "setting-up"));

    snapshot = { ...snapshot, gateways: [settingUpPhone({ simId: "sim-1", supportsSms: true, availability: "offline", settingUp: false })] };
    await act(async () => stateListener?.());
    await waitFor(() => expect(document.getElementById("gateway-presence-hint")).not.toHaveTextContent("finished setting up"));

    snapshot = { ...snapshot, gateways: [settingUpPhone({ simId: "sim-1", supportsSms: true, availability: "connected", settingUp: false })] };
    await act(async () => stateListener?.());
    await waitFor(() => expect(document.getElementById("gateway-presence-hint")).toHaveTextContent("Pixel 8 finished setting up. You can send now."));

    snapshot = { ...snapshot, gateways: [settingUpPhone({ simId: "sim-1", supportsSms: true, availability: "offline", settingUp: false })] };
    await act(async () => stateListener?.());
    await waitFor(() => expect(document.getElementById("gateway-presence-hint")).not.toHaveTextContent("finished setting up"));
  });

  it("does not ask an owner to add a phone while the only phone is setting up", () => {
    const base = { ...snapshot, deviceRole: "owner" as const, connection: { state: "connected" as const, origin: "https://push.example.com" }, encryption: { state: "unlocked" as const } };
    expect(ownerWithoutPhone({ ...base, gateways: [] })).toBe(true);
    expect(ownerWithoutPhone({ ...base, gateways: [settingUpPhone()] })).toBe(false);
  });
});
