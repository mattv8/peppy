import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AttachmentView, RcsAttachmentState } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";
import { RcsMediaActionsProvider } from "./RcsMediaActions";
import { RcsMediaFileRow, RcsRetryAll } from "./RcsMediaPlaceholder";
import { RCS_RETRY_COOLDOWN_MS } from "./rcsMediaState";
import type { RcsDownloadRetryBlock } from "./rcsRoute";

const online: RcsDownloadRetryBlock = { kind: "none" };
const file = (overrides: Partial<AttachmentView> = {}): AttachmentView => ({ id: "rcs-file", name: "estimate.pdf", mediaType: "application/pdf", byteSize: 2048, state: "pending", rcsState: "failed", index: 0, ...overrides });

type Options = { retryDownload?: (messageId: string, index: number) => Promise<void>; retryBlock?: RcsDownloadRetryBlock; openStorageSettings?: () => void };

/** Renders a file row inside the provider. `rerender` swaps the attachment state while keeping the same provider. */
function renderRow(attachment: AttachmentView, options: Options = {}) {
  const retryDownload = options.retryDownload ?? vi.fn().mockResolvedValue(undefined);
  const openStorageSettings = options.openStorageSettings ?? vi.fn();
  const wrap = (value: AttachmentView) => <RcsMediaActionsProvider retryBlock={options.retryBlock ?? online} retryDownload={retryDownload} openStorageSettings={openStorageSettings}>
    <RcsMediaFileRow messageId="message-in" attachment={value} attachmentIndex={value.index ?? 0} size="2 KB" />
  </RcsMediaActionsProvider>;
  const view = render(wrap(attachment));
  return { ...view, retryDownload, openStorageSettings, rerenderState: (state: RcsAttachmentState) => view.rerender(wrap({ ...attachment, rcsState: state })) };
}

describe("RcsMediaFileRow", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it.each([
    ["downloading", peppyCopy.rcs_media_state_downloading, undefined],
    ["too_large", peppyCopy.rcs_media_state_too_large, peppyCopy.rcs_media_detail_too_large.replace("{limit}", "32 MiB")],
    ["quota_full", peppyCopy.rcs_media_state_quota_full, peppyCopy.rcs_media_detail_quota_full],
    ["failed", peppyCopy.rcs_media_state_failed, peppyCopy.rcs_media_detail_failed],
    ["expired", peppyCopy.rcs_media_state_expired, undefined],
  ] as const)("renders the %s state as an incoming file row with its label and detail", (state, label, detail) => {
    const { container } = renderRow(file({ rcsState: state }));
    const row = container.querySelector("[data-rcs-media-state]");
    expect(row).toHaveAttribute("data-rcs-media-state", state);
    expect(row).toHaveAttribute("data-rcs-media-direction", "incoming");
    expect(row).toHaveAttribute("id", "rcs-media-placeholder-rcs-file");
    expect(document.getElementById("rcs-media-status-rcs-file")).toHaveTextContent(label);
    if (detail) expect(document.getElementById("rcs-media-detail-rcs-file")).toHaveTextContent(detail);
    else expect(document.getElementById("rcs-media-detail-rcs-file")).toBeNull();
    expect(screen.queryByRole("button", { name: /^Save/ })).toBeNull();
  });

  it("offers Retry download for storage full and failed, and Free up space only for storage full", () => {
    const quota = renderRow(file({ rcsState: "quota_full" }));
    expect(screen.getByRole("button", { name: /Retry download for estimate.pdf/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: peppyCopy.rcs_media_free_space })).toBeInTheDocument();
    quota.unmount();

    renderRow(file({ rcsState: "failed" }));
    expect(screen.getByRole("button", { name: /Retry download/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: peppyCopy.rcs_media_free_space })).toBeNull();
  });

  it.each(["downloading", "too_large", "expired"] as const)("offers no retry for %s", (state) => {
    renderRow(file({ rcsState: state }));
    expect(screen.queryByRole("button", { name: /Retry/ })).toBeNull();
  });

  it("keeps focus on the retry button, marks it requested, and re-enables it after the cooldown", async () => {
    vi.useFakeTimers();
    const retryDownload = vi.fn().mockResolvedValue(undefined);
    renderRow(file({ rcsState: "failed", index: 2 }), { retryDownload });
    const button = document.getElementById("rcs-media-retry-rcs-file") as HTMLButtonElement;
    button.focus();
    fireEvent.click(button);

    expect(retryDownload).toHaveBeenCalledWith("message-in", 2);
    expect(document.getElementById("rcs-media-retry-rcs-file")).toBe(button);
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveTextContent(peppyCopy.rcs_media_retry_requested);
    expect(document.activeElement).toBe(button);
    expect(screen.getByRole("status")).toHaveTextContent(peppyCopy.rcs_media_retry_requested);
    expect(button).toHaveAttribute("aria-describedby", "rcs-media-retry-note-rcs-file");

    fireEvent.click(button);
    expect(retryDownload).toHaveBeenCalledTimes(1);

    await act(async () => { vi.advanceTimersByTime(RCS_RETRY_COOLDOWN_MS); });
    expect(button).not.toHaveAttribute("aria-disabled");
    expect(button).toHaveTextContent(peppyCopy.rcs_media_retry_again);
    fireEvent.click(button);
    expect(retryDownload).toHaveBeenCalledTimes(2);
  });

  it("ends a request when the state changes and shows the new state's own actions", () => {
    const { rerenderState } = renderRow(file({ rcsState: "quota_full" }), { retryDownload: vi.fn().mockResolvedValue(undefined) });
    fireEvent.click(screen.getByRole("button", { name: /Retry download/ }));
    expect(screen.getByRole("button", { name: peppyCopy.rcs_media_retry_requested })).toBeInTheDocument();
    rerenderState("failed");
    const button = document.getElementById("rcs-media-retry-rcs-file");
    expect(button).toHaveTextContent(peppyCopy.rcs_media_retry);
    expect(button).not.toHaveAttribute("aria-disabled");
    expect(document.getElementById("rcs-media-free-space-rcs-file")).toBeNull();
  });

  it("reports a rejected retry and restores the button", async () => {
    const retryDownload = vi.fn().mockRejectedValue(new Error("gateway"));
    renderRow(file({ rcsState: "failed" }), { retryDownload });
    fireEvent.click(document.getElementById("rcs-media-retry-rcs-file")!);
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveAttribute("data-rcs-media-retry-error");
    expect(alert).toHaveTextContent(peppyCopy.rcs_media_retry_error);
    expect(document.getElementById("rcs-media-retry-rcs-file")).not.toHaveAttribute("aria-disabled");
    expect(document.getElementById("rcs-media-retry-rcs-file")).toHaveTextContent(peppyCopy.rcs_media_retry);
  });

  it("disables retry offline and explains why, while Free up space still navigates", () => {
    const retryDownload = vi.fn().mockResolvedValue(undefined);
    const openStorageSettings = vi.fn();
    renderRow(file({ rcsState: "quota_full" }), { retryDownload, openStorageSettings, retryBlock: { kind: "offline" } });
    const button = document.getElementById("rcs-media-retry-rcs-file")!;
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAttribute("aria-describedby", "rcs-media-retry-note-rcs-file");
    expect(document.getElementById("rcs-media-retry-note-rcs-file")).toHaveTextContent(peppyCopy.rcs_media_retry_offline);
    fireEvent.click(button);
    expect(retryDownload).not.toHaveBeenCalled();
    fireEvent.click(document.getElementById("rcs-media-free-space-rcs-file")!);
    expect(openStorageSettings).toHaveBeenCalledTimes(1);
  });

  it("explains a link that needs attention", () => {
    renderRow(file({ rcsState: "failed" }), { retryBlock: { kind: "link" } });
    expect(document.getElementById("rcs-media-retry-note-rcs-file")).toHaveTextContent(peppyCopy.rcs_media_retry_link);
  });

  it("renders nothing for stored or unknown attachments", () => {
    const { container } = render(<RcsMediaFileRow messageId="message-in" attachment={file({ rcsState: "stored" })} attachmentIndex={0} />);
    expect(container).toBeEmptyDOMElement();
  });
});

