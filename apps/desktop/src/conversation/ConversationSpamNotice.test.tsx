import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ConversationSpamNotice } from "./ConversationSpamNotice";

describe("ConversationSpamNotice", () => {
  afterEach(cleanup);
  it.each([
    [{ folder: "spam", folderReason: "blocked" }, "Blocked in your messaging app. New texts from this number don't arrive. Use Not spam to unblock it."],
    [{ folder: "spam", folderReason: "marked" }, "You marked this as spam. It sends no notifications."],
    [{ folder: "inbox", unblock: "pending" }, "Unblocking on your phone…"],
    [{ folder: "inbox", unblock: "failed" }, "Your phone couldn't unblock this number. Unblock it in your messaging app."],
  ] as const)("renders %s", (props, copy) => {
    render(<ConversationSpamNotice {...props} />);
    expect(document.getElementById("conversation-spam-notice")).toHaveTextContent(copy);
  });

  it("stays hidden outside the spam and unblock states", () => {
    render(<ConversationSpamNotice folder="inbox" />);
    expect(document.getElementById("conversation-spam-notice")).toBeEmptyDOMElement();
  });
});
