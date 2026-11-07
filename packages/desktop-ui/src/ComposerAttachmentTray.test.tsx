import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ComposerAttachmentTray } from "./ComposerAttachmentTray";
import { Composer } from "./index";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const attachments = [
  { id: "one", name: "one.png", state: "pending" as const, byteSize: 1024 },
  { id: "two", name: "two.png", state: "failed" as const, byteSize: 1024 },
];

function pasteData(files: File[], text = ""): DataTransfer {
  return { files, getData: (type: string) => type === "text/plain" ? text : "" } as unknown as DataTransfer;
}

describe("ComposerAttachmentTray", () => {
  it("renders tile and row layouts with mapped statuses", () => {
    const { rerender } = render(<ComposerAttachmentTray attachments={attachments} onRemoveAttachment={() => {}} onRetryAttachment={() => {}} onFocusTextarea={() => {}} />);
    expect(screen.getByText("2 images")).toBeInTheDocument();
    expect(screen.getByText("Waiting for connection")).toBeInTheDocument();
    expect(screen.getByText("Upload failed")).toBeInTheDocument();
    expect(document.querySelector("#attachment-tray")).toHaveAttribute("data-attachment-layout", "tiles");
    rerender(<ComposerAttachmentTray attachments={attachments} layout="rows" onRemoveAttachment={() => {}} onFocusTextarea={() => {}} />);
    expect(document.querySelector("#attachment-tray")).toHaveAttribute("data-attachment-layout", "rows");
  });

  it("shows retry only for actionable states and moves removal focus", () => {
    const remove = vi.fn();
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { callback(0); return 1; });
    render(<><ComposerAttachmentTray attachments={attachments} onRemoveAttachment={remove} onRetryAttachment={() => {}} onFocusTextarea={() => {}} /><textarea id="composer-textarea" /></>);
    expect(screen.queryByRole("button", { name: "Retry one.png" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry two.png" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Remove one.png" }));
    expect(remove).toHaveBeenCalledWith("one");
    expect(screen.getByRole("button", { name: "Remove two.png" })).toHaveFocus();
  });

  it("shows only the failed status and keeps returned details descriptive", () => {
    const retry = vi.fn();
    const longName = "a-very-long-unbroken-attachment-name-that-must-remain-available.png";
    const returnedError = "The server rejected this attachment because its encrypted upload token expired.";
    render(<ComposerAttachmentTray
      attachments={[
        { id: "first", name: longName, state: "failed", error: returnedError },
        { id: "second", name: "second.png", state: "failed" },
      ]}
      onRetryAttachment={retry}
      onFocusTextarea={() => {}}
    />);

    expect(screen.getByText(longName)).toHaveTextContent(longName);
    const firstTile = document.querySelector<HTMLElement>("[data-attachment-id=first]")!;
    expect(within(firstTile).queryByRole("alert")).not.toBeInTheDocument();
    expect(within(firstTile).getByText("Upload failed")).toBeVisible();
    expect(screen.getByRole("button", { name: `Retry ${longName}` })).toHaveAccessibleDescription(returnedError);
    expect(document.querySelector(".attachment-error")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: `Retry ${longName}` }));
    expect(retry).toHaveBeenCalledOnce();
    expect(retry).toHaveBeenCalledWith("first");
  });

  it("replaces the total size with header detail when supplied", () => {
    render(<ComposerAttachmentTray attachments={attachments} attachmentHeaderDetail={<span>Fits in one MMS</span>} onFocusTextarea={() => {}} />);

    expect(screen.getByText("Fits in one MMS")).toBeInTheDocument();
    expect(screen.queryByText("2 KB")).not.toBeInTheDocument();
  });

  it("announces status changes and removals but not its initial contents", () => {
    const { container, rerender } = render(<ComposerAttachmentTray attachments={attachments} onFocusTextarea={() => {}} />);
    const liveRegion = container.querySelector("[aria-live=polite]")!;
    expect(liveRegion).toHaveTextContent("");
    rerender(<ComposerAttachmentTray attachments={[{ ...attachments[0], state: "ready" }]} onFocusTextarea={() => {}} />);
    expect(liveRegion).toHaveTextContent("one.png: Uploaded, two.png removed");
  });

  it("announces a newly failed attachment once", () => {
    const { container, rerender } = render(<ComposerAttachmentTray attachments={[attachments[0]]} onFocusTextarea={() => {}} />);

    rerender(<ComposerAttachmentTray attachments={[attachments[0], { id: "failed", name: "failed.png", state: "failed" }]} onFocusTextarea={() => {}} />);

    expect(container.querySelectorAll("[aria-live=polite]")).toHaveLength(1);
    expect(container.querySelector("[aria-live=polite]")).toHaveTextContent("failed.png: Upload failed");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("replaces failed previews with a placeholder icon", () => {
    render(<ComposerAttachmentTray attachments={[{ ...attachments[0], previewUrl: "broken-image" }]} onFocusTextarea={() => {}} />);
    fireEvent.error(document.querySelector(".attachment-preview img")!);
    expect(document.querySelector(".attachment-preview img")).not.toBeInTheDocument();
    expect(document.querySelector(".attachment-preview svg")).toBeInTheDocument();
  });
});

