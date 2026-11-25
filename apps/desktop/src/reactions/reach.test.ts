import { afterEach, describe, expect, it, vi } from "vitest";
import { measureReach } from "./ReactionLayer";

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
    rect(element.firstElementChild!, 100, 180);
    rect(element.lastElementChild!, 182, 220);
    expect(measureReach(element)).toEqual({ top: 16, bottom: 0 });
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
