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
    vi.mocked(bridge.billing_summary).mockResolvedValue({ provider: "none", manageableIn: "none", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false });
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValue({ executeAfter: null, cancellable: false });
    vi.mocked(bridge.hosted_cancel_account_deletion).mockResolvedValue();
  });

  it("allows canceling a scheduled deletion and returns focus to delete", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_deletion_status)
      .mockResolvedValueOnce({ executeAfter: "2026-11-26T18:00:00Z", cancellable: true })
      .mockResolvedValue({ executeAfter: null, cancellable: false });
    const onDeletionCanceled = vi.fn();
    render(<HostedAccountSettings onDeletionCanceled={onDeletionCanceled} />);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel deletion" }));
    await waitFor(() => expect(onDeletionCanceled).toHaveBeenCalledOnce());
    await waitFor(() => expect(document.getElementById("hosted-account-delete-open")).toHaveFocus());
  });

  it("shows processing when cancellation loses the race", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_deletion_status)
      .mockResolvedValueOnce({ executeAfter: "2026-11-26T18:00:00Z", cancellable: true })
      .mockResolvedValue({ executeAfter: "2026-11-26T18:00:00Z", cancellable: false });
    vi.mocked(bridge.hosted_cancel_account_deletion).mockRejectedValue({ code: "deletion-already-started" });
    render(<HostedAccountSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel deletion" }));
    await waitFor(() => expect(document.getElementById("hosted-account-delete-cancel-error")).toHaveTextContent("Deletion has already started"));
    expect(document.getElementById("hosted-account-delete-status")).toHaveFocus();
  });

  it("returns to the delete action when nothing is scheduled", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValueOnce({ executeAfter: "2026-11-26T18:00:00Z", cancellable: true }).mockResolvedValue({ executeAfter: null, cancellable: false });
    vi.mocked(bridge.hosted_cancel_account_deletion).mockRejectedValue({ code: "deletion-not-scheduled" });
    render(<HostedAccountSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel deletion" }));
    await waitFor(() => expect(document.getElementById("hosted-account-delete-open")).toHaveFocus());
    expect(document.getElementById("hosted-account-delete-announcer")).toHaveTextContent("Nothing is scheduled");
  });

  it("allows retry after a cancellation failure", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValueOnce({ executeAfter: "2026-11-26T18:00:00Z", cancellable: true }).mockResolvedValue({ executeAfter: null, cancellable: false });
    vi.mocked(bridge.hosted_cancel_account_deletion).mockRejectedValueOnce(new Error("offline")).mockResolvedValue();
    render(<HostedAccountSettings />);
    const cancel = await screen.findByRole("button", { name: "Cancel deletion" });
    cancel.focus();
    fireEvent.click(cancel);
    await waitFor(() => expect(document.getElementById("hosted-account-delete-cancel-error")).toHaveTextContent("Nothing was changed"));
    expect(cancel).toBeEnabled();
    expect(cancel).toHaveFocus();
    fireEvent.click(cancel);
    await waitFor(() => expect(bridge.hosted_cancel_account_deletion).toHaveBeenCalledTimes(2));
  });

  it("sends only one cancellation while it is pending", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValue({ executeAfter: "2026-11-26T18:00:00Z", cancellable: true });
    const pending = deferred<void>();
    vi.mocked(bridge.hosted_cancel_account_deletion).mockReturnValue(pending.promise);
    render(<HostedAccountSettings />);
    const cancel = await screen.findByRole("button", { name: "Cancel deletion" });
    fireEvent.click(cancel);
    fireEvent.click(cancel);
    expect(cancel).toHaveAttribute("aria-disabled", "true");
    expect(cancel).toHaveAttribute("aria-busy", "true");
    expect(bridge.hosted_cancel_account_deletion).toHaveBeenCalledOnce();
    await act(async () => pending.resolve());
  });

  it.each(["stripe", "apple", "google_play"] as const)("shows the %s subscription note while scheduled", async provider => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.billing_summary).mockResolvedValue({ provider, manageableIn: "none", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false });
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValue({ executeAfter: "2026-11-26T18:00:00Z", cancellable: true });
    render(<HostedAccountSettings />);
    await screen.findByRole("button", { name: "Cancel deletion" });
    expect(document.getElementById("hosted-account-delete-subscription-note")).toHaveTextContent(provider === "stripe" ? "we'll try to turn renewal back on" : provider === "apple" ? "App Store" : "Google Play");
  });

  it.each([false, true])("uses billing pending=%s when status cannot be fetched", async pending => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.billing_summary).mockResolvedValue({ provider: "none", manageableIn: "none", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: pending });
    vi.mocked(bridge.hosted_deletion_status).mockRejectedValue(new Error("offline"));
    render(<HostedAccountSettings />);
    expect(await screen.findByRole("button", { name: pending ? "Cancel deletion" : "Delete account" })).toBeEnabled();
    expect(document.getElementById("hosted-account-error")).not.toBeInTheDocument();
  });

  it("keeps the known scheduled state when a focus refresh fails", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValueOnce({ executeAfter: "2026-11-26T18:00:00Z", cancellable: true }).mockRejectedValue(new Error("offline"));
    render(<HostedAccountSettings />);
    await screen.findByRole("button", { name: "Cancel deletion" });
    fireEvent.focus(window);
    await waitFor(() => expect(bridge.hosted_deletion_status).toHaveBeenCalledTimes(2));
    expect(screen.getByRole("button", { name: "Cancel deletion" })).toBeInTheDocument();
  });

  it("shows the local date and time of a scheduled deletion", async () => {
    const executeAfter = "2026-11-26T18:00:00Z";
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValue({ executeAfter, cancellable: true });
    render(<HostedAccountSettings />);
    await screen.findByRole("button", { name: "Cancel deletion" });
    const expected = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(executeAfter));
    expect(document.getElementById("hosted-account-delete-status")).toHaveTextContent(expected);
  });

  it("clears deletion status when the session scope changes", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValueOnce({ executeAfter: "2026-11-26T18:00:00Z", cancellable: true }).mockRejectedValue(new Error("offline"));
    const view = render(<HostedAccountSettings sessionScopeId="first" />);
    await screen.findByRole("button", { name: "Cancel deletion" });
    view.rerender(<HostedAccountSettings sessionScopeId="second" />);
    await screen.findByRole("button", { name: "Delete account" });
    expect(screen.queryByRole("button", { name: "Cancel deletion" })).not.toBeInTheDocument();
  });

  it("shows a scheduled row immediately after a pending deletion", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.billing_summary).mockResolvedValue({ provider: "none", manageableIn: "none", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false });
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValueOnce({ executeAfter: null, cancellable: false }).mockResolvedValue({ executeAfter: "2026-11-26T18:00:00Z", cancellable: true });
    vi.mocked(bridge.hosted_delete_account).mockResolvedValue("pending");
    render(<HostedAccountSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Delete account" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "I understand my account will be deleted in 7 days" }));
    fireEvent.click(document.getElementById("account-delete-submit")!);
    await screen.findByRole("button", { name: "Cancel deletion" });
    expect(document.getElementById("hosted-account-delete")).toHaveAttribute("data-deletion-phase", "scheduled");
  });

  it("shows processing after a 202 when status is not cancellable", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValueOnce({ executeAfter: null, cancellable: false }).mockResolvedValue({ executeAfter: "2026-11-26T18:00:00Z", cancellable: false });
    vi.mocked(bridge.hosted_delete_account).mockResolvedValue("pending");
    render(<HostedAccountSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Delete account" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "I understand my account will be deleted in 7 days" }));
    fireEvent.click(document.getElementById("account-delete-submit")!);
    await waitFor(() => expect(document.getElementById("hosted-account-delete")).toHaveAttribute("data-deletion-phase", "processing"));
    expect(screen.queryByRole("button", { name: "Cancel deletion" })).not.toBeInTheDocument();
  });

  it("offers Peppy sign-out for a signed-in account without inventing a label", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true }));
    render(<HostedAccountSettings />);

    expect(await screen.findByRole("button", { name: "Sign out of Peppy" })).toBeEnabled();
    expect(screen.queryByText("Google")).not.toBeInTheDocument();
  });

  it("groups signed-in account actions into compact account rows", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true, accountLabel: "Ada" }));
    render(<HostedAccountSettings />);

    expect(await screen.findByRole("button", { name: "Sign out of Peppy" })).toBeInTheDocument();
    expect(document.getElementById("hosted-account-sign-out-row")).toContainElement(document.getElementById("hosted-account-sign-out"));
    expect(document.getElementById("hosted-account-delete")).toContainElement(document.getElementById("hosted-account-delete-open"));
  });

  it("places the feedback link after storage and before account deletion when signed in", async () => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true, accountLabel: "Ada" }));
    render(<HostedAccountSettings />);

    const feedback = await waitFor(() => {
      const element = document.getElementById("settings-feedback");
      expect(element).toBeInTheDocument();
      return element!;
    });
    expect(feedback).toContainElement(screen.getByRole("button", { name: /send feedback/i }));
    const storage = document.getElementById("attachment-storage");
    const deletion = document.getElementById("hosted-account-delete");
    expect(storage?.compareDocumentPosition(feedback)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    expect(feedback.compareDocumentPosition(deletion!)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  });

  it.each([
    ["scheduled", { executeAfter: "2026-11-26T18:00:00Z", cancellable: true }],
    ["processing", { executeAfter: "2026-11-26T18:00:00Z", cancellable: false }],
  ] as const)("hides the feedback link while account deletion is %s", async (phase, status) => {
    vi.mocked(bridge.hosted_account).mockResolvedValue(account({ signedIn: true, accountLabel: "Ada" }));
    vi.mocked(bridge.hosted_deletion_status).mockResolvedValue(status);
    render(<HostedAccountSettings />);

    await waitFor(() => expect(document.getElementById("hosted-account-delete")).toHaveAttribute("data-deletion-phase", phase));
    expect(document.getElementById("settings-feedback")).not.toBeInTheDocument();
  });

  it("omits the feedback link when signed out", async () => {
    render(<HostedAccountSettings />);

    await waitFor(() => expect(document.getElementById("hosted-account-sign-in-options")).toBeInTheDocument());
    expect(document.getElementById("settings-feedback")).not.toBeInTheDocument();
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
       expect(document.getElementById("hosted-account-loading")).not.toBeInTheDocument();
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
    await screen.findByText(/20 GB of/);

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Last known storage usage")).toBeInTheDocument();
    expect(screen.getByText(/20 GB of/)).toBeInTheDocument();
  });

  it("keeps loading billing controls after a retention-status failure", async () => {
    vi.mocked(bridge.attachment_retention_status!).mockRejectedValueOnce(new Error("offline"));
    vi.mocked(bridge.purchase_options).mockResolvedValue({ appStore: false, website: true });
    render(<AttachmentStorageSettingsHost onManageStorage={vi.fn()} />);

    expect(await screen.findByText(/Storage policy status is unavailable/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Manage storage on the website" })).toBeInTheDocument();
  });

  it("hides website management for Google Play billing", async () => {
    vi.mocked(bridge.attachment_retention_status!).mockResolvedValue(status());
    vi.mocked(bridge.billing_summary).mockResolvedValueOnce({ provider: "google_play", manageableIn: "google_play", planActive: true, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false });
    vi.mocked(bridge.purchase_options).mockResolvedValueOnce({ appStore: false, website: true });
    render(<AttachmentStorageSettingsHost onManageStorage={vi.fn()} />);

    expect(await screen.findByText("Extra storage isn't available with a Google Play subscription.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Manage storage on the website" })).not.toBeInTheDocument();
  });

  it("clears a loaded device-a status before device-b becomes unavailable", async () => {
    const deviceA = deferred<import("../bridge/bridge").AttachmentRetentionStatusResult>();
    const deviceB = deferred<import("../bridge/bridge").AttachmentRetentionStatusResult>();
    vi.mocked(bridge.attachment_retention_status!)
      .mockReturnValueOnce(deviceA.promise)
      .mockReturnValueOnce(deviceB.promise);
    const { rerender } = render(<AttachmentStorageSettingsHost sessionScopeId="device-a" />);
    await act(async () => deviceA.resolve(status({ usedBytes: 20_000_000_000 })));
    expect(await screen.findByText(/20 GB of/)).toBeInTheDocument();

    rerender(<AttachmentStorageSettingsHost sessionScopeId="device-b" />);
    expect(screen.queryByText(/20 GB of/)).not.toBeInTheDocument();

    await act(async () => deviceB.resolve({ kind: "temporarily_unavailable" }));
    expect(await screen.findByText(/Storage policy status is unavailable/)).toBeInTheDocument();
    expect(screen.queryByText(/20 GB of/)).not.toBeInTheDocument();
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
