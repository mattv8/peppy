import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { RcsMessageMeta } from "./RcsMessageMeta";

describe("RcsMessageMeta", () => {
  it("renders only reported RCS outcome detail without upgrading submitted delivery", () => {
    const { rerender } = render(<RcsMessageMeta messageId="submitted" status="submitted" />);
    expect(screen.queryByText("Read")).not.toBeInTheDocument();
    expect(document.querySelector('[data-message-route-hint="submitted"]')).toBeNull();

    rerender(<RcsMessageMeta messageId="read" status="read" />);
    expect(screen.getByText("Read")).toHaveAttribute("data-message-read", "read");

    rerender(<RcsMessageMeta messageId="failed" status="failed-confirmed" />);
    expect(document.querySelector('[data-message-route-hint="failed"]')).toHaveAttribute("data-hint", "failed-reported");

    rerender(<RcsMessageMeta messageId="unknown" status="unknown" />);
    expect(document.querySelector('[data-message-route-hint="unknown"]')).toHaveAttribute("data-hint", "unknown");
  });
});
