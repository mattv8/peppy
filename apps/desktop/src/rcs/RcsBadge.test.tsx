import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { expect, it } from "vitest";
import { RcsBadge } from "./RcsBadge";

it("exposes the current route through visible and accessible badge text", () => {
  const { container } = render(<RcsBadge />);
  const badge = container.querySelector("#conversation-rcs-badge")!;

  expect(badge).toHaveAttribute("data-rcs-badge", "current");
  expect(badge).toHaveAttribute("title", "New messages in this chat are sent through Google Messages as RCS.");
  expect(badge).toHaveTextContent("RCS. New messages in this chat are sent through Google Messages as RCS.");
  expect(badge.closest("[role=status], [aria-live]")).toBeNull();
});
