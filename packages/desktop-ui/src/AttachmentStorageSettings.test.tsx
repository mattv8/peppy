import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AttachmentRetentionStatusResult } from "./bridge-contracts";
import { AttachmentStorageSettings, attachmentStorageRemovalText, type AttachmentStorageCopy } from "./AttachmentStorageSettings";

afterEach(cleanup);

const copy = {
  attachment_storage_section_title: "Hosted attachment storage", attachment_storage_allowance: "Included storage", attachment_storage_used_of_limit: "%1$s of %2$s used", attachment_storage_included_plus_addon: "%1$s included + %2$s add-on storage", attachment_storage_billing_handoff: "Manage storage online.", attachment_storage_disabled: "Automatic cleanup is not enabled", attachment_storage_read_only: "Your subscription is read-only", attachment_storage_unavailable: "Automatic cleanup status is unavailable.", attachment_storage_reload_cta: "Refresh", attachment_storage_manage_cta: "Manage storage on the website", attachment_storage_cleanup_pending: "Storage cleanup is pending.", attachment_storage_removed_title: "Hosted media removed", attachment_storage_removed: "This attachment was removed from hosted storage to make room. The message remains.", attachment_storage_removed_local_copy: "Hosted copy removed. Available on this device.", attachment_storage_stale_usage: "Last known storage usage", attachment_storage_billing_pending: "Your storage change is still processing", attachment_storage_billing_unavailable: "Storage billing is unavailable", attachment_storage_billing_open_failed: "Could not open billing", attachment_storage_device_update_required: "Automatic cleanup is waiting for compatible, ready devices", attachment_storage_loading: "Loading storage", attachment_storage_info_label: "About storage cleanup", attachment_storage_info_title: "How storage cleanup works", attachment_storage_info_body_cleanup: "When storage is full, Peppy may remove the oldest eligible hosted message media to make room for new uploads. Messages, phone originals, and downloaded copies are not actively deleted by cleanup; normal device cache limits still apply.", attachment_storage_info_body_protected: "Attachments stored before this policy, and other protected attachments, are not removed automatically.", attachment_storage_info_body_purchase: "More storage can reduce future removals. It cannot restore files already removed.", attachment_storage_info_dismiss: "Got it", attachment_storage_near_full: "Hosted attachment storage is almost full.", attachment_storage_full_cleanup: "Storage is full. New uploads may remove the oldest eligible hosted message media. Messages remain.", attachment_storage_learn_more: "Learn more",
} as AttachmentStorageCopy;

const available = (overrides: Record<string, unknown> = {}): AttachmentRetentionStatusResult => ({ kind: "available", scopeId: "session-one", status: { usedBytes: 20, limitBytes: 100, baseBytes: 50, packBytes: 50, protectedBytes: 4, removedBytes: 0, cleanupState: "available", cleanupUnavailableReason: null, ...overrides } } as unknown as AttachmentRetentionStatusResult);

