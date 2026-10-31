import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AttachmentRetentionStatusResult } from "./bridge-contracts";
import { AttachmentStorageSettings, attachmentStorageRemovalText } from "./AttachmentStorageSettings";
import { peppyCopy } from "../../../apps/desktop/src/generated/peppyCopy";

afterEach(cleanup);

const available = (overrides: Partial<Extract<AttachmentRetentionStatusResult, { kind: "available" }>["status"]> = {}): AttachmentRetentionStatusResult => ({
  kind: "available",
  scopeId: "session-one",
  status: {
    usedBytes: 20_000_000_000,
    limitBytes: 75_000_000_000,
    baseBytes: 50_000_000_000,
    packBytes: 25_000_000_000,
    protectedBytes: 4_000_000_000,
    removedBytes: 0,
    cleanupState: "disabled",
    cleanupUnavailableReason: "cleanup_disabled",
    notice: {
      state: "not_presented",
      policyVersion: "hosted_attachment_retention_v1",
      presentedAtSeconds: null,
      acknowledgedAtSeconds: null,
      activationAtSeconds: 1_800_000_000,
    },
    ...overrides,
  },
});

describe("AttachmentStorageSettings", () => {
  it("records the supported disclosure once per rendered session scope", () => {
    const acknowledge = vi.fn();
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={peppyCopy} onAcknowledge={acknowledge} />);

    expect(screen.getByText("Standard plans include 50 GB of hosted attachment storage.")).toBeInTheDocument();
    expect(acknowledge).toHaveBeenCalledWith("session-one", "hosted_attachment_retention_v1");
    expect(document.getElementById("attachment-storage-notice")).toHaveTextContent(peppyCopy.attachment_storage_notice);
    expect(document.getElementById("attachment-storage-notice")).toHaveTextContent(peppyCopy.attachment_storage_protected);
    expect(document.getElementById("attachment-storage-notice")).toHaveTextContent(peppyCopy.attachment_storage_device_copies);
    expect(document.getElementById("attachment-storage-notice")).toHaveTextContent(peppyCopy.attachment_storage_purchase);
    expect(screen.getByRole("link", { name: "Privacy Policy" })).toBeInTheDocument();

    rerender(<AttachmentStorageSettings result={available()} copy={peppyCopy} onAcknowledge={acknowledge} />);
    expect(acknowledge).toHaveBeenCalledTimes(1);
  });

  it("shows unknown policy data but never acknowledges unsupported copy", () => {
    const acknowledge = vi.fn();
    render(<AttachmentStorageSettings result={available({ notice: { state: "not_presented", policyVersion: "future-v2", presentedAtSeconds: null, acknowledgedAtSeconds: null, activationAtSeconds: null } })} copy={peppyCopy} onAcknowledge={acknowledge} />);

    expect(screen.getByText("20 GB")).toBeInTheDocument();
    expect(screen.getByText("75 GB")).toBeInTheDocument();
    expect(screen.getByText(/Storage policy status is unavailable/)).toBeInTheDocument();
    expect(document.getElementById("attachment-storage-notice")).not.toBeInTheDocument();
    expect(screen.queryByText(peppyCopy.attachment_storage_notice)).not.toBeInTheDocument();
    expect(screen.queryByText(peppyCopy.attachment_storage_grace)).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Privacy Policy" })).not.toBeInTheDocument();
    expect(acknowledge).not.toHaveBeenCalled();
  });

  it("keeps billing and privacy opening failures distinct", () => {
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={peppyCopy} onManageStorage={vi.fn()} manageStorageError />);
    expect(screen.getByRole("alert")).toHaveTextContent(peppyCopy.attachment_storage_billing_open_failed);

    rerender(<AttachmentStorageSettings result={available()} copy={peppyCopy} privacyError />);
    expect(screen.getByRole("alert")).toHaveTextContent(peppyCopy.attachment_storage_privacy_open_failed);
    expect(screen.queryByText(peppyCopy.attachment_storage_billing_open_failed)).not.toBeInTheDocument();
  });

  it("keeps a validated browser billing handoff after retention is unsupported", () => {
    render(<AttachmentStorageSettings result={{ kind: "unsupported" }} copy={peppyCopy} manageStorageUrl="https://account.example.test/billing" />);

    expect(screen.getByRole("link", { name: /manage storage on the website/i })).toHaveAttribute("href", "https://account.example.test/billing");
    expect(screen.queryByText("When automatic cleanup is enabled")).not.toBeInTheDocument();
  });

  it("binds the server activation date into one labelled announcement and rejects unsafe privacy links", () => {
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={peppyCopy} privacyUrl="javascript:alert(1)" />);

    expect(screen.queryByRole("link", { name: "Privacy Policy" })).not.toBeInTheDocument();
    const activation = screen.getByRole("status", { name: /Cleanup will not begin before/ });
    expect(activation).toHaveTextContent("Cleanup will not begin before");

    rerender(<AttachmentStorageSettings result={available()} copy={peppyCopy} privacyUrl="https://peppy.pro/privacy" />);
    expect(screen.getByRole("link", { name: "Privacy Policy" })).toHaveAttribute("href", "https://peppy.pro/privacy");
  });

  it("uses the shared grace-full blocker without inferring cleanup from the server date", () => {
    render(<AttachmentStorageSettings
      result={available({ usedBytes: 75_000_000_000, cleanupState: "blocked", cleanupUnavailableReason: "notice_not_eligible", notice: { state: "grace_period", policyVersion: "hosted_attachment_retention_v1", presentedAtSeconds: 1_700_000_000, acknowledgedAtSeconds: 1_700_000_001, activationAtSeconds: 1_800_000_000 } })}
      copy={{ ...peppyCopy, attachment_storage_grace_full: "Shared grace-full copy" }}
    />);

    expect(screen.getByRole("status", { name: "" })).toHaveTextContent("Shared grace-full copy");
    expect(screen.queryByText("Storage cleanup is pending.")).not.toBeInTheDocument();
  });

  it("keeps global-off and read-only precedence over full grace", () => {
    const grace = { state: "grace_period" as const, policyVersion: "hosted_attachment_retention_v1", presentedAtSeconds: 1_700_000_000, acknowledgedAtSeconds: 1_700_000_001, activationAtSeconds: 1_800_000_000 };
    const { rerender } = render(<AttachmentStorageSettings result={available({ usedBytes: 75_000_000_000, cleanupState: "disabled", cleanupUnavailableReason: "cleanup_disabled", notice: grace })} copy={peppyCopy} />);

    expect(document.getElementById("attachment-storage-policy-state")).toHaveTextContent("Automatic cleanup is not enabled");
    rerender(<AttachmentStorageSettings result={available({ usedBytes: 75_000_000_000, cleanupState: "blocked", cleanupUnavailableReason: "read_only", notice: grace })} copy={peppyCopy} />);
    expect(document.getElementById("attachment-storage-policy-state")).toHaveTextContent("Your subscription is read-only");
  });

  it("maps billing and device blockers to their shared labels", () => {
    const { rerender } = render(<AttachmentStorageSettings result={available({ cleanupState: "blocked", cleanupUnavailableReason: "billing_mutation_pending" })} copy={peppyCopy} />);
    expect(document.getElementById("attachment-storage-policy-state")).toHaveTextContent("Your storage change is still processing");

    rerender(<AttachmentStorageSettings result={available({ cleanupState: "blocked", cleanupUnavailableReason: "device_not_ready" })} copy={peppyCopy} />);
    expect(document.getElementById("attachment-storage-policy-state")).toHaveTextContent("Automatic cleanup is waiting for compatible, ready devices");
  });

  it("labels stale usage and notice recording failures without hiding known status", () => {
    render(<AttachmentStorageSettings result={available()} copy={peppyCopy} stale acknowledgementError onRefresh={vi.fn()} />);

    expect(document.getElementById("attachment-storage-stale-usage")).toHaveTextContent("Last known storage usage");
    expect(screen.getByRole("alert")).toHaveTextContent("The notice could not be recorded");
    expect(screen.getByText(/20 GB/)).toBeInTheDocument();
  });

  it("always offers Refresh and disables it while a refresh is in progress", () => {
    const refresh = vi.fn();
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={peppyCopy} loading onRefresh={refresh} />);

    const button = screen.getByRole("button", { name: "Refresh" });
    expect(button).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(button);
    expect(refresh).not.toHaveBeenCalled();
    rerender(<AttachmentStorageSettings result={available()} copy={peppyCopy} onRefresh={refresh} />);
    fireEvent.click(button);
    expect(refresh).toHaveBeenCalledOnce();
  });

  it("acknowledges each rendered scope at most once", () => {
    const acknowledge = vi.fn();
    const { rerender } = render(<AttachmentStorageSettings result={available()} copy={peppyCopy} onAcknowledge={acknowledge} />);
    rerender(<AttachmentStorageSettings result={{ ...available(), scopeId: "session-two" } as AttachmentRetentionStatusResult} copy={peppyCopy} onAcknowledge={acknowledge} />);
    rerender(<AttachmentStorageSettings result={available()} copy={peppyCopy} onAcknowledge={acknowledge} />);

    expect(acknowledge).toHaveBeenCalledTimes(2);
  });

  it("keeps a verified local copy usable while remote-only removal is permanent", () => {
    expect(attachmentStorageRemovalText(true, peppyCopy)).toEqual({ title: "", body: "Hosted copy removed. Available on this device." });
    expect(attachmentStorageRemovalText(false, peppyCopy)).toEqual({ title: "Hosted media removed", body: "This attachment was removed from hosted storage to make room. The message remains." });
  });
});
