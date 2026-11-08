import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MmsSizeHelp } from "./MmsSizeHelp";

afterEach(() => cleanup());

describe("MmsSizeHelp", () => {
  it("opens the message-size details from its accessible control", () => {
    render(<MmsSizeHelp estimatedSize="248 KB" limitLabel="Carrier limit" limitSize="300 KB" recipientLimit={20} />);

    expect(screen.queryByRole("dialog", { name: "Message size" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "About message size" }));

    const dialog = screen.getByRole("dialog", { name: "Message size" });
    expect(dialog).toHaveTextContent("Estimated size");
    expect(dialog).toHaveTextContent("248 KB");
    expect(dialog).toHaveTextContent("Carrier limit");
    expect(dialog).toHaveTextContent("300 KB");
    expect(dialog).toHaveTextContent("up to 20");
  });

  it("returns focus to the info button after dismissal", () => {
    render(<MmsSizeHelp estimatedSize="248 KB" limitLabel="Default limit" limitSize="300 KB" recipientLimit={20} />);

    const info = screen.getByRole("button", { name: "About message size" });
    fireEvent.click(info);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    expect(screen.queryByRole("dialog", { name: "Message size" })).not.toBeInTheDocument();
    expect(document.activeElement).toBe(info);
  });

  it("contains Escape so it does not close the composer", () => {
    const closeComposer = vi.fn();
    window.addEventListener("keydown", closeComposer);
    try {
      render(<MmsSizeHelp estimatedSize="248 KB" limitLabel="Default limit" limitSize="300 KB" recipientLimit={20} />);
      fireEvent.click(screen.getByRole("button", { name: "About message size" }));
      fireEvent.keyDown(screen.getByRole("dialog", { name: "Message size" }), { key: "Escape" });

      expect(closeComposer).not.toHaveBeenCalled();
      expect(screen.queryByRole("dialog", { name: "Message size" })).not.toBeInTheDocument();
    } finally {
      window.removeEventListener("keydown", closeComposer);
    }
  });

  it("explains the resize and split plan when images require multiple messages", () => {
    render(<MmsSizeHelp estimatedSize="6.0 MB" limitLabel="Carrier limit" limitSize="300 KB" limitKilobytes={300} recipientLimit={20} resizingImages />);

    fireEvent.click(screen.getByRole("button", { name: "About message size" }));

    expect(screen.getByRole("dialog", { name: "Message size" })).toHaveTextContent("Images are resized to fit your carrier's MMS limit (300 KB here). When they still don't fit in one message, Peppy sends your text first, then the images in order.");
  });
});
