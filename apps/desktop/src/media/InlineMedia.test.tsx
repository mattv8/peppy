import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { InlineMedia } from "./InlineMedia";

const attachment = (durationMs?: string) => ({ id: "audio-1", name: "Clip", mediaType: "audio/mpeg", byteSize: 1, state: "ready" as const, mediaMeta: durationMs === undefined ? undefined : { durationMs } });
const props = { requestOriginal: vi.fn(), checkOriginal: vi.fn() };

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("InlineMedia duration metadata", () => {
  it("formats finite metadata duration", () => {
    const { container } = render(<InlineMedia attachment={attachment("65000")} {...props} />);
    expect(container.querySelector("[data-media-duration]")).toHaveTextContent("1:05");
  });

  it.each([
    ["0", "0:00"],
    ["65999.9", "1:05"],
    ["3600000", "60:00"],
  ])("formats boundary metadata duration %s as %s", (durationMs, label) => {
    const { container } = render(<InlineMedia attachment={attachment(durationMs)} {...props} />);
    expect(container.querySelector("[data-media-duration]")).toHaveTextContent(label);
  });

  it.each([undefined, "", "abc", "NaN", "-1", "Infinity"])("hides invalid duration %s", (durationMs) => {
    const { container } = render(<InlineMedia attachment={attachment(durationMs)} {...props} />);
    expect(container.querySelector("[data-media-duration]")).toBeNull();
  });

  it("does not request a remote-only policy-removed recording", () => {
    const requestOriginal = vi.fn();
    render(<InlineMedia attachment={{ ...attachment(), remoteState: "removed_by_storage_policy", localAvailable: false }} requestOriginal={requestOriginal} checkOriginal={vi.fn()} />);

    expect(screen.getByText("This attachment was removed from hosted storage to make room. The message remains.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /play/i })).not.toBeInTheDocument();
    expect(requestOriginal).not.toHaveBeenCalled();
  });

  it("keeps a verified local policy-removed recording playable", () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready", url: "blob:local" });
    render(<InlineMedia attachment={{ ...attachment(), remoteState: "removed_by_storage_policy", localAvailable: true }} requestOriginal={requestOriginal} checkOriginal={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: /play audio: clip/i }));
    expect(requestOriginal).toHaveBeenCalledWith("audio-1");
  });
});
