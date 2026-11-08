import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { expect, it } from "vitest";
import { TypingIndicator } from "./TypingIndicator";

it("exposes its label, scope, title, and visibility state", () => {
  const { container, rerender } = render(<TypingIndicator visible={false} label="Someone is typing" scope="group" />);
  const indicator = container.querySelector("#typing-indicator")!;
  expect(indicator).toHaveAttribute("hidden");
  expect(indicator).toHaveAttribute("data-typing-scope", "group");
  expect(indicator).toHaveAttribute("data-typing-visible", "false");
  expect(indicator).toHaveAttribute("title", "Someone is typing");
  expect(container.querySelector("#typing-indicator-label")).toHaveTextContent("Someone is typing");
  rerender(<TypingIndicator visible label="Typing" />);
  expect(indicator).toHaveAttribute("data-typing-visible", "true");
  expect(container.querySelector("#typing-indicator-dots")).toHaveAttribute("aria-hidden", "true");
});
