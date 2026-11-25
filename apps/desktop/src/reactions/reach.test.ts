import { afterEach, describe, expect, it, vi } from "vitest";
import { measureReach, reachBox } from "./ReactionLayer";
import { placeReactionBar } from "./placement";

function rect(element: Element, top: number, bottom: number, width = 100) {
  vi.spyOn(element, "getBoundingClientRect").mockReturnValue({
    x: 0, y: top, top, bottom, left: 0, right: width, width, height: bottom - top, toJSON: () => ({}),
  });
}

function anchor(markup = "") {
  const element = document.createElement("div");
  element.innerHTML = markup;
  rect(element, 100, 200);
  vi.spyOn(window, "getComputedStyle").mockReturnValue({
    borderTopWidth: "1px", borderBottomWidth: "1px", paddingTop: "10px", paddingBottom: "10px",
  } as CSSStyleDeclaration);
  return element;
}

afterEach(() => vi.restoreAllMocks());

describe("measureReach", () => {
  it("uses a text bubble's border and padding", () => {
    expect(measureReach(anchor())).toEqual({ top: 11, bottom: 11 });
  });

  it.each([
    ["link", '<a href="https://example.com">link</a>'],
    ["cancel button", '<button data-message-cancel>cancel</button>'],
  ])("limits text reach near a %s", (_name, markup) => {
    const element = anchor(markup);
    rect(element.firstElementChild!, 104, 150);
    expect(measureReach(element)).toEqual({ top: 4, bottom: 11 });
  });

  it("skips a zero-size control", () => {
    const element = anchor('<a href="https://example.com">link</a>');
    rect(element.firstElementChild!, 100, 100, 0);
    expect(measureReach(element)).toEqual({ top: 11, bottom: 11 });
  });

  it("does not limit photo reach for a tile button", () => {
    const element = anchor("<button>tile</button>");
    element.setAttribute("data-media-message-content", "");
    rect(element.firstElementChild!, 100, 200);
    expect(measureReach(element)).toEqual({ top: 16, bottom: 16 });
  });

  it("measures photo reach against the frame and keeps its caption clear", () => {
    const element = anchor('<div data-media-frame></div><div data-message-caption>caption</div>');
    element.setAttribute("data-media-message-content", "");
    rect(element, 100, 220);
    rect(element.firstElementChild!, 100, 180);
    rect(element.lastElementChild!, 182, 220);
    const box = reachBox(element);
    const reach = measureReach(element);
    expect(box).toEqual({ top: 100, bottom: 220, left: 0, right: 100 });
    expect(reach).toEqual({ top: 16, bottom: 0 });
    const placement = placeReactionBar({ anchor: box, reach, outgoing: false, list: { top: 95, bottom: 800, left: 0, right: 400 }, composerTop: 700, viewport: { top: 0, bottom: 800 }, width: 212, height: 32, touch: false, allowCover: false });
    expect(placement?.placement).toBe("below");
    expect(placement!.top).toBeGreaterThanOrEqual(220);
  });

  it("limits chromeless reach to keep a caption clear", () => {
    const element = anchor("<span data-message-caption>caption</span>");
    element.className = "message-bubble";
    element.setAttribute("data-chromeless", "");
    rect(element.firstElementChild!, 150, 198);
    expect(measureReach(element)).toEqual({ top: 16, bottom: 2 });
  });

  it("limits chromeless reach to keep a button clear", () => {
    const element = anchor("<button>code</button>");
    element.className = "message-bubble";
    element.setAttribute("data-chromeless", "");
    rect(element.firstElementChild!, 104, 150);
    expect(measureReach(element)).toEqual({ top: 4, bottom: 16 });
  });
});
