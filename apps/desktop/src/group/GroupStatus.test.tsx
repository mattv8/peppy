import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { ConversationRcsNotice } from "../rcs/ConversationRcsNotice";
import { MmsGroupBadge, RcsBadge } from "../rcs/RcsBadge";
import { RcsMessageMeta } from "../rcs/RcsMessageMeta";

afterEach(cleanup);

describe("group header badges", () => {
  it("marks an MMS group with a visible MMS label and a full accessible sentence", () => {
    const { container } = render(<MmsGroupBadge />);
    const badge = container.querySelector("#conversation-mms-badge")!;
    expect(badge).toHaveAttribute("data-rcs-badge", "mms");
    expect(badge).toHaveAttribute("title", peppyCopy.group_badge_mms_description);
    expect(badge.querySelector("[aria-hidden='true']")).toHaveTextContent("MMS");
    expect(badge.querySelector(".visually-hidden")).toHaveTextContent(`MMS. ${peppyCopy.group_badge_mms_description}`);
    expect(badge.closest("[role=status], [aria-live]")).toBeNull();
  });

  it("keeps the RCS badge for a paused group and marks it paused", () => {
    const { container } = render(<RcsBadge paused />);
    expect(container.querySelector("#conversation-rcs-badge")).toHaveAttribute("data-paused", "true");
  });

  it("does not mark a live group as paused", () => {
    const { container } = render(<RcsBadge />);
    expect(container.querySelector("#conversation-rcs-badge")).not.toHaveAttribute("data-paused");
  });
});

describe("ConversationRcsNotice group states", () => {
  it("says a group was left, which outranks a history gap", () => {
    const { container } = render(<ConversationRcsNotice historyGap group={{ state: "left" }} />);
    const notice = container.querySelector("#conversation-rcs-notice")!;
    expect(notice).toHaveAttribute("data-rcs-notice", "group-left");
    expect(notice).toHaveTextContent(peppyCopy.group_notice_left);
  });

  it("says members are being checked while a group is paused, outranking a history gap", () => {
    const { container } = render(<ConversationRcsNotice historyGap group={{ state: "paused" }} />);
    const notice = container.querySelector("#conversation-rcs-notice")!;
    expect(notice).toHaveAttribute("data-rcs-notice", "group-paused");
    expect(notice).toHaveTextContent(peppyCopy.group_notice_paused);
  });

  it("stays empty for a live or MMS group with no history gap", () => {
    for (const state of ["live", "mms"] as const) {
      const { container, unmount } = render(<ConversationRcsNotice group={{ state }} />);
      expect(container.querySelector("#conversation-rcs-notice")).toBeEmptyDOMElement();
      expect(container.querySelector("#conversation-rcs-notice")).not.toHaveAttribute("data-rcs-notice");
      unmount();
    }
  });

  it("falls back to the history gap when the group is live", () => {
    const { container } = render(<ConversationRcsNotice historyGap group={{ state: "live" }} />);
    expect(container.querySelector("#conversation-rcs-notice")).toHaveAttribute("data-rcs-notice", "history-gap");
  });
});

describe("RcsMessageMeta group hints", () => {
  it("explains an unknown create and offers Create it again only as an explicit press", () => {
    const onCreateAgain = vi.fn();
    const { container } = render(<RcsMessageMeta messageId="m1" status="unknown" statusLabel="Unknown delivery state" groupHint="create_unknown" onCreateAgain={onCreateAgain} />);
    const hint = container.querySelector("[data-hint='group-create-unknown']")!;
    expect(hint).toHaveAttribute("data-message-route-hint", "m1");
    expect(hint).toHaveTextContent(peppyCopy.group_create_unconfirmed);
    expect(onCreateAgain).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.group_create_again_action }));
    expect(onCreateAgain).toHaveBeenCalledTimes(1);
  });

  it("omits the create-again action when no callback is given", () => {
    const { container } = render(<RcsMessageMeta messageId="m1b" status="unknown" statusLabel="Unknown delivery state" groupHint="create_unknown" />);
    expect(container.querySelector("[data-hint='group-create-unknown']")).toHaveTextContent(peppyCopy.group_create_unconfirmed);
    expect(container.querySelector("button")).toBeNull();
  });

  it.each([
    ["update_required", peppyCopy.group_update_required],
    ["withdrawn", peppyCopy.group_withdrawn_notice],
    ["create_not_rcs", peppyCopy.group_create_not_rcs_info],
  ] as const)("shows the %s message hint as informational text with no action", (groupHint, copy) => {
    const { container } = render(<RcsMessageMeta messageId="m4" status="sent" statusLabel="Sent" groupHint={groupHint} onEditResend={vi.fn()} onCreateAgain={vi.fn()} />);
    const hint = container.querySelector(`[data-message-route-hint='m4'][data-hint^='group-']`)!;
    expect(hint).toHaveTextContent(copy);
    expect(container.querySelector("button")).toBeNull();
  });

  it("offers Edit and send again for a members-changed refusal, and calls back only on press", () => {
    const onEditResend = vi.fn();
    const { container } = render(<RcsMessageMeta messageId="m2" status="failed-before-submit" statusLabel="Failed before submit" groupHint="members_changed" onEditResend={onEditResend} />);
    const hint = container.querySelector("[data-hint='group-members-changed']")!;
    expect(hint).toHaveTextContent(peppyCopy.group_message_members_changed_hint);
    expect(onEditResend).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.group_message_edit_resend }));
    expect(onEditResend).toHaveBeenCalledTimes(1);
    expect(container.querySelector("#group-message-edit-m2")).toBeInTheDocument();
  });

  it("omits the edit action when no callback is given", () => {
    const { container } = render(<RcsMessageMeta messageId="m3" status="failed-before-submit" statusLabel="Failed before submit" groupHint="members_changed" />);
    expect(container.querySelector("#group-message-edit-m3")).toBeNull();
  });
});
