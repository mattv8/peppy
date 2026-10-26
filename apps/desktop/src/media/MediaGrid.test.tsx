import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaGrid } from "./MediaGrid";
import { MessageText } from "../message-links";

const message = (id: string, body = "") => ({ id, revision: "1", sender: "self" as const, body, timestamp: `2026-01-01T12:0${id}:00.000Z`, attachments: [{ id: `${id}-image`, name: id, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, mediaMeta: { thumbnail: { id: `${id}-thumbnail`, name: `${id}-thumbnail`, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, previewUrl: "thumb" } } }] });

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("MediaGrid", () => {
  it("renders every caption and keeps a single message-row hook per message", () => {
    const view = render(<MediaGrid messages={[message("1", "first"), message("2", "final")]} onOpen={vi.fn()} renderMessageMeta={(value) => value.id} />);
    expect(view.container.querySelectorAll("[data-message-meta]")).toHaveLength(2);
    expect(screen.getByText("first")).toBeInTheDocument();
    expect(screen.getByText("final")).toBeInTheDocument();
    expect(view.container.querySelectorAll('[data-message-row="1"]')).toHaveLength(1);
    expect(view.container.querySelector('[data-message-group="1"]')).toBeInTheDocument();
  });

  it("uses the supplied renderer for media captions", () => {
    render(<MediaGrid messages={[message("1", "Read https://peppy.pro/docs")]} onOpen={vi.fn()} renderCaption={(value) => <MessageText text={value.body} />} />);
    expect(screen.getByRole("link", { name: "https://peppy.pro/docs" })).toHaveAttribute("href", "https://peppy.pro/docs");
  });

  it("renders a single tile without carousel controls", () => {
    const view = render(<MediaGrid messages={[message("1")]} onOpen={vi.fn()} />);
    expect(view.container.querySelector("[data-media-carousel]")).toBeNull();
    expect(view.container.querySelector("[data-media-tile][data-variant='single']")).toBeInTheDocument();
  });

  it("renders multiple attachments as a carousel and moves slide focus with arrow keys", () => {
    const first = message("1");
    const second = message("2");
    const view = render(<MediaGrid messages={[first, second]} onOpen={vi.fn()} />);
    expect(view.container.querySelector("[data-media-carousel]")).toHaveAttribute("aria-label", "Images, 2 total");
    expect(screen.getByText("1 / 2")).toBeInTheDocument();
    const firstBody = screen.getByRole("button", { name: "Photo, 1 of 2" });
    const secondBody = screen.getByRole("button", { name: "Photo, 2 of 2" });
    firstBody.focus();
    fireEvent.keyDown(firstBody, { key: "ArrowRight" });
    expect(secondBody).toHaveFocus();
    expect(view.container.querySelectorAll("[data-message-meta]")).toHaveLength(2);
  });

  it("renders chevrons for a scrollable two-slide carousel", async () => {
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { callback(0); return 0; });
    const view = render(<MediaGrid messages={[message("1"), message("2")]} onOpen={vi.fn()} />);
    const track = view.container.querySelector<HTMLDivElement>(".carousel-track")!;
    Object.defineProperties(track, { clientWidth: { configurable: true, value: 200 }, scrollWidth: { configurable: true, value: 406 }, scrollLeft: { configurable: true, value: 0 } });
    fireEvent.scroll(track);
    await waitFor(() => expect(screen.getByRole("button", { name: "Next images" })).toBeInTheDocument());
  });

  it("uses the first half-visible slide for the counter", () => {
    let notify!: IntersectionObserverCallback;
    vi.stubGlobal("IntersectionObserver", class {
      constructor(callback: IntersectionObserverCallback) { notify = callback; }
      observe() {}
      disconnect() {}
    });
    const view = render(<MediaGrid messages={[message("1"), message("2"), message("3")]} onOpen={vi.fn()} />);
    const slides = view.container.querySelectorAll("[data-media-slide]");
    act(() => notify([{ target: slides[1], isIntersecting: true, intersectionRatio: .5 } as IntersectionObserverEntry, { target: slides[2], isIntersecting: true, intersectionRatio: .5 } as IntersectionObserverEntry], {} as IntersectionObserver));
    expect(screen.getByText("2 / 3")).toBeInTheDocument();
  });

  it("skips failed slides during arrow-key navigation", () => {
    const failedAttachment = message("2").attachments[0];
    const failed = { ...message("2"), attachments: [{ ...failedAttachment, state: "failed" as const, mediaMeta: { thumbnail: { ...failedAttachment.mediaMeta.thumbnail, state: "failed" as const } } }] };
    render(<MediaGrid messages={[message("1"), failed, message("3")]} onOpen={vi.fn()} />);
    const firstBody = screen.getByRole("button", { name: "Photo, 1 of 3" });
    const thirdBody = screen.getByRole("button", { name: "Photo, 3 of 3" });
    firstBody.focus();
    fireEvent.keyDown(firstBody, { key: "ArrowRight" });
    expect(thirdBody).toHaveFocus();
  });
});
