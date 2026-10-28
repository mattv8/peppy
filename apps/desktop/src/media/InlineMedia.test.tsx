import "@testing-library/jest-dom/vitest";
import { cleanup, render } from "@testing-library/react";
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
});
