import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { ConversationRcsNotice } from "./ConversationRcsNotice";

describe("ConversationRcsNotice", () => {
  it("stays mounted and empty when there is no history gap", () => {
    const { container } = render(<ConversationRcsNotice />);
    const notice = container.querySelector("#conversation-rcs-notice")!;
    expect(notice).toHaveAttribute("role", "status");
    expect(notice).not.toHaveAttribute("data-rcs-notice");
    expect(notice).toBeEmptyDOMElement();
  });

  it("states the history gap without counts, dates or identifiers", () => {
    const { container } = render(<ConversationRcsNotice historyGap />);
    const notice = container.querySelector("#conversation-rcs-notice")!;
    expect(notice).toHaveAttribute("data-rcs-notice", "history-gap");
    expect(notice).toHaveTextContent(peppyCopy.rcs_history_gap_notice);
    expect(peppyCopy.rcs_history_gap_notice).not.toMatch(/\d/);
  });
});
