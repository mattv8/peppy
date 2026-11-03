import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { InlineMedia } from "./InlineMedia";

const attachment = (durationMs?: string) => ({ id: "audio-1", name: "Clip", mediaType: "audio/mpeg", byteSize: 1, state: "ready" as const, mediaMeta: durationMs === undefined ? undefined : { durationMs } });
const videoAttachment = (overrides = {}) => ({ id: "video-1", name: "Clip", mediaType: "video/mp4", byteSize: 1, state: "ready" as const, ...overrides });
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

  it("marks audio playback with its media kind", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready", url: "blob:audio" });
    render(<InlineMedia attachment={attachment()} requestOriginal={requestOriginal} checkOriginal={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: /play audio: clip/i }));

    expect(await screen.findByLabelText("Clip")).toHaveAttribute("data-media-kind", "audio");
  });

  it("uses a plain button for an audio placeholder", () => {
    const { container } = render(<InlineMedia attachment={attachment()} requestOriginal={vi.fn()} checkOriginal={vi.fn()} />);

    const button = screen.getByRole("button", { name: /play audio: clip/i });
    expect(button).toHaveClass("secondary-button");
    expect(button).not.toHaveClass("media-play-trigger");
    expect(container.querySelector('[data-media-kind="audio"] > .media-play-trigger')).toBeNull();
  });

  it("keeps every video state on the shared frame hooks", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready", url: "blob:video" });
    const { rerender } = render(<InlineMedia attachment={videoAttachment()} requestOriginal={requestOriginal} checkOriginal={vi.fn()} />);

    const placeholder = screen.getByRole("button", { name: /play video: clip/i }).parentElement!;
    expect(placeholder).toHaveAttribute("data-inline-media");
    expect(placeholder).toHaveAttribute("data-media-kind", "video");
    expect(placeholder).toHaveAttribute("data-attachment-id", "video-1");

    const playButton = screen.getByRole("button", { name: /play video: clip/i });
    playButton.focus();
    fireEvent.click(playButton);
    const video = await screen.findByLabelText("Clip");
    expect(video).toHaveAttribute("data-inline-media");
    expect(video).toHaveAttribute("data-media-kind", "video");
    expect(video).toHaveAttribute("data-attachment-id", "video-1");
    expect(video).toHaveFocus();

    rerender(<InlineMedia attachment={videoAttachment({ remoteState: "removed_by_storage_policy", localAvailable: false })} requestOriginal={vi.fn()} checkOriginal={vi.fn()} />);
    const removed = screen.getByRole("status");
    expect(removed).toHaveTextContent("This attachment was removed from hosted storage to make room. The message remains.");
    expect(removed).toHaveAttribute("data-inline-media");
    expect(removed).toHaveAttribute("data-media-kind", "video");
    expect(removed).toHaveAttribute("data-attachment-id", "video-1");
    expect(removed).toHaveAttribute("data-attachment-storage-removed");
  });

  it("reconciles the frame ratio after video metadata loads", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready", url: "blob:video" });
    render(<InlineMedia attachment={videoAttachment()} requestOriginal={requestOriginal} checkOriginal={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: /play video: clip/i }));
    const video = await screen.findByLabelText("Clip");
    Object.defineProperties(video, { videoWidth: { value: 1080 }, videoHeight: { value: 1920 } });
    fireEvent.loadedMetadata(video);

    expect(video).toHaveStyle({ "--video-ar": "1080 / 1920" });
  });

  it("does not override a frame ratio within one percent", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready", url: "blob:video" });
    render(<InlineMedia attachment={videoAttachment()} requestOriginal={requestOriginal} checkOriginal={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: /play video: clip/i }));
    const video = await screen.findByLabelText("Clip");
    Object.defineProperties(video, { videoWidth: { value: 1616 }, videoHeight: { value: 900 } });
    fireEvent.loadedMetadata(video);

    expect(video.style.getPropertyValue("--video-ar")).toBe("");
  });

  it("ignores video metadata with invalid dimensions", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready", url: "blob:video" });
    render(<InlineMedia attachment={videoAttachment()} requestOriginal={requestOriginal} checkOriginal={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: /play video: clip/i }));
    const video = await screen.findByLabelText("Clip");
    Object.defineProperties(video, { videoWidth: { value: 0 }, videoHeight: { value: 1080 } });
    fireEvent.loadedMetadata(video);

    expect(video.style.getPropertyValue("--video-ar")).toBe("");
  });

  it("does not move focus after mouse activation", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready", url: "blob:video" });
    render(<InlineMedia attachment={videoAttachment()} requestOriginal={requestOriginal} checkOriginal={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: /play video: clip/i }));

    expect(await screen.findByLabelText("Clip")).not.toHaveFocus();
  });

  it("does not move focus when a pending request later becomes ready", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "pending" });
    const checkOriginal = vi.fn().mockResolvedValue({ state: "ready", url: "blob:video" });
    const { rerender } = render(<InlineMedia attachment={videoAttachment()} requestOriginal={requestOriginal} checkOriginal={checkOriginal} retryKey={0} />);

    const play = screen.getByRole("button", { name: /play video: clip/i });
    play.focus();
    fireEvent.click(play);
    await waitFor(() => expect(screen.getByRole("button", { name: /downloading video: clip/i })).toHaveAttribute("aria-busy", "true"));
    rerender(<InlineMedia attachment={videoAttachment()} requestOriginal={requestOriginal} checkOriginal={checkOriginal} retryKey={1} />);

    expect(await screen.findByLabelText("Clip")).not.toHaveFocus();
  });
});
