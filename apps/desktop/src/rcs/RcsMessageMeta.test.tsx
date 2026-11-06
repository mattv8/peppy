import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import type { MessageStatus } from "../bridge/bridge";
import { RcsMessageMeta } from "./RcsMessageMeta";

describe("RcsMessageMeta", () => {
  it("does not infer delivery from submitted and renders the reported RCS outcomes", () => {
    const { rerender, container } = render(<RcsMessageMeta messageId="submitted" status="submitted" />);
    expect(container).toBeEmptyDOMElement();

    rerender(<RcsMessageMeta messageId="sent" status="sent" />);
    expect(container).toBeEmptyDOMElement();

    rerender(<RcsMessageMeta messageId="delivered" status="delivery-confirmed" />);
    expect(container).toBeEmptyDOMElement();

    rerender(<RcsMessageMeta messageId="read" status="read" />);
    expect(screen.getByText("Read")).toHaveAttribute("data-message-read", "read");

    rerender(<RcsMessageMeta messageId="failed" status="failed-confirmed" rcsReported />);
    expect(document.querySelector('[data-message-route-hint="failed"]')).toHaveAttribute("data-hint", "failed-reported");

    rerender(<RcsMessageMeta messageId="unknown" status="unknown" />);
    expect(document.querySelector('[data-message-route-hint="unknown"]')).toHaveAttribute("data-hint", "unknown");
  });

  it("gives a failure Google did not report the neutral hint, never the Google attribution", () => {
    const { rerender } = render(<RcsMessageMeta messageId="send-state" status="failed-confirmed" rcsReported={false} />);
    const hint = document.querySelector('[data-message-route-hint="send-state"]');
    expect(hint).toHaveAttribute("data-hint", "failed");
    expect(hint).toHaveTextContent(peppyCopy.rcs_message_failed_unreported);
    expect(hint).not.toHaveTextContent(peppyCopy.rcs_message_failed_reported);

    rerender(<RcsMessageMeta messageId="no-flag" status="failed-confirmed" />);
    expect(document.querySelector('[data-message-route-hint="no-flag"]')).toHaveAttribute("data-hint", "failed");
  });

  it("renders nothing for a status it does not know", () => {
    const { container } = render(<RcsMessageMeta messageId="future" status={"future-state" as MessageStatus} />);
    expect(container).toBeEmptyDOMElement();
  });
});
