import "@testing-library/jest-dom/vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge";

const signedOut = {
  available: false,
  signedIn: false,
  accountLabel: null,
  classification: null,
  entitlement: null,
  access: null,
  hasVault: false,
  resumable: false,
} as const;

const injectSnapshot = async (
  overrides: Partial<DesktopSnapshot>,
  landing = false,
) => {
  const base = await fixtureBridge.load_state();
  vi.spyOn(bridge, "load_state").mockResolvedValue({
    ...base,
    activeConversationId: undefined,
    ...overrides,
  });
  if (!landing) return;

  vi.spyOn(bridge, "hosted_account").mockResolvedValue(signedOut);
  vi.spyOn(bridge, "join_start").mockResolvedValue({
    state: "waiting",
    qrPayload: '{"join":true}',
    expiresInSeconds: 300,
  });
  vi.spyOn(bridge, "join_status").mockResolvedValue({
    state: "waiting",
    qrPayload: '{"join":true}',
    expiresInSeconds: 300,
  });
  vi.spyOn(bridge, "join_cancel").mockResolvedValue();
};

describe("setup progressive disclosure", () => {
  beforeEach(() => {
    const values = new Map<string, string>();
    Object.defineProperty(window, "localStorage", {
      configurable: true,
      value: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem: (key: string) => values.delete(key),
        clear: () => values.clear(),
      },
    });
    vi.restoreAllMocks();
  });

  afterEach(() => cleanup());

  it("1 hides unusable setup actions from native self-hosted landing Advanced", async () => {
    localStorage.setItem("peppy.setup.mode", "self-hosted");
    await injectSnapshot(
      {
        mode: "native",
        connection: { state: "offline", errorCode: "server-required" },
        encryption: { state: "locked" },
        credentialExportAvailable: true,
      },
      true,
    );

    render(<App />);
    fireEvent.click(await screen.findByText("Advanced"));

    const advanced = document.getElementById("self-hosted-advanced");
    expect(advanced).toBeInTheDocument();
    expect(
      await within(advanced!).findByRole("button", {
        name: "Import credentials natively",
      }),
    ).toBeEnabled();
    expect(
      within(advanced!).queryByRole("button", { name: "Configure server" }),
    ).not.toBeInTheDocument();
    expect(
      within(advanced!).queryByRole("button", { name: "Unlock sync natively" }),
    ).not.toBeInTheDocument();
    expect(
      within(advanced!).queryByRole("button", { name: "Export credentials" }),
    ).not.toBeInTheDocument();
  });

  it("2 hides unavailable recovery actions in native Hosted mode", async () => {
    await injectSnapshot({
      mode: "native",
      connection: { state: "offline", errorCode: "credentials-required" },
      encryption: { state: "locked" },
      credentialExportAvailable: true,
    });

    render(<App />);

    const onboarding = await screen.findByRole("region", {
      name: "Set up Peppy",
    });
    expect(
      await within(onboarding).findByRole("button", {
        name: "Import credentials natively",
      }),
    ).toBeEnabled();
    expect(
      within(onboarding).queryByRole("button", { name: "Configure server" }),
    ).not.toBeInTheDocument();
    expect(
      within(onboarding).queryByRole("button", { name: "Unlock sync natively" }),
    ).not.toBeInTheDocument();
    expect(
      within(onboarding).queryByRole("button", { name: "Export credentials" }),
    ).not.toBeInTheDocument();
  });

  it("3 shows Configure server in native self-hosted recovery", async () => {
    localStorage.setItem("peppy.setup.mode", "self-hosted");
    await injectSnapshot({
      mode: "native",
      connection: { state: "offline", errorCode: "credentials-required" },
      encryption: { state: "locked" },
      credentialExportAvailable: true,
    });

    render(<App />);

    const onboarding = await screen.findByRole("region", {
      name: "Set up Peppy",
    });
    expect(
      within(onboarding).getByRole("button", { name: "Configure server" }),
    ).toBeEnabled();
  });

  it("4 enables native recovery unlock and export when a credential exists", async () => {
    await injectSnapshot({
      mode: "native",
      connection: { state: "offline" },
      encryption: { state: "locked" },
      credentialExportAvailable: true,
    });

    render(<App />);

    const onboarding = await screen.findByRole("region", {
      name: "Set up Peppy",
    });
    expect(
      within(onboarding).getByRole("button", { name: "Unlock sync natively" }),
    ).toBeEnabled();
    expect(
      within(onboarding).getByRole("button", { name: "Export credentials" }),
    ).toBeEnabled();
  });

  it("5 hides browser preview export in landing Advanced and Settings", async () => {
    await injectSnapshot(
      {
        mode: "browser",
        connection: { state: "offline" },
        encryption: { state: "preview" },
        credentialExportAvailable: true,
      },
      true,
    );

    render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    fireEvent.click(await screen.findByText("Advanced"));

    const onboarding = document.getElementById("onboarding-view");
    expect(onboarding).toHaveAttribute("data-compact", "true");
    expect(
      await within(onboarding!).findByRole("button", { name: "Import credentials" }),
    ).toBeEnabled();
    expect(
      within(onboarding!).queryByRole("button", { name: "Export credentials" }),
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const settings = await screen.findByRole("region", { name: "Settings" });
    expect(
      await within(settings).findByRole("button", { name: "Import credentials" }),
    ).toBeEnabled();
    expect(
      within(settings).queryByRole("button", { name: "Export credentials" }),
    ).not.toBeInTheDocument();
    expect(document.getElementById("settings-credential-export")).not.toBeInTheDocument();
  });

  it("6 keeps locked browser credential export visible with its disabled reason", async () => {
    await injectSnapshot({
      mode: "browser",
      connection: { state: "offline" },
      encryption: { state: "locked" },
      credentialExportAvailable: true,
    });

    render(<App hostKind="browser" fixedOrigin="https://community.example" />);

    const onboarding = await screen.findByRole("region", {
      name: "Set up Peppy",
    });
    const exportButton = within(onboarding).getByRole("button", {
      name: "Export credentials",
    });
    expect(exportButton).toBeDisabled();
    expect(exportButton).toHaveAttribute(
      "aria-describedby",
      "setup-credential-export-reason",
    );
    expect(document.getElementById("setup-credential-export-reason")).toBeInTheDocument();
  });

  it("7 preserves native revoked credential export in Settings without unlock", async () => {
    await injectSnapshot({
      mode: "native",
      connection: { state: "error", errorCode: "revoked" },
      encryption: { state: "locked" },
      credentialExportAvailable: true,
    });

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));

    const settings = await screen.findByRole("region", { name: "Settings" });
    const exportSection = document.getElementById("settings-credential-export");
    expect(exportSection).toBeInTheDocument();
    expect(
      within(exportSection!).getByRole("button", { name: "Export credentials" }),
    ).toBeEnabled();
    expect(
      within(settings).queryByRole("button", { name: "Unlock sync natively" }),
    ).not.toBeInTheDocument();
  });
});
