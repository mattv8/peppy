import "@testing-library/jest-dom/vitest";
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { IsolatedText } from "./IsolatedText";

afterEach(cleanup);

describe("IsolatedText", () => {
  it.each([
    ["plain ASCII", "notes.pdf"],
    ["an unmatched RLO", "a\u202Etxt"],
    ["RTL text", "محمد"],
  ])("preserves %s without changing its inline layout", (_description, text) => {
    const { container } = render(<IsolatedText text={text} />);
    const name = container.querySelector("bdi");

    expect(name).toHaveTextContent(text);
    expect(name).not.toHaveAttribute("style");
  });

  it.each([
    ["PDI", "a\u2069\u202Etxt"],
    ["LF", "a\u000A\u202Etxt"],
    ["CR", "a\u000D\u202Etxt"],
    ["U+001C", "a\u001C\u202Etxt"],
    ["U+001D", "a\u001D\u202Etxt"],
    ["U+001E", "a\u001E\u202Etxt"],
    ["U+0085", "a\u0085\u202Etxt"],
    ["U+2029", "a\u2029\u202Etxt"],
  ])("contains %s and RLO text without changing its characters", (_breaker, text) => {
    const { container } = render(<IsolatedText text={text} />);
    const name = container.querySelector("bdi");

    expect(name).toHaveStyle({ display: "inline-block" });
    expect(name?.textContent).toBe(text);
  });
});
