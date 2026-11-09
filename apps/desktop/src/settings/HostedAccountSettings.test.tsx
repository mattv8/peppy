import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AttachmentStorageSettingsHost, HostedAccountSettings } from "./HostedAccountSettings";
import { bridge, type HostedAccountView } from "../bridge/bridge";

vi.mock("../bridge/bridge", { spy: true });

const account = (changes: Partial<HostedAccountView> = {}): HostedAccountView => ({
  available: true,
  providers: ["google"],
  signedIn: false,
  accountLabel: null,
  classification: null,
  entitlement: null,
  access: null,
  hasVault: true,
  resumable: false,
  ...changes,
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((complete, fail) => { resolve = complete; reject = fail; });
  return { promise, resolve, reject };
}

describe("HostedAccountSettings", () => {
  afterEach(cleanup);
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(bridge.hosted_account).mockResolvedValue(account());
    vi.mocked(bridge.hosted_sign_out).mockResolvedValue();
    vi.mocked(bridge.hosted_sign_in).mockResolvedValue(account({ signedIn: true }));
  });

  it("offers Peppy sign-out for a signed-in account without inventing a label", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    render(<HostedAccountSettings />);

    expect(await screen.findByRole("button", { name: "Sign out of Peppy" })).toBeEnabled();
    expect(screen.queryByText("Google")).not.toBeInTheDocument();
  });

  it("refreshes to the signed-out state after sign-out and focuses its action", async () => {
    vi.mocked(bridge.hosted_account)
      .mockResolvedValueOnce(account({ signedIn: true, accountLabel: "Ada" }))
      .mockResolvedValueOnce(account());
    render(<HostedAccountSettings />);

    const signOut = await screen.findByRole("button", { name: "Sign out of Peppy" });
    fireEvent.click(signOut);

    await waitFor(() => expect(bridge.hosted_sign_out).toHaveBeenCalledOnce());
    const signIn = await screen.findByRole("button", { name: "Continue with Google" });
    await waitFor(() => expect(signIn).toHaveFocus());
  });

  it("preserves sign-out focus through an earlier account render's queued effect", async () => {
    const initial = deferred<HostedAccountView>();
    const signOut = deferred<void>();
    const refresh = deferred<HostedAccountView>();
    vi.mocked(bridge.hosted_account)
      .mockReturnValueOnce(initial.promise)
      .mockReturnValueOnce(refresh.promise);
    vi.mocked(bridge.hosted_sign_out).mockReturnValueOnce(signOut.promise);

    // Click between the initial DOM commit and its passive effect, before React
    // can commit pending=true. That earlier effect must not consume action focus.
    const observer = new MutationObserver(() => {
      const button = document.getElementById("hosted-account-sign-out");
      if (!button) return;
      observer.disconnect();
      button.focus();
      fireEvent.click(button);
    });
    observer.observe(document.body, { childList: true, subtree: true });
    try {
      render(<HostedAccountSettings />);
      initial.resolve(account({ signedIn: true }));
      await waitFor(() => expect(screen.getByRole("button", { name: "Sign out of Peppy" })).toBeDisabled());

      await act(async () => signOut.resolve());
       expect(screen.queryByRole("status")).not.toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Continue with Google" })).not.toBeInTheDocument();

      await act(async () => refresh.resolve(account()));
      const signIn = screen.getByRole("button", { name: "Continue with Google" });
      expect(signIn).toBeEnabled();
      expect(signIn).toHaveFocus();
    } finally {
      observer.disconnect();
    }
  });

  it("keeps the signed-in state when sign-out fails", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_sign_out).mockRejectedValue(new Error("write failed"));
    render(<HostedAccountSettings />);

    fireEvent.click(await screen.findByRole("button", { name: "Sign out of Peppy" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Sign-out was not completed");
    expect(screen.getByRole("button", { name: "Sign out of Peppy" })).toBeEnabled();
  });

  it("shows availability retry failure after a successful sign-out refresh fails", async () => {
    vi.mocked(bridge.hosted_account)
      .mockResolvedValueOnce(account({ signedIn: true }))
      .mockRejectedValueOnce(new Error("offline"));
    render(<HostedAccountSettings />);

    fireEvent.click(await screen.findByRole("button", { name: "Sign out of Peppy" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load account status");
    expect(screen.queryByRole("button", { name: "Sign out of Peppy" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
    expect(document.getElementById("hosted-account-unavailable")).toBeNull();
  });

  it("does not let a stale account read restore the signed-in state after sign-out", async () => {
    const staleRead = deferred<HostedAccountView>();
    vi.mocked(bridge.hosted_account)
      .mockReturnValueOnce(staleRead.promise)
      .mockResolvedValueOnce(account({ signedIn: true }))
      .mockResolvedValueOnce(account());
    render(<StrictMode><HostedAccountSettings /></StrictMode>);

    expect(await screen.findByRole("button", { name: "Sign out of Peppy" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Sign out of Peppy" }));
    expect(await screen.findByRole("button", { name: "Continue with Google" })).toBeInTheDocument();
    await act(async () => staleRead.resolve(account({ signedIn: true })));
    expect(screen.getByRole("button", { name: "Continue with Google" })).toBeInTheDocument();
  });

  it("restores focus to sign-in after a delayed sign-out refresh completes", async () => {
    const signOut = deferred<void>();
    const refresh = deferred<HostedAccountView>();
    vi.mocked(bridge.hosted_account)
      .mockResolvedValueOnce(account({ signedIn: true, accountLabel: "Ada" }))
      .mockReturnValueOnce(refresh.promise);
    vi.mocked(bridge.hosted_sign_out).mockReturnValueOnce(signOut.promise);
    render(<HostedAccountSettings />);

    fireEvent.click(await screen.findByRole("button", { name: "Sign out of Peppy" }));
    await act(async () => { signOut.resolve(); });
    await waitFor(() => expect(bridge.hosted_account).toHaveBeenCalledTimes(2));

    // Browsers resolve state updates made in microtasks under the originating discrete
    // event (window.event), so the refresh commit can land before the action's finally clears pending.
    Object.defineProperty(window, "event", { configurable: true, value: { type: "click" } });
    try {
      refresh.resolve(account());
      await Promise.resolve();
      await Promise.resolve();
    } finally {
      Object.defineProperty(window, "event", { configurable: true, value: undefined });
    }

    const signIn = await screen.findByRole("button", { name: "Continue with Google" });
    await waitFor(() => expect(signIn).toBeEnabled());
    expect(signIn).toHaveFocus();
  });

  it("does not move focus after the initial account check", async () => {
    render(<><button autoFocus>Settings navigation</button><HostedAccountSettings /></>);

    await screen.findByRole("button", { name: "Continue with Google" });
    expect(screen.getByRole("button", { name: "Settings navigation" })).toHaveFocus();
  });

  it("disables duplicate actions while sign-in is pending", async () => {
    const signIn = deferred<HostedAccountView>();
    vi.mocked(bridge.hosted_sign_in).mockReturnValue(signIn.promise);
    render(<HostedAccountSettings />);

    const action = await screen.findByRole("button", { name: "Continue with Google" });
    fireEvent.click(action);

    expect(action).toBeDisabled();
    expect(action).toHaveAttribute("aria-busy", "true");
    expect(bridge.hosted_sign_in).toHaveBeenCalledWith("google");
  });

  it("shows sign-in unavailable when the server advertises no providers", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ providers: [] }));
    render(<HostedAccountSettings />);

    expect(await screen.findByText("Sign-in is not available.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /continue with google/i })).not.toBeInTheDocument();
  });
});


describe("AttachmentStorageSettingsHost", () => {
  afterEach(cleanup);
  beforeEach(() => {
    vi.mocked(bridge.attachment_retention_status!).mockReset();
    vi.mocked(bridge.attachment_retention_status!).mockResolvedValue({ kind: "unsupported" });
  });

  const status = (overrides: Record<string, unknown> = {}): import("../bridge/bridge").AttachmentRetentionStatusResult => ({
    kind: "available",
    scopeId: "scope-one",
    status: {
      usedBytes: 20_000_000_000,
      limitBytes: 75_000_000_000,
      baseBytes: 50_000_000_000,
      packBytes: 25_000_000_000,
      protectedBytes: 0,
      removedBytes: 0,
      cleanupState: "disabled",
      cleanupUnavailableReason: "cleanup_disabled",
      ...overrides,
    },
  } as unknown as import("../bridge/bridge").AttachmentRetentionStatusResult);

  it("preserves known usage as stale when an explicit refresh is temporarily unavailable", async () => {
    vi.mocked(bridge.attachment_retention_status!)
      .mockResolvedValueOnce(status())
      .mockResolvedValueOnce({ kind: "temporarily_unavailable" });
    render(<AttachmentStorageSettingsHost />);
    await screen.findByText("20 GB");

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Last known storage usage")).toBeInTheDocument();
    expect(screen.getByText("20 GB")).toBeInTheDocument();
  });

  it("keeps loading billing controls after a retention-status failure", async () => {
    vi.mocked(bridge.attachment_retention_status!).mockRejectedValueOnce(new Error("offline"));
    vi.mocked(bridge.purchase_options).mockResolvedValue({ appStore: false, website: true });
    render(<AttachmentStorageSettingsHost onManageStorage={vi.fn()} />);

    expect(await screen.findByText(/Storage policy status is unavailable/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Manage storage on the website" })).toBeInTheDocument();
  });

  it("clears a loaded device-a status before device-b becomes unavailable", async () => {
    const deviceA = deferred<import("../bridge/bridge").AttachmentRetentionStatusResult>();
    const deviceB = deferred<import("../bridge/bridge").AttachmentRetentionStatusResult>();
    vi.mocked(bridge.attachment_retention_status!)
      .mockReturnValueOnce(deviceA.promise)
      .mockReturnValueOnce(deviceB.promise);
    const { rerender } = render(<AttachmentStorageSettingsHost sessionScopeId="device-a" />);
    await act(async () => deviceA.resolve(status({ usedBytes: 20_000_000_000 })));
    expect(await screen.findByText("20 GB")).toBeInTheDocument();

    rerender(<AttachmentStorageSettingsHost sessionScopeId="device-b" />);
    expect(screen.queryByText("20 GB")).not.toBeInTheDocument();

    await act(async () => deviceB.resolve({ kind: "temporarily_unavailable" }));
    expect(await screen.findByText(/Storage policy status is unavailable/)).toBeInTheDocument();
    expect(screen.queryByText("20 GB")).not.toBeInTheDocument();
  });

  it("keeps the billing handoff after an unsupported retention response", async () => {
    vi.mocked(bridge.attachment_retention_status!).mockResolvedValue({ kind: "unsupported" });
    render(<AttachmentStorageSettingsHost onManageStorage={vi.fn()} />);

    await waitFor(() => expect(document.getElementById("attachment-storage-loading")).not.toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Manage storage on the website" })).toBeInTheDocument();
    expect(screen.queryByText("When automatic cleanup is enabled")).not.toBeInTheDocument();

    cleanup();
    render(<AttachmentStorageSettingsHost manageStorageUrl="https://account.example.test/billing" />);
    await waitFor(() => expect(document.getElementById("attachment-storage-loading")).not.toBeInTheDocument());
    expect(screen.getByRole("link", { name: /manage storage on the website/i })).toHaveAttribute("href", "https://account.example.test/billing");
  });

  it("does not announce an unavailable status while the initial storage read is pending", () => {
    vi.mocked(bridge.attachment_retention_status!).mockReturnValue(new Promise(() => undefined));
    render(<AttachmentStorageSettingsHost />);

    expect(screen.getByRole("status")).toHaveTextContent("Checking hosted attachment storage");
    expect(screen.queryByText("Storage policy status is unavailable")).not.toBeInTheDocument();
  });


});
