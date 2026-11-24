import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge } from "./bridge/bridge";
import { createHost, installAppHost } from "./test-support/appHost";
import { openComposerWindow } from "./test-support/appFixtures";
import { cleanupAppTest, resetAppStorage } from "./test-support/appSetup";

vi.mock("./updates/updates", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./updates/updates")>()),
  useUpdateDistribution: vi.fn(),
}));

let host: ReturnType<typeof createHost>;

beforeEach(() => {
  resetAppStorage();
  vi.restoreAllMocks();
  host = createHost();
  installAppHost(host, {
    lifecycle: vi.fn(), lifecycleFinished: vi.fn(), subscribe: vi.fn(), drafts: vi.fn(),
    lifecycleDispose: vi.fn(), lifecycleFinishedDispose: vi.fn(), draftDispose: vi.fn(),
    hintListeners: new Set(),
  });
});

afterEach(cleanupAppTest);

it("renders a prototype-property sender label blank", async () => {
  host.conversations[0] = {
    ...host.conversations[0],
    participants: ["constructor", "+18015551235"],
    messages: [{ id: "prototype-sender", revision: "1", sender: "other", senderAddress: "constructor", body: "Prototype sender message", timestamp: "1788434460000", attachments: [] }],
  };
  render(<App />);
  await screen.findByText("Prototype sender message");
  const label = document.querySelector('[data-sender-label="constructor"]');
  expect(label).toBeInTheDocument();
  // Pins current behaviour of review finding D-34; flip this assertion when the fix is authorized.
  expect(label).toBeEmptyDOMElement();
});

it("collapses a composer when Escape is pressed in its media dialog", async () => {
  openComposerWindow("aurora");
  const attachment = { id: "image", name: "image.png", mediaType: "image/png", byteSize: 1, state: "ready" as const, previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" };
  host.conversations[0].messages = [{ id: "image-message", revision: "1", sender: "other", senderAddress: "+15550001", transport: "mms", body: "", timestamp: "1788434460000", attachments: [attachment] }];
  const target = { attachment, messageId: "image-message", timestamp: "1788434460000", cursor: { sortAtMs: "1788434460000", localOrder: "image-message", messageId: "image-message", attachmentPosition: 0 } };
  vi.spyOn(bridge, "media_page").mockResolvedValue({ items: [target] });
  vi.spyOn(bridge, "media_page_around").mockResolvedValue({ target, newer: [], older: [] });
  vi.spyOn(bridge, "open_media").mockResolvedValue({ state: "pending" });
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "image.png" }));
  const dialog = await screen.findByRole("dialog", { name: "Media viewer" });
  fireEvent.keyDown(dialog, { key: "Escape" });
  // Pins current behaviour of review finding D-33; flip this assertion when the fix is authorized.
  await waitFor(() => expect(bridge.close_composer).toHaveBeenCalledOnce());
});
