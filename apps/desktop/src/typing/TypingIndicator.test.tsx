import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { expect, it } from "vitest";
import { TypingIndicator } from "./TypingIndicator";

it("hides the indicator and keeps its dots decorative", () => {
  const { container, rerender } = render(<TypingIndicator visible={false} label="Typing" />);
  expect(container.querySelector("#typing-indicator")).toHaveAttribute("hidden");
  rerender(<TypingIndicator visible label="Typing" />);
  expect(container.querySelector("#typing-indicator-dots")).toHaveAttribute("aria-hidden", "true");
});
