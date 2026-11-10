import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { GroupCreateNotRcsInfo, GroupCreateProgress, GroupCreateUnconfirmed, GroupUpdateRequiredNotice, GroupWithdrawnNotice } from "./GroupComposerNotices";
import { GroupMembersChangedNotice } from "./GroupMembersChangedNotice";
import { GroupNameField } from "./GroupNameField";
import { GroupNotice } from "./GroupNotice";

const displayName = (address: string) => ({ "+15551230001": "Alex", "+15551230002": "Sam" } as Record<string, string>)[address] ?? address;

afterEach(cleanup);

describe("GroupNotice", () => {
  it("renders a separator with the notice id and hooks, and isolates names in bdi", () => {
    const { container } = render(<GroupNotice notice={{ id: "group-notice-added-2", kind: "added", revision: "2", atMs: 1, addresses: ["+15551230001"] }} displayName={displayName} />);
    const notice = container.querySelector("#group-notice-added-2")!;
    expect(notice).toHaveClass("timeline-notice");
    expect(notice).toHaveAttribute("data-group-notice", "added");
    expect(notice).toHaveAttribute("data-group-notice-revision", "2");
    expect(notice.querySelector("bdi")).toHaveTextContent("Alex");
    expect(notice).toHaveTextContent("Alex was added");
    expect(notice).not.toHaveAttribute("title");
  });

  it("puts the full text in title when the name was shortened", () => {
    const long = "L".repeat(80);
    const { container } = render(<GroupNotice notice={{ id: "group-notice-name-2", kind: "name", revision: "2", atMs: 1, name: long, addresses: [] }} displayName={displayName} />);
    expect(container.querySelector("#group-notice-name-2")).toHaveAttribute("title", `Name changed to ${long}`);
  });

  it("is not a live status and sits in the log without a role of its own", () => {
    const { container } = render(<GroupNotice notice={{ id: "group-notice-removed-3", kind: "removed", revision: "3", atMs: 1, addresses: ["+15551230002"] }} displayName={displayName} />);
    expect(container.querySelector("#group-notice-removed-3")).not.toHaveAttribute("role");
    expect(container).toHaveTextContent("Sam is no longer in the group");
  });
});

describe("GroupNameField", () => {
  it("renders nothing for a one-to-one draft", () => {
    const { container } = render(<GroupNameField value="" onChange={vi.fn()} recipientCount={1} rcsLinked />);
    expect(container).toBeEmptyDOMElement();
  });

  it("offers a 100-character name row and the RCS help line when the link is live", () => {
    const { container } = render(<GroupNameField value="" onChange={vi.fn()} recipientCount={2} rcsLinked />);
    const input = container.querySelector<HTMLInputElement>("#new-group-name")!;
    expect(container.querySelector("#new-group-name-row")).toBeInTheDocument();
    expect(input).toHaveAttribute("maxLength", "100");
    expect(input).toHaveAttribute("aria-describedby", "new-group-help");
    expect(input).toHaveAttribute("autocomplete", "off");
    expect(input).toHaveAttribute("placeholder", peppyCopy.group_name_label);
    expect(screen.getByRole("textbox", { name: peppyCopy.group_name_label })).toBe(input);
    expect(container.querySelector("#new-group-help")).toHaveTextContent(peppyCopy.group_name_help_rcs);
    expect(container.querySelector("#new-group-help")).toHaveAttribute("data-group-name-help", "rcs");
  });

  it("uses the MMS help line when the link is not live", () => {
    const { container } = render(<GroupNameField value="" onChange={vi.fn()} recipientCount={3} rcsLinked={false} />);
    expect(container.querySelector("#new-group-help")).toHaveTextContent(peppyCopy.group_name_help_mms);
    expect(container.querySelector("#new-group-help")).toHaveAttribute("data-group-name-help", "mms");
  });

  it("passes the raw value up on every change", () => {
    const onChange = vi.fn();
    const { container } = render(<GroupNameField value="" onChange={onChange} recipientCount={2} rcsLinked />);
    fireEvent.change(container.querySelector("#new-group-name")!, { target: { value: "Weekend  crew" } });
    expect(onChange).toHaveBeenCalledWith("Weekend  crew");
  });
});

