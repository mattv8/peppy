import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import type { MessageStatus } from "../bridge/bridge";
import { RcsMessageMeta } from "./RcsMessageMeta";

describe("RcsMessageMeta", () => {
  it("does not infer delivery from submitted and renders the reported RCS outcomes", () => {
    const { rerender, container } = render(<RcsMessageMeta messageId="submitted" status="submitted" statusLabel="Submitted" />);
    expect(container).toBeEmptyDOMElement();

    rerender(<RcsMessageMeta messageId="sent" status="sent" statusLabel="Sent" />);
    expect(container).toBeEmptyDOMElement();

    rerender(<RcsMessageMeta messageId="delivered" status="delivery-confirmed" statusLabel="Delivery confirmed" />);
    expect(container).toBeEmptyDOMElement();

    rerender(<RcsMessageMeta messageId="read" status="read" statusLabel="Read" />);
    expect(screen.getByText("Read")).toHaveAttribute("data-message-read", "read");

    rerender(<RcsMessageMeta messageId="failed" status="failed-confirmed" statusLabel="Failed" rcsReported />);
    expect(document.querySelector('[data-message-route-hint="failed"]')).toHaveAttribute("data-hint", "failed-reported");

    rerender(<RcsMessageMeta messageId="unknown" status="unknown" statusLabel="Unknown" />);
    expect(document.querySelector('[data-message-route-hint="unknown"]')).toHaveAttribute("data-hint", "unknown");
  });

  it("gives a failure Google did not report the neutral hint, never the Google attribution", () => {
    const { rerender } = render(<RcsMessageMeta messageId="send-state" status="failed-confirmed" statusLabel="Failed" rcsReported={false} />);
    const hint = document.querySelector('[data-message-route-hint="send-state"]');
    expect(hint).toHaveAttribute("data-hint", "failed");
    expect(hint).toHaveTextContent(peppyCopy.rcs_message_failed_unreported);
    expect(hint).not.toHaveTextContent(peppyCopy.rcs_message_failed_reported);

    rerender(<RcsMessageMeta messageId="no-flag" status="failed-confirmed" statusLabel="Failed" />);
    expect(document.querySelector('[data-message-route-hint="no-flag"]')).toHaveAttribute("data-hint", "failed");
  });

  it("renders nothing for a status it does not know", () => {
    const { container } = render(<RcsMessageMeta messageId="future" status={"future-state" as MessageStatus} statusLabel="Future" />);
    expect(container).toBeEmptyDOMElement();
  });
});

describe("RcsMessageMeta attachment outcomes", () => {
  afterEach(cleanup);
  const sent = { id: "sent-1", name: "a.jpg", mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, rcsSendStatus: "sent" as const };
  const refused = { id: "refused-1", name: "notes.pdf", mediaType: "application/pdf", byteSize: 1, state: "ready" as const, rcsSendStatus: "not_sent" as const, error: "not_allowed_mime" };
  const unknown = { id: "unknown-1", name: "receipt.jpg", mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, rcsSendStatus: "outcome_unknown" as const };

  it("gives a partial outcome a summary, a hint per problem, and 'Send the rest again' for refused attachments", () => {
    const onEditResend = vi.fn();
    render(<RcsMessageMeta messageId="partial" status="failed-confirmed" statusLabel="Failed" attachments={[sent, refused, unknown]} onEditResend={onEditResend} />);
    expect(document.querySelector("[data-rcs-media-summary]")).toHaveTextContent("1 of 3 sent");
    expect(document.querySelector('[data-hint="media-refused"]')).toHaveTextContent(peppyCopy.rcs_media_refused_hint.replace("{name}", "notes.pdf"));
    expect(document.querySelector('[data-hint="media-unknown"]')).toHaveTextContent(peppyCopy.rcs_media_unknown_hint.replace("{name}", "receipt.jpg"));
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.rcs_media_resend_rest }));
    expect(onEditResend).toHaveBeenCalledTimes(1);
  });

  it("offers the plain edit-and-resend action when nothing was sent", () => {
    render(<RcsMessageMeta messageId="none" status="failed-confirmed" statusLabel="Failed" attachments={[refused]} onEditResend={vi.fn()} />);
    expect(document.querySelector("[data-rcs-media-summary]")).toBeNull();
    expect(screen.getByRole("button", { name: peppyCopy.group_message_edit_resend })).toBeInTheDocument();
  });

  it("never offers a resend for an unknown outcome alone", () => {
    render(<RcsMessageMeta messageId="unknown" status="unknown" statusLabel="Unknown" attachments={[sent, unknown]} onEditResend={vi.fn()} />);
    expect(screen.queryByRole("button")).toBeNull();
    expect(document.querySelector('[data-hint="media-unknown"]')).toBeInTheDocument();
  });

  it("derives the group review hint from a members-changed attachment without duplicating it", () => {
    const review = vi.fn();
    render(<RcsMessageMeta messageId="members" status="failed-confirmed" statusLabel="Failed" attachments={[{ ...refused, id: "members-file", name: "مرحبا.pdf", rcsSendStatus: "members_changed" }]} onEditResend={review} />);
    expect(document.querySelector('[data-hint="group-members-changed"]')).toHaveTextContent(peppyCopy.group_message_members_changed_hint);
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.group_message_edit_resend }));
    expect(review).toHaveBeenCalledTimes(1);
  });

  it("renders nothing when every attachment was sent", () => {
    const { container } = render(<RcsMessageMeta messageId="all-sent" status="delivery-confirmed" statusLabel="" attachments={[sent]} />);
    expect(container).toBeEmptyDOMElement();
  });
});

describe("media recovery hint boundaries", () => {
  afterEach(cleanup);
  it("offers one recovery callback when the album has both refusal types", () => {
    const file = { id: "first", name: "first.pdf", mediaType: "application/pdf", byteSize: 1, state: "ready" as const };
    const recover = vi.fn();
    render(<RcsMessageMeta messageId="mixed-refusal" statusLabel="Failed" attachments={[{ ...file, rcsSendStatus: "members_changed" }, { ...file, id: "second", rcsSendStatus: "not_sent" }]} onEditResend={recover} />);
    expect(screen.getAllByRole("button")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button"));
    expect(recover).toHaveBeenCalledOnce();
  });
  it("isolates and bounds only the filename substitution, retaining its full title", () => {
    const name = `${"مرحبا".repeat(30)}.pdf`;
    const { container } = render(<RcsMessageMeta messageId="long-name" statusLabel="Unknown" attachments={[{ id: "long-file", name, mediaType: "application/pdf", byteSize: 1, state: "ready", rcsSendStatus: "outcome_unknown" }]} />);
    const value = container.querySelector("bdi")!;
    expect(value).toHaveAttribute("title", name);
    expect(value.textContent!.length).toBeLessThanOrEqual(48);
    expect(value.textContent).toMatch(/\.pdf$/);
    expect(value.closest("p")!.textContent).toContain("Couldn't confirm");
    expect(value.textContent).not.toContain("Couldn't confirm");
  });
});
