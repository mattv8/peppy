import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { formatPhoneNumber } from "@peppy/desktop-ui";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { GroupDetailsDialog, type GroupDetailsDialogProps, groupConnection, orderGroupMembers } from "./GroupDetailsDialog";

const contacts: Record<string, string> = {
  "+15551230001": "Alex Rivera",
  "+15551230002": "Sam",
};
const contactName = (address: string) => contacts[address];

function renderDialog(overrides: Partial<GroupDetailsDialogProps["group"]> & Partial<Omit<GroupDetailsDialogProps, "group">> = {}) {
  const { onOpenContact = vi.fn(), onDismiss = vi.fn(), toneSeed = "conversation-1", name, members, state, ...rest } = overrides;
  const props: GroupDetailsDialogProps = {
    group: { name, members: members ?? ["+15551230001", "+15551230002", "+15551230003"], state: state ?? "live" },
    contactName,
    toneSeed,
    onOpenContact,
    onDismiss,
    ...rest,
  };
  const view = render(<GroupDetailsDialog {...props} />);
  return { ...view, onOpenContact, onDismiss };
}

describe("GroupDetailsDialog", () => {
  const originalShowModal = HTMLDialogElement.prototype.showModal;
  const originalClose = HTMLDialogElement.prototype.close;

  beforeEach(() => {
    HTMLDialogElement.prototype.showModal = vi.fn(function (this: HTMLDialogElement) { this.open = true; });
    HTMLDialogElement.prototype.close = vi.fn(function (this: HTMLDialogElement) { this.open = false; });
  });

  afterEach(() => {
    cleanup();
    HTMLDialogElement.prototype.showModal = originalShowModal;
    HTMLDialogElement.prototype.close = originalClose;
    vi.restoreAllMocks();
  });

  it("opens as a titled dialog with a single OK action", () => {
    const { container, onDismiss } = renderDialog({ name: "Weekend crew" });
    expect(container.querySelector("#group-details-dialog")).not.toBeNull();
    expect(screen.getByRole("dialog", { name: peppyCopy.group_details_title })).toBeInTheDocument();
    fireEvent.click(container.querySelector("#group-details-close")!);
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("shows the name read-only and isolates it with bdi", () => {
    const { container } = renderDialog({ name: "  Weekend crew  " });
    const name = container.querySelector("[data-group-fact='name'] dd bdi");
    expect(name).toHaveTextContent("Weekend crew");
    expect(container.querySelectorAll("input, textarea, [contenteditable]")).toHaveLength(0);
  });

  it("says there is no name when the group is unnamed or the name is blank", () => {
    const { container } = renderDialog({ name: "   " });
    expect(container.querySelector("[data-group-fact='name'] dd")).toHaveTextContent(peppyCopy.group_details_name_none);
  });

  it("states RCS delivery and the phone-only pointer for a live group", () => {
    const { container } = renderDialog({ name: "Weekend crew", state: "live" });
    expect(container.querySelector("[data-group-connection]")).toHaveAttribute("data-group-connection", "rcs");
    expect(container.querySelector("#group-details-google-pointer")).toHaveTextContent("To rename or change members, use Google Messages on your phone.");
    expect(container.querySelector("#group-details-paused")).toBeNull();
  });

  it("keeps the pointer for a paused group and adds a status line", () => {
    const { container } = renderDialog({ state: "paused" });
    expect(container.querySelector("[data-group-connection]")).toHaveAttribute("data-group-connection", "paused");
    expect(container.querySelector("#group-details-paused")).toHaveAttribute("role", "status");
    expect(container.querySelector("#group-details-paused")).toHaveTextContent(peppyCopy.group_notice_paused);
    expect(container.querySelector("#group-details-google-pointer")).not.toBeNull();
  });

  it("hides the pointer for an MMS group, since Google cannot change a carrier group", () => {
    const { container } = renderDialog({ state: "unlinked" });
    expect(container.querySelector("[data-group-connection]")).toHaveAttribute("data-group-connection", "mms");
    expect(screen.getByText(peppyCopy.group_details_connection_mms)).toBeInTheDocument();
    expect(container.querySelector("#group-details-google-pointer")).toBeNull();
  });

  it("reads a withdrawn group as left, without the self row or the pointer", () => {
    const { container } = renderDialog({ state: "withdrawn" });
    expect(container.querySelector("[data-group-connection]")).toHaveAttribute("data-group-connection", "left");
    expect(screen.getByText(peppyCopy.group_details_connection_left)).toBeInTheDocument();
    expect(container.querySelector("#group-details-google-pointer")).toBeNull();
    expect(container.querySelector("[data-member-self]")).toBeNull();
  });

  it("puts the user first and counts the user in the members heading", () => {
    const { container } = renderDialog({ members: ["+15551230001", "+15551230002"] });
    const items = container.querySelectorAll("#group-members-list > li");
    expect(items[0]).toHaveAttribute("data-member-self", "true");
    expect(items[0]).toHaveTextContent(peppyCopy.group_details_you);
    expect(container.querySelector("#group-members-heading span")).toHaveTextContent("3");
  });

  it("names each member, and shows the number under a resolved contact", () => {
    const { container } = renderDialog({ members: ["+15551230001", "+15551230003"] });
    const alex = container.querySelector("#group-member-\\+15551230001")!;
    expect(alex).toHaveAttribute("data-member-address", "+15551230001");
    expect(within(alex as HTMLElement).getByText("Alex Rivera", { selector: "bdi" })).toBeInTheDocument();
    expect(alex.querySelector("small")).toHaveTextContent(formatPhoneNumber("+15551230001"));
    const unresolved = container.querySelector("#group-member-\\+15551230003")!;
    expect(unresolved.querySelector("small")).toBeNull();
    expect(unresolved.querySelector("bdi")).toHaveTextContent(formatPhoneNumber("+15551230003"));
  });

  it("labels member rows as contact actions, matching sender labels", () => {
    renderDialog({ members: ["+15551230001", "+15551230003"] });
    expect(screen.getByRole("button", { name: "Open Alex Rivera in Contacts" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: `Add ${formatPhoneNumber("+15551230003")} to contacts` })).toBeInTheDocument();
  });

  it("closes the dialog before opening the member's contact", () => {
    const calls: string[] = [];
    const { container } = renderDialog({
      members: ["+15551230001"],
      onDismiss: () => calls.push("dismiss"),
      onOpenContact: (address) => calls.push(`open ${address}`),
    });
    fireEvent.click(container.querySelector("#group-member-\\+15551230001 button")!);
    expect(calls).toEqual(["dismiss", "open +15551230001"]);
  });

  it("says members are not available yet when the roster is empty", () => {
    const { container } = renderDialog({ members: [] });
    expect(container.querySelector("#group-members-empty")).toHaveTextContent(peppyCopy.group_details_members_empty);
    expect(container.querySelector("#group-members-list")).toBeNull();
  });

  it("keeps a very long name and a long member list inside the dialog", () => {
    const longName = "Extraordinarily long group name ".repeat(6);
    const members = Array.from({ length: 40 }, (_, index) => `+1555${String(1230000 + index).padStart(7, "0")}`);
    const { container } = renderDialog({ name: longName, members });
    expect(container.querySelector("[data-group-fact='name'] dd bdi")).toHaveTextContent(longName.trim());
    expect(container.querySelectorAll("#group-members-list > li")).toHaveLength(41);
  });
});

describe("groupConnection", () => {
  it.each([
    ["live", "rcs"],
    ["paused", "paused"],
    ["unlinked", "mms"],
    ["proving", "mms"],
    ["withdrawn", "left"],
  ] as const)("maps %s to %s", (state, connection) => {
    expect(groupConnection(state)).toBe(connection);
  });
});

describe("orderGroupMembers", () => {
  it("orders named contacts A to Z and then unresolved numbers by number", () => {
    const order = orderGroupMembers(
      ["+15551230009", "+15551230003", "+15551230002", "+15551230001"],
      (address) => ({ "+15551230001": "Zed", "+15551230002": "amy" } as Record<string, string>)[address],
    );
    expect(order).toEqual(["+15551230002", "+15551230001", "+15551230003", "+15551230009"]);
  });
});