describe("Composer paste", () => {
  const baseProps = { attachments: [], draft: "hello", sendSupported: true, onDraftChange: vi.fn(), onSend: vi.fn() };

  it("inserts accepted caption text at the selection and passes images", () => {
    const onDraftChange = vi.fn();
    const onPasteImages = vi.fn();
    render(<Composer {...baseProps} onDraftChange={onDraftChange} onPasteImages={onPasteImages} />);
    const textarea = screen.getByRole("textbox") as HTMLTextAreaElement;
    textarea.setSelectionRange(1, 4);
    fireEvent.paste(textarea, { clipboardData: pasteData([new File(["x"], "image.png", { type: "image/png" })], "caption") });
    expect(onDraftChange).toHaveBeenCalledWith("hcaptiono");
    expect(onPasteImages).toHaveBeenCalledWith([expect.any(File)]);
  });

  it("restores the caret after the inserted caption", async () => {
    function ControlledComposer() {
      const [draft, setDraft] = useState("hello");
      return <Composer {...baseProps} draft={draft} onDraftChange={setDraft} onPasteImages={() => {}} />;
    }
    render(<ControlledComposer />);
    const textarea = screen.getByRole("textbox") as HTMLTextAreaElement;
    textarea.focus();
    textarea.setSelectionRange(1, 4);
    fireEvent.paste(textarea, { clipboardData: pasteData([new File(["x"], "image.png", { type: "image/png" })], "caption") });
    await waitFor(() => expect(textarea.selectionStart).toBe(8));
    expect(textarea.selectionEnd).toBe(8);
  });

  it("does nothing for read-only composers or without an image callback", () => {
    const readOnlyImages = vi.fn();
    const { rerender } = render(<Composer {...baseProps} readOnly onPasteImages={readOnlyImages} />);
    const textarea = screen.getByRole("textbox");
    fireEvent.paste(textarea, { clipboardData: pasteData([new File(["x"], "image.bmp", { type: "image/bmp" })]) });
    expect(readOnlyImages).not.toHaveBeenCalled();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    rerender(<Composer {...baseProps} />);
    fireEvent.paste(screen.getByRole("textbox"), { clipboardData: pasteData([new File(["x"], "image.png", { type: "image/png" })]) });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("handles tray paste with remaining attachment slots and reason-specific feedback", () => {
    const onPasteImages = vi.fn();
    render(<Composer {...baseProps} attachments={attachments} maxAttachments={3} onPasteImages={onPasteImages} />);
    fireEvent.paste(screen.getByRole("list", { name: "Image list" }), { clipboardData: pasteData([new File(["x"], "image.png", { type: "image/png" }), new File(["x"], "second.png", { type: "image/png" })]) });
    expect(onPasteImages).toHaveBeenCalledWith([expect.any(File)]);
    expect(screen.getByText("Couldn't add second.png. Up to 3 images per message.")).toHaveAttribute("role", "alert");
  });

  it("clears a paste rejection and tray history when changing drafts", () => {
    const { rerender } = render(<Composer {...baseProps} draftKey="draft-one" attachments={[attachments[0]]} onPasteImages={() => {}} />);
    fireEvent.paste(screen.getByRole("textbox"), { clipboardData: pasteData([new File(["x"], "image.bmp", { type: "image/bmp" })]) });
    expect(screen.getByRole("alert")).toBeInTheDocument();

    rerender(<Composer {...baseProps} draftKey="draft-two" attachments={[attachments[1]]} onPasteImages={() => {}} />);

    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(document.querySelector("[aria-live=polite]")).toHaveTextContent("");
  });

  it("does not send when Enter activates attachment controls", () => {
    const onSend = vi.fn();
    const onRetryAttachment = vi.fn();
    const onRemoveAttachment = vi.fn();
    render(<Composer {...baseProps} attachments={attachments} onSend={onSend} onRetryAttachment={onRetryAttachment} onRemoveAttachment={onRemoveAttachment} />);
    const retry = screen.getByRole("button", { name: "Retry two.png" });
    const remove = screen.getByRole("button", { name: "Remove one.png" });
    retry.focus();
    fireEvent.keyDown(retry, { key: "Enter" });
    fireEvent.click(retry);
    remove.focus();
    fireEvent.keyDown(remove, { key: "Enter" });
    fireEvent.click(remove);
    expect(onRetryAttachment).toHaveBeenCalledWith("two");
    expect(onRemoveAttachment).toHaveBeenCalledWith("one");
    expect(onSend).not.toHaveBeenCalled();
  });

  it("focuses the textarea callback after removing the last item", () => {
    const onFocusTextarea = vi.fn();
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { callback(0); return 1; });
    render(<ComposerAttachmentTray attachments={[attachments[0]]} onRemoveAttachment={() => {}} onFocusTextarea={onFocusTextarea} />);
    fireEvent.click(screen.getByRole("button", { name: "Remove one.png" }));
    expect(onFocusTextarea).toHaveBeenCalledOnce();
  });

  it("announces a last attachment removal from Composer", () => {
    function ControlledComposer() {
      const [currentAttachments, setCurrentAttachments] = useState([attachments[0]]);
      return <Composer {...baseProps} attachments={currentAttachments} onRemoveAttachment={(id) => setCurrentAttachments((current) => current.filter((attachment) => attachment.id !== id))} />;
    }
    render(<ControlledComposer />);
    fireEvent.click(screen.getByRole("button", { name: "Remove one.png" }));
    expect(document.querySelector("[aria-live=polite]")).toHaveTextContent("one.png removed");
  });

  it("does not intercept text-only paste and reports rejected images", () => {
    render(<Composer {...baseProps} onPasteImages={() => {}} />);
    const textarea = screen.getByRole("textbox");
    const textPaste = fireEvent.paste(textarea, { clipboardData: pasteData([], "caption") });
    expect(textPaste).toBe(true);
    fireEvent.paste(textarea, { clipboardData: pasteData([new File(["x"], "image.bmp", { type: "image/bmp" })]) });
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't add image.bmp.");
  });
});
