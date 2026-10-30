import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { measureComposerChrome } from "./composerChrome";

const styleOverrides = new Map<Element, Partial<CSSStyleDeclaration>>();

const DEFAULT_STYLE = {
  display: "block",
  position: "static",
  flexDirection: "column",
  rowGap: "0px",
  paddingTop: "0px",
  paddingBottom: "0px",
  borderTopWidth: "0px",
  borderBottomWidth: "0px",
  marginTop: "0px",
  marginBottom: "0px",
};

function mockElement(
  parent: HTMLElement | null,
  tag: string,
  id: string,
  height: number,
  style: Partial<CSSStyleDeclaration> = {},
): HTMLElement {
  const element = document.createElement(tag);
  element.id = id;
  Object.defineProperty(element, "offsetHeight", { configurable: true, get: () => height });
  styleOverrides.set(element, style);
  parent?.append(element);
  return element;
}

interface ComposerOptions {
  areaHeight?: number;
  textareaHeight?: number;
  status?: { height: number; hidden?: boolean };
  banner?: { height: number; hidden?: boolean };
  attachments?: boolean;
  rowSibling?: boolean;
  grip?: boolean;
  fieldGap?: string;
}

function buildComposer(options: ComposerOptions = {}) {
  const area = mockElement(document.body, "section", "composer-area", options.areaHeight ?? 96, {
    paddingTop: "8px",
    paddingBottom: "8px",
  });
  const form = mockElement(area, "form", "shared-composer", 0, { display: "flex" });
  const field = mockElement(form, "div", "composer-field", 0, {
    display: "flex",
    flexDirection: "column",
    rowGap: options.fieldGap ?? "0px",
    borderTopWidth: "2px",
    borderBottomWidth: "2px",
  });
  if (options.banner) {
    const banner = mockElement(field, "div", "composer-banner-row", options.banner.height);
    banner.hidden = options.banner.hidden ?? false;
  }
  if (options.attachments) {
    mockElement(field, "ul", "attachment-tray", 30, { marginBottom: "4px" });
  }
  const row = mockElement(field, "div", "composer-input-row", 0, {
    display: "flex",
    flexDirection: "row",
    paddingTop: "10px",
    paddingBottom: "4px",
  });
  if (options.rowSibling) mockElement(row, "span", "row-sibling", 50);
  const textarea = mockElement(row, "textarea", "composer-textarea", options.textareaHeight ?? 22);
  if (options.status) {
    const status = mockElement(field, "div", "composer-status-row", options.status.height);
    status.hidden = options.status.hidden ?? false;
  }
  mockElement(field, "div", "composer-toolbar", 44);
  if (options.grip) {
    mockElement(area, "div", "composer-resize-grip", 12, { position: "absolute" });
  }
  return { area, textarea };
}

describe("measureComposerChrome", () => {
  beforeEach(() => {
    styleOverrides.clear();
    vi.spyOn(globalThis, "getComputedStyle").mockImplementation(
      (element: Element) =>
        ({ ...DEFAULT_STYLE, ...styleOverrides.get(element) }) as unknown as CSSStyleDeclaration,
    );
  });

  afterEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = "";
  });

  it("counts a two-line status row in a 96px clipped composer instead of trusting the outer height", () => {
    const { area, textarea } = buildComposer({ status: { height: 36 } });

    // 14 input row + 36 status + 44 toolbar + 4 field border + 16 area padding
    expect(measureComposerChrome(area, textarea)).toBe(114);
    expect(area.offsetHeight - textarea.offsetHeight).toBe(74);
  });

  it("omits a hidden status row", () => {
    const { area, textarea } = buildComposer({ status: { height: 36, hidden: true } });

    expect(measureComposerChrome(area, textarea)).toBe(78);
  });

  it("includes the attachment tray with its margin", () => {
    const { area, textarea } = buildComposer({ attachments: true });

    // 78 base + 30 tray + 4 tray margin
    expect(measureComposerChrome(area, textarea)).toBe(112);
  });

  it("counts a visible notice banner row", () => {
    const { area, textarea } = buildComposer({ banner: { height: 24 } });

    expect(measureComposerChrome(area, textarea)).toBe(102);
  });

  it("excludes a hidden notice banner row", () => {
    const { area, textarea } = buildComposer({ banner: { height: 24, hidden: true } });

    expect(measureComposerChrome(area, textarea)).toBe(78);
  });

  it("adds column row gaps between stacked children", () => {
    const { area, textarea } = buildComposer({ status: { height: 36 }, fieldGap: "4px" });

    // 114 base + 2 gaps between input row, status, and toolbar
    expect(measureComposerChrome(area, textarea)).toBe(122);
  });

  it("does not add row-flex siblings of the textarea vertically", () => {
    const { area, textarea } = buildComposer({ rowSibling: true });

    expect(measureComposerChrome(area, textarea)).toBe(78);
  });

  it("ignores an absolutely positioned resize handle", () => {
    const { area, textarea } = buildComposer({ grip: true });

    expect(measureComposerChrome(area, textarea)).toBe(78);
  });

  it("does not count the textarea's own height", () => {
    const { area, textarea } = buildComposer({ textareaHeight: 200 });

    expect(measureComposerChrome(area, textarea)).toBe(78);
  });

  it("returns a finite nonnegative value when the textarea is outside the area", () => {
    const area = mockElement(document.body, "section", "composer-area", 96);
    const outside = mockElement(document.body, "div", "outside", 0);
    const textarea = mockElement(outside, "textarea", "composer-textarea", 22);

    const chrome = measureComposerChrome(area, textarea);

    expect(Number.isFinite(chrome)).toBe(true);
    expect(chrome).toBe(74);
  });
});