describe("GroupMembersChangedNotice", () => {
  const base = { memberRevision: "6", displayName, canResend: true, checking: false, onResend: vi.fn(), onReview: vi.fn() };

  it("is an alert with the refusal hooks and the new member list", () => {
    const { container } = render(<GroupMembersChangedNotice {...base} members={["+15551230001", "+15551230002"]} />);
    const region = container.querySelector("#composer-group-changed")!;
    expect(region).toHaveAttribute("role", "alert");
    expect(region).toHaveAttribute("data-group-send-refusal", "members-changed");
    expect(region).toHaveAttribute("data-member-revision", "6");
    expect(region).toHaveTextContent(peppyCopy.group_send_members_changed);
    expect(container.querySelector("#composer-group-changed-members")).toHaveTextContent("Now in this group: Alex, Sam.");
  });

  it("shows only the generic line until the metadata arrives", () => {
    const { container } = render(<GroupMembersChangedNotice {...base} />);
    expect(container.querySelector("#composer-group-changed")).toHaveTextContent(peppyCopy.group_send_members_changed);
    expect(container.querySelector("#composer-group-changed-members")).toBeNull();
  });

  it("summarises more than five members with a count of the rest", () => {
    const members = ["+15551230001", "+15551230002", "+15551230003", "+15551230004", "+15551230005", "+15551230006", "+15551230007"];
    const { container } = render(<GroupMembersChangedNotice {...base} members={members} />);
    expect(container.querySelector("#composer-group-changed-members")).toHaveTextContent("Alex, Sam, +15551230003, +15551230004, +15551230005 and 2 more");
  });

  it("resends to the updated group and reviews members through their callbacks", () => {
    const onResend = vi.fn();
    const onReview = vi.fn();
    const { container } = render(<GroupMembersChangedNotice {...base} onResend={onResend} onReview={onReview} />);
    fireEvent.click(container.querySelector("#group-send-updated")!);
    fireEvent.click(container.querySelector("#group-send-review")!);
    expect(onResend).toHaveBeenCalledTimes(1);
    expect(onReview).toHaveBeenCalledTimes(1);
  });

  it("holds the resend action while the group is being checked and does not fire it", () => {
    const onResend = vi.fn();
    const { container } = render(<GroupMembersChangedNotice {...base} checking onResend={onResend} />);
    const resend = container.querySelector("#group-send-updated")!;
    expect(resend).toHaveAttribute("aria-disabled", "true");
    expect(resend).toHaveTextContent(peppyCopy.group_send_checking);
    fireEvent.click(resend);
    expect(onResend).not.toHaveBeenCalled();
  });

  it("removes the resend action when the group can no longer take the send", () => {
    const { container } = render(<GroupMembersChangedNotice {...base} canResend={false} />);
    expect(container.querySelector("#group-send-updated")).toBeNull();
    expect(container.querySelector("#group-send-review")).not.toBeNull();
  });
});

describe("GroupComposerNotices", () => {
  it("shows the create progress as a status line", () => {
    const { container } = render(<GroupCreateProgress />);
    expect(container.querySelector("#composer-group-create-status")).toHaveAttribute("role", "status");
    expect(container).toHaveTextContent(peppyCopy.group_create_progress);
  });

  it("explains an unconfirmed create and calls back only on Create it again", () => {
    const onCreateAgain = vi.fn();
    const { container } = render(<GroupCreateUnconfirmed onCreateAgain={onCreateAgain} />);
    const region = container.querySelector("#composer-group-create-unconfirmed")!;
    expect(region).toHaveAttribute("role", "alert");
    expect(region).toHaveAttribute("data-group-create", "unconfirmed");
    expect(region).toHaveTextContent(peppyCopy.group_create_again_confirm);
    expect(onCreateAgain).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.group_create_again_action }));
    expect(onCreateAgain).toHaveBeenCalledTimes(1);
  });

  it("reports an MMS fallback that kept the name only in Peppy", () => {
    const { container } = render(<GroupCreateNotRcsInfo />);
    expect(container.querySelector("#composer-group-create-not-rcs")).toHaveTextContent("Created as a group MMS. The name is only visible in Peppy.");
  });

  it("tells a stale device to update, without a retry", () => {
    const { container } = render(<GroupUpdateRequiredNotice />);
    expect(container.querySelector("#composer-group-update-required")).toHaveAttribute("data-group-send-refusal", "update-required");
    expect(container.querySelector("button")).toBeNull();
  });

  it("offers a new MMS group for a withdrawn group", () => {
    const onStartNewMms = vi.fn();
    const { container } = render(<GroupWithdrawnNotice onStartNewMms={onStartNewMms} />);
    expect(container.querySelector("#composer-group-withdrawn")).toHaveAttribute("data-group-send-refusal", "withdrawn");
    fireEvent.click(container.querySelector("#group-start-new-mms")!);
    expect(onStartNewMms).toHaveBeenCalledTimes(1);
  });
});