describe("AttachmentStorageSettings", () => {
  it("opens static information without a request and returns focus after dismissal", () => {
    const refresh = vi.fn();
    render(<AttachmentStorageSettings result={available()} copy={copy} onRefresh={refresh} />);
    const opener = screen.getByRole("button", { name: "About storage cleanup" });
    fireEvent.click(opener);
    expect(screen.getByRole("dialog", { name: "How storage cleanup works" })).toBeInTheDocument();
    expect(screen.getByText(copy.attachment_storage_info_body_cleanup)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Got it" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(opener).toHaveFocus();
    expect(refresh).not.toHaveBeenCalled();
  });

  it("closes information when the result scope changes", () => {
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={copy} />);
    fireEvent.click(screen.getByRole("button", { name: "About storage cleanup" }));
    const nextScope = available() as Extract<AttachmentRetentionStatusResult, { kind: "available" }>;
    rerender(<AttachmentStorageSettings result={{ ...nextScope, scopeId: "session-two" }} copy={copy} />);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it.each([[20_000_000_000, "normal", "20 GB"], [90_000_000_000, "near-full", "90 GB"], [100_000_000_000, "full", "100 GB"]])("renders the usage meter and capacity bar at %i bytes", (usedBytes, level, usedText) => {
    render(<AttachmentStorageSettings result={available({ usedBytes, limitBytes: 100_000_000_000 })} copy={copy} />);
    expect(document.getElementById("attachment-storage-meter")).toHaveTextContent(`${usedText} of 100 GB used`);
    expect(document.getElementById("attachment-storage-bar")).toHaveAttribute("data-capacity", level);
  });

  it("suppresses predictions for stale or invalid capacity and does not imply cleanup for unknown state", () => {
    const { rerender } = render(<AttachmentStorageSettings result={available({ usedBytes: 100 })} copy={copy} stale />);
    expect(document.getElementById("attachment-storage-bar")).toHaveAttribute("data-capacity", "stale");
    expect(document.getElementById("attachment-storage-capacity-notice")).toBeNull();
    rerender(<AttachmentStorageSettings result={available({ usedBytes: 100, limitBytes: 0 })} copy={copy} />);
    expect(document.getElementById("attachment-storage-bar")).toBeNull();
    expect(document.getElementById("attachment-storage-capacity-notice")).toBeNull();
    rerender(<AttachmentStorageSettings result={available({ usedBytes: 100, cleanupState: "blocked", cleanupUnavailableReason: "unknown_reason" })} copy={copy} />);
    expect(screen.getByText(copy.attachment_storage_unavailable)).toBeInTheDocument();
    expect(screen.queryByText(copy.attachment_storage_full_cleanup)).not.toBeInTheDocument();
  });

  it("does not render a stale bar for invalid capacity and clamps over-limit usage", () => {
    const { rerender } = render(<AttachmentStorageSettings result={available({ usedBytes: 0, limitBytes: 0 })} copy={copy} stale />);
    expect(document.getElementById("attachment-storage-bar")).toBeNull();

    rerender(<AttachmentStorageSettings result={available({ usedBytes: 200, limitBytes: 100 })} copy={copy} />);
    expect(document.querySelector("#attachment-storage-bar > span")).toHaveStyle({ width: "100%" });
  });

  it("keeps blocked full states conservative and preserves their causes", () => {
    const { rerender } = render(<AttachmentStorageSettings result={available({ usedBytes: 100, cleanupState: "disabled", cleanupUnavailableReason: "cleanup_disabled" })} copy={copy} />);
    expect(screen.getByText(copy.attachment_storage_disabled)).toBeInTheDocument();
    expect(screen.queryByText(copy.attachment_storage_full_cleanup)).not.toBeInTheDocument();

    rerender(<AttachmentStorageSettings result={available({ usedBytes: 100, cleanupState: "blocked", cleanupUnavailableReason: "read_only" })} copy={copy} />);
    expect(screen.getByText(copy.attachment_storage_read_only)).toBeInTheDocument();
    expect(screen.queryByText(copy.attachment_storage_full_cleanup)).not.toBeInTheDocument();

    rerender(<AttachmentStorageSettings result={available({ usedBytes: 100, cleanupState: "blocked", cleanupUnavailableReason: "unknown_reason" })} copy={copy} />);
    expect(screen.getByText(copy.attachment_storage_unavailable)).toBeInTheDocument();
    expect(screen.queryByText(copy.attachment_storage_full_cleanup)).not.toBeInTheDocument();
  });

  it("keeps Learn more available for a blocked near-full warning and returns focus there", () => {
    render(<AttachmentStorageSettings result={available({ usedBytes: 90, cleanupState: "blocked", cleanupUnavailableReason: "read_only" })} copy={copy} />);
    const learnMore = screen.getByRole("button", { name: copy.attachment_storage_learn_more });
    expect(screen.getByText(copy.attachment_storage_read_only)).toBeInTheDocument();
    fireEvent.click(learnMore);
    fireEvent.click(screen.getByRole("button", { name: copy.attachment_storage_info_dismiss }));
    expect(learnMore).toHaveFocus();
  });

  it("preserves management and blocker state behavior", () => {
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={copy} onManageStorage={vi.fn()} manageStorageError />);
    expect(screen.getByRole("alert")).toHaveTextContent(copy.attachment_storage_billing_open_failed);

    rerender(<AttachmentStorageSettings result={available({ cleanupState: "blocked", cleanupUnavailableReason: "billing_mutation_pending" })} copy={copy} />);
    expect(document.getElementById("attachment-storage-policy-state")).toHaveTextContent(copy.attachment_storage_billing_pending);

    rerender(<AttachmentStorageSettings result={available({ cleanupState: "blocked", cleanupUnavailableReason: "device_not_ready" })} copy={copy} />);
    expect(document.getElementById("attachment-storage-policy-state")).toHaveTextContent(copy.attachment_storage_device_update_required);
  });

  it("keeps browser management available when storage is unsupported", () => {
    render(<AttachmentStorageSettings result={{ kind: "unsupported" }} copy={copy} manageStorageUrl="https://account.example.test/billing" />);
    expect(screen.getByRole("link", { name: /manage storage on the website/i })).toHaveAttribute("href", "https://account.example.test/billing");
  });

  it("keeps known usage visible while stale and permits only one refresh while loading", () => {
    const refresh = vi.fn();
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={copy} stale loading onRefresh={refresh} />);
    expect(document.getElementById("attachment-storage-stale-usage")).toHaveTextContent(copy.attachment_storage_stale_usage);
    const button = screen.getByRole("button", { name: copy.attachment_storage_reload_cta });
    expect(button).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(button);
    expect(refresh).not.toHaveBeenCalled();

    rerender(<AttachmentStorageSettings result={available()} copy={copy} onRefresh={refresh} />);
    fireEvent.click(button);
    expect(refresh).toHaveBeenCalledOnce();
  });

  it("shows add-on storage separately from the included allowance", () => {
    render(<AttachmentStorageSettings result={available({ baseBytes: 50_000_000_000, packBytes: 20_000_000_000 })} copy={copy} />);
    expect(document.getElementById("attachment-storage-allowance")).toHaveTextContent("50 GB included + 20 GB add-on storage");
  });

  it("keeps direct storage management and offers App Store add-ons by origin", async () => {
    const purchase = vi.fn().mockResolvedValue("purchased");
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={copy} onManageStorage={vi.fn()} purchase={{ purchaseOptions: { appStore: false, website: true }, billingSummary: { provider: "none", manageableIn: "website", planActive: true, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false }, products: [], currentPacks: 0, state: "ready", onPurchase: purchase, onManageAppStore: vi.fn() }} />);
    expect(screen.getByRole("button", { name: copy.attachment_storage_manage_cta })).toBeInTheDocument();

    rerender(<AttachmentStorageSettings result={available()} copy={copy} purchase={{ purchaseOptions: { appStore: true, website: false }, billingSummary: { provider: "none", manageableIn: "app_store", planActive: true, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false }, products: [{ id: "org.peppy.desktop.storage.packs.2", kind: "storage", displayName: "100 GB", displayPrice: "$2", period: "month", storageBytes: 100_000_000_000 }], currentPacks: 0, state: "ready", onPurchase: purchase, onManageAppStore: vi.fn() }} />);
    fireEvent.click(document.getElementById("attachment-storage-addon-2-choose")!);
    expect(purchase).toHaveBeenCalledWith("org.peppy.desktop.storage.packs.2");
    expect(document.body.textContent?.toLowerCase()).not.toContain("website");
  });

  it("shows App Store lapsed and elsewhere notices without choose controls", () => {
    const basePurchase = { purchaseOptions: { appStore: true, website: false }, products: [], currentPacks: 1, state: "ready" as const, onPurchase: vi.fn(), onManageAppStore: vi.fn() };
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={copy} purchase={{ ...basePurchase, billingSummary: { provider: "apple", manageableIn: "app_store", planActive: false, storagePacks: 1, storageRenewsWithoutPlan: true, deletionPending: false } }} />);
    expect(document.getElementById("attachment-storage-addon-lapsed-warning")).toBeInTheDocument();
    expect(document.getElementById("attachment-storage-addon-1-choose")).toBeNull();

    rerender(<AttachmentStorageSettings result={available()} copy={copy} purchase={{ ...basePurchase, billingSummary: { provider: "stripe", manageableIn: "website", planActive: true, storagePacks: 1, storageRenewsWithoutPlan: false, deletionPending: false } }} />);
    expect(document.getElementById("billing-elsewhere-notice")).toHaveAttribute("data-variant", "other");
  });

  it("shows an add-on skeleton while products load and manages Apple subscriptions without packs", () => {
    render(<AttachmentStorageSettings result={available()} copy={copy} purchase={{ purchaseOptions: { appStore: true, website: false }, billingSummary: { provider: "apple", manageableIn: "app_store", planActive: true, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false }, products: [], currentPacks: 0, state: "loading", onPurchase: vi.fn(), onManageAppStore: vi.fn() }} />);
    expect(document.getElementById("attachment-storage-addon-skeleton")).toBeInTheDocument();
    expect(document.getElementById("attachment-storage-addon-0")).toBeNull();
    expect(document.getElementById("attachment-storage-addon-manage")).toBeInTheDocument();
  });

  it("hides unknown add-on sizes and reloads after a completed purchase", async () => {
    const onPurchased = vi.fn();
    const purchase = vi.fn().mockResolvedValue("purchased");
    render(<AttachmentStorageSettings result={available()} copy={copy} purchase={{ purchaseOptions: { appStore: true, website: false }, billingSummary: { provider: "none", manageableIn: "app_store", planActive: true, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false }, products: [{ id: "org.peppy.desktop.storage.packs.1", kind: "storage", displayName: "Storage", displayPrice: "$1", period: "month" }], currentPacks: 0, state: "ready", onPurchase: purchase, onManageAppStore: vi.fn(), onPurchased }} />);
    expect(document.getElementById("attachment-storage-addon-1-label")).toBeNull();
    fireEvent.click(document.getElementById("attachment-storage-addon-1-choose")!);
    await waitFor(() => expect(onPurchased).toHaveBeenCalledOnce());
  });

  it("replaces add-on choices after a provider-conflict error code", async () => {
    render(<AttachmentStorageSettings result={available()} copy={copy} purchase={{ purchaseOptions: { appStore: true, website: false }, billingSummary: { provider: "none", manageableIn: "app_store", planActive: true, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false }, products: [{ id: "org.peppy.desktop.storage.packs.1", kind: "storage", displayName: "Storage", displayPrice: "$1", period: "month", storageBytes: 50_000_000_000 }], currentPacks: 0, state: "ready", onPurchase: vi.fn().mockRejectedValue({ code: "store-conflict" }), onManageAppStore: vi.fn() }} />);
    fireEvent.click(document.getElementById("attachment-storage-addon-1-choose")!);
    await waitFor(() => expect(document.getElementById("billing-elsewhere-notice")).toBeInTheDocument());
  });

  it.each([
    ["apple", "apple", "apple"],
    ["none", "other", null],
    ["stripe", "website", "stripe"],
  ] as const)("uses the %s provider conflict variant", async (provider, variant, dataProvider) => {
    render(<AttachmentStorageSettings result={available()} copy={copy} purchase={{ purchaseOptions: { appStore: true, website: true }, billingSummary: { provider, manageableIn: "app_store", planActive: true, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false }, products: [{ id: "org.peppy.desktop.storage.packs.1", kind: "storage", displayName: "Storage", displayPrice: "$1", period: "month", storageBytes: 50_000_000_000 }], currentPacks: 0, state: "ready", onPurchase: vi.fn().mockRejectedValue({ code: "store-conflict" }), onManageAppStore: vi.fn() }} />);
    if (provider !== "stripe") fireEvent.click(document.getElementById("attachment-storage-addon-1-choose")!);

    await waitFor(() => expect(document.getElementById("billing-elsewhere-notice")).toHaveAttribute("data-variant", variant));
    const notice = document.getElementById("billing-elsewhere-notice");
    if (dataProvider === null) expect(notice).not.toHaveAttribute("data-billing-provider");
    else expect(notice).toHaveAttribute("data-billing-provider", dataProvider);
  });

  it("preserves the verified local-copy removal message", () => {
    expect(attachmentStorageRemovalText(true, copy)).toEqual({ title: "", body: copy.attachment_storage_removed_local_copy });
    expect(attachmentStorageRemovalText(false, copy)).toEqual({ title: copy.attachment_storage_removed_title, body: copy.attachment_storage_removed });
  });
});
