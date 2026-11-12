import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { GroupNameField } from "./GroupNameField";
import { GroupNotice } from "./GroupNotice";
import { groupComposerStatusItems, type GroupComposerStatusInput } from "./groupComposerStatus";

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

describe("groupComposerStatusItems", () => {
  const base = (): GroupComposerStatusInput => ({
    creating: false,
    notice: null,
    groupLeft: false,
    displayName,
    canResend: true,
    checking: false,
    onResend: vi.fn(),
    onReview: vi.fn(),
    onCreateAgain: vi.fn(),
    onStartNewMms: vi.fn(),
  });

  it("builds the members-changed refusal with its detail and actions", () => {
    const input = { ...base(), notice: { kind: "members-changed" as const, memberRevision: "6", members: ["+15551230001", "+15551230002"] } };
    const [item] = groupComposerStatusItems(input);
    expect(item).toMatchObject({
      key: "group-members-changed",
      id: "composer-group-changed",
      tone: "error",
      role: "alert",
      message: peppyCopy.composer_status_group_changed,
      detail: peppyCopy.composer_status_group_changed_detail,
      detailItems: ["Now in this group: Alex, Sam."],
      hooks: { "data-group-send-refusal": "members-changed", "data-member-revision": "6" },
    });
    expect(item.actions?.map(action => action.id)).toEqual(["group-send-updated", "group-send-review"]);
  });

  it("holds or hides resend while preserving review", () => {
    const checking = groupComposerStatusItems({ ...base(), checking: true, notice: { kind: "members-changed", memberRevision: "6" } })[0];
    expect(checking.actions?.[0]).toMatchObject({ id: "group-send-updated", label: peppyCopy.composer_status_group_checking, disabled: true });

    const cannotResend = groupComposerStatusItems({ ...base(), canResend: false, notice: { kind: "members-changed", memberRevision: "6" } })[0];
    expect(cannotResend.actions?.map(action => action.id)).toEqual(["group-send-review"]);
  });

  it("omits unknown members and summarizes large member lists", () => {
    const unknownMembers = groupComposerStatusItems({ ...base(), notice: { kind: "members-changed", memberRevision: "6" } })[0];
    expect(unknownMembers.detailItems).toBeUndefined();

    const members = ["+15551230001", "+15551230002", "+15551230003", "+15551230004", "+15551230005", "+15551230006", "+15551230007"];
    const item = groupComposerStatusItems({ ...base(), notice: { kind: "members-changed", memberRevision: "6", members } })[0];
    expect(item.detailItems).toEqual(["Now in this group: Alex, Sam, +15551230003, +15551230004, +15551230005 and 2 more."]);
  });

  it("keeps legacy hooks and action ids for the remaining statuses", () => {
    const createAgain = vi.fn();
    const unconfirmed = groupComposerStatusItems({ ...base(), onCreateAgain: createAgain, notice: { kind: "create-unconfirmed" } })[0];
    expect(unconfirmed).toMatchObject({ key: "group-create-unconfirmed", id: "composer-group-create-unconfirmed", tone: "warning", role: "alert", hooks: { "data-group-create": "unconfirmed" } });
    expect(unconfirmed.actions?.[0]).toMatchObject({ id: "group-create-again", confirm: { confirmLabel: peppyCopy.composer_status_create_again } });
    unconfirmed.actions?.[0]?.onAction();
    expect(createAgain).toHaveBeenCalledTimes(1);

    const startNewMms = vi.fn();
    const withdrawn = groupComposerStatusItems({ ...base(), onStartNewMms: startNewMms, notice: { kind: "withdrawn" } })[0];
    expect(withdrawn).toMatchObject({ key: "group-withdrawn", id: "composer-group-withdrawn", hooks: { "data-group-send-refusal": "withdrawn" } });
    expect(withdrawn.actions?.[0].id).toBe("group-start-new-mms");
    withdrawn.actions?.[0]?.onAction();
    expect(startNewMms).toHaveBeenCalledTimes(1);
  });

  it("orders action-required statuses before blockers and creating progress", () => {
    const items = groupComposerStatusItems({ ...base(), creating: true, groupLeft: true, notice: { kind: "members-changed", memberRevision: "6" } });
    expect(items.map(item => item.key)).toEqual(["group-members-changed", "group-left", "group-creating"]);
    expect(items[1]).toMatchObject({ id: "composer-group-withdrawn", message: peppyCopy.composer_status_group_left });
  });

  it("uses the expected info and update-required items", () => {
    expect(groupComposerStatusItems({ ...base(), notice: { kind: "create-not-rcs" } })[0]).toMatchObject({ key: "group-create-not-rcs", id: "composer-group-create-not-rcs", tone: "info", hooks: { "data-group-create": "not-rcs" } });
    expect(groupComposerStatusItems({ ...base(), notice: { kind: "update-required" } })[0]).toMatchObject({ key: "group-update-required", id: "composer-group-update-required", tone: "warning", hooks: { "data-group-send-refusal": "update-required" } });
  });
});