describe("RcsRetryAll", () => {
  afterEach(cleanup);

  it("stays hidden for a single placeholder", () => {
    render(<RcsMediaActionsProvider retryBlock={online} retryDownload={vi.fn()} openStorageSettings={vi.fn()}><RcsRetryAll messageId="message-in" placeholders={[{ index: 0, state: "failed" }]} /></RcsMediaActionsProvider>);
    expect(screen.queryByRole("button", { name: peppyCopy.rcs_media_retry_all })).toBeNull();
  });

  it("retries each attachment one after another, never in parallel", async () => {
    const resolvers: Array<() => void> = [];
    const retryDownload = vi.fn(() => new Promise<void>((resolve) => resolvers.push(resolve)));
    render(<RcsMediaActionsProvider retryBlock={online} retryDownload={retryDownload} openStorageSettings={vi.fn()}><RcsRetryAll messageId="message-in" placeholders={[{ index: 1, state: "failed" }, { index: 3, state: "quota_full" }]} /></RcsMediaActionsProvider>);
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.rcs_media_retry_all }));
    expect(retryDownload).toHaveBeenCalledTimes(1);
    expect(retryDownload).toHaveBeenLastCalledWith("message-in", 1);
    await act(async () => { resolvers[0](); });
    await waitFor(() => expect(retryDownload).toHaveBeenCalledTimes(2));
    expect(retryDownload).toHaveBeenLastCalledWith("message-in", 3);
    await act(async () => { resolvers[1](); });
  });

  it("does nothing while offline and says why", () => {
    const retryDownload = vi.fn().mockResolvedValue(undefined);
    render(<RcsMediaActionsProvider retryBlock={{ kind: "offline" }} retryDownload={retryDownload} openStorageSettings={vi.fn()}><RcsRetryAll messageId="message-in" placeholders={[{ index: 0, state: "failed" }, { index: 1, state: "quota_full" }]} /></RcsMediaActionsProvider>);
    const button = screen.getByRole("button", { name: peppyCopy.rcs_media_retry_all });
    expect(button).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(button);
    expect(retryDownload).not.toHaveBeenCalled();
    expect(screen.getByText(peppyCopy.rcs_media_retry_offline)).toBeInTheDocument();
  });
});

describe("offline downloading recovery", () => {
  afterEach(cleanup);
  it("shows the waiting-for-phone state without enabling a second request", () => {
    const { container } = renderRow(file({ rcsState: "downloading" }), { retryBlock: { kind: "offline" } });
    expect(container.querySelector("[data-rcs-media-waiting]")).toHaveTextContent(peppyCopy.rcs_media_retry_waiting);
    expect(screen.queryByRole("button", { name: /Retry/ })).toBeNull();
    expect(container.querySelector("[role='status']")).toBeNull();
  });
});
