import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { RcsMediaActionsProvider } from "../rcs/RcsMediaActions";
import { MediaActionsProvider } from "./MediaActionsContext";
import { MediaTile } from "./MediaTile";
import type { GalleryAttachment, GalleryMessage } from "./grouping";

const message: GalleryMessage = { id: "message-rcs", revision: "1", sender: "other", body: "", timestamp: "2026-01-01T12:00:00.000Z", attachments: [] };
const photo = (overrides: Partial<GalleryAttachment> = {}): GalleryAttachment => ({ id: "rcs-photo", name: "IMG_0201.jpg", mediaType: "image/jpeg", byteSize: 67, state: "pending", index: 0, ...overrides } as GalleryAttachment);
const providerProps = { publicCopies: {}, onSave: vi.fn(), onPublish: vi.fn(), onRetry: vi.fn(), requestDownload: vi.fn(), loadOriginal: vi.fn().mockResolvedValue({ state: "pending" }), releaseMedia: vi.fn(), canCopyImage: false };

function renderTile(value: GalleryAttachment, retryDownload = vi.fn().mockResolvedValue(undefined)) {
  return render(<MediaActionsProvider {...providerProps}><RcsMediaActionsProvider retryBlock={{ kind: "none" }} retryDownload={retryDownload} openStorageSettings={vi.fn()}>
    <MediaTile attachment={value} message={{ ...message, attachments: [value] }} variant="single" label="Photo, 1 of 1" onOpen={vi.fn()} />
  </RcsMediaActionsProvider></MediaActionsProvider>);
}

describe("MediaTile RCS placeholders", () => {
  afterEach(cleanup);

  it.each([
    ["downloading", peppyCopy.rcs_media_state_downloading],
    ["too_large", peppyCopy.rcs_media_state_too_large],
    ["quota_full", peppyCopy.rcs_media_state_quota_full],
    ["failed", peppyCopy.rcs_media_state_failed],
    ["expired", peppyCopy.rcs_media_state_expired],
  ] as const)("replaces the photo with a labelled placeholder for %s", (state, label) => {
    const { container } = renderTile(photo({ rcsState: state }));
    const tile = container.querySelector("[data-media-tile]");
    expect(tile).toHaveAttribute("data-rcs-media-state", state);
    expect(tile).toHaveAttribute("data-rcs-media-direction", "incoming");
    expect(screen.getByRole("group", { name: peppyCopy.rcs_media_state_label.replace("{label}", "Photo, 1 of 1").replace("{state}", label) })).toHaveAttribute("id", "rcs-media-placeholder-rcs-photo");
    expect(document.getElementById("rcs-media-status-rcs-photo")).toHaveTextContent(label);
    expect(container.querySelector("img")).toBeNull();
  });

  it("describes a too-large photo and offers no retry", () => {
    renderTile(photo({ rcsState: "too_large" }));
    const group = screen.getByRole("group");
    expect(group).toHaveAttribute("aria-describedby", "rcs-media-detail-rcs-photo");
    expect(document.getElementById("rcs-media-detail-rcs-photo")).toHaveTextContent(peppyCopy.rcs_media_detail_too_large.replace("{limit}", "32 MiB"));
    expect(screen.queryByRole("button", { name: /Retry/ })).toBeNull();
  });

  it("uses the loading glyph while an incoming photo is downloading", () => {
    const { container } = renderTile(photo({ rcsState: "downloading" }));
    expect(container.querySelector(".tile-loading-icon")).toBeInTheDocument();
  });

  it("gives a storage-full photo a retry and a free-space action, and sends the retry for its index", () => {
    const retryDownload = vi.fn().mockResolvedValue(undefined);
    renderTile(photo({ rcsState: "quota_full", index: 3 }), retryDownload);
    fireEvent.click(screen.getByRole("button", { name: /Retry download for IMG_0201.jpg/ }));
    expect(retryDownload).toHaveBeenCalledWith("message-rcs", 3);
    expect(screen.getByRole("button", { name: peppyCopy.rcs_media_free_space })).toBeInTheDocument();
  });

  it("leaves a stored photo as an ordinary tile", () => {
    const { container } = renderTile(photo({ rcsState: "stored", state: "ready", previewUrl: "https://example.test/photo.jpg" }));
    expect(container.querySelector(".tile-placeholder")).toBeNull();
    expect(container.querySelector("[data-media-tile]")).toHaveAttribute("data-rcs-media-state", "stored");
  });

  it("shows a per-attachment send pill for an outgoing photo that is not sent", () => {
    const value = photo({ id: "rcs-out", rcsSendStatus: "not_sent", state: "ready", previewUrl: "https://example.test/photo.jpg" });
    const { container } = render(<MediaActionsProvider {...providerProps}><MediaTile attachment={value} message={{ ...message, attachments: [value, photo({ id: "rcs-out-2", rcsSendStatus: "sent" })] }} variant="single" label="Photo, 1 of 2" onOpen={vi.fn()} /></MediaActionsProvider>);
    expect(container.querySelector("[data-media-tile]")).toHaveAttribute("data-rcs-media-direction", "outgoing");
    expect(container.querySelector("[data-rcs-media-status]")).toHaveTextContent(peppyCopy.rcs_media_state_not_sent);
  });
});

describe("RCS replacement focus", () => {
  afterEach(cleanup);
  it.each([false, true])("retains focus through downloading=%s and a changed stored id", (intermediate) => {
    const view = (rcsState: "quota_full" | "downloading" | "stored") => {
      const value = photo({ rcsState, ...(rcsState === "stored" ? { id: "hosted-photo", state: "ready", previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" } : {}) });
      return <MediaActionsProvider {...providerProps}><RcsMediaActionsProvider retryBlock={{ kind: "none" }} retryDownload={vi.fn()} openStorageSettings={vi.fn()}><MediaTile attachment={value} message={{ ...message, attachments: [value] }} variant="single" onOpen={vi.fn()} /></RcsMediaActionsProvider></MediaActionsProvider>;
    };
    const result = render(view("quota_full"));
    screen.getByRole("button", { name: /Retry download/ }).focus();
    if (intermediate) result.rerender(view("downloading"));
    result.rerender(view("stored"));
    expect(document.activeElement).toBe(result.container.querySelector("button.tile-body"));
  });
});
