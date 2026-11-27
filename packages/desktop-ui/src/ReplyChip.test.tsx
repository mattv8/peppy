import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Composer, ReplyChip } from "./index";

afterEach(cleanup);

function renderComposer(overrides: Partial<Parameters<typeof Composer>[0]> = {}) {
  const props = {
    draft: "",
    attachments: [],
    sendSupported: true,
    onDraftChange: () => {},
    onSend: () => {},
    ...overrides,
  };
  return render(<Composer {...props} />);
}

describe("ReplyChip", () => {
  it("labels the group with the sender and excerpt, and cancels with its own name", () => {
    const onCancel = vi.fn();
    const { container } = render(<ReplyChip name="Alex" excerpt="See you at noon" onCancel={onCancel} />);
    const group = screen.getByRole("group", { name: "Replying to Alex: See you at noon" });
    expect(group).toHaveAttribute("id", "composer-reply-chip");
    expect(container.querySelector("[data-reply-chip-avatar]")).toHaveAttribute("aria-hidden", "true");
    expect(container.querySelector("[data-reply-chip-avatar]")).toHaveTextContent("A");
    expect(container.querySelector("[data-reply-chip-quote] strong")).toHaveTextContent("Alex");
    expect(container.querySelector("[data-reply-chip-excerpt]")).toHaveTextContent("See you at noon");
    fireEvent.click(screen.getByRole("button", { name: "Cancel reply to Alex" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("omits the excerpt span and its label clause when the target has no text", () => {
    const { container } = render(<ReplyChip name="Sam" onCancel={() => {}} />);
    expect(screen.getByRole("group", { name: "Replying to Sam" })).toBeInTheDocument();
    expect(container.querySelector("[data-reply-chip-excerpt]")).toBeNull();
  });

  it("names your own message by subject, while the visible name stays You", () => {
    const onCancel = vi.fn();
    const { container } = render(<ReplyChip name="You" subject="your message" excerpt="Sounds good" onCancel={onCancel} />);
    expect(screen.getByRole("group", { name: "Replying to your message: Sounds good" })).toBeInTheDocument();
    expect(container.querySelector("[data-reply-chip-quote] strong")).toHaveTextContent("You");
    fireEvent.click(screen.getByRole("button", { name: "Cancel reply to your message" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("uses the first letter or digit of the name as the avatar initial", () => {
    const { container, rerender } = render(<ReplyChip name="  élodie" onCancel={() => {}} />);
    expect(container.querySelector("[data-reply-chip-avatar]")).toHaveTextContent("É");
    rerender(<ReplyChip name="7 Pines" onCancel={() => {}} />);
    expect(container.querySelector("[data-reply-chip-avatar]")).toHaveTextContent("7");
  });

  it("shows a user icon instead of an initial when the name has no letter or digit", () => {
    const { container } = render(<ReplyChip name="+ ()" onCancel={() => {}} />);
    const avatar = container.querySelector("[data-reply-chip-avatar]")!;
    expect(avatar).toHaveTextContent("");
    expect(avatar.querySelector("svg")).not.toBeNull();
  });
});

describe("Composer reply props", () => {
  it("renders the reply slot inside the field, before the input row", () => {
    renderComposer({ replySlot: <ReplyChip name="Alex" onCancel={() => {}} /> });
    const chip = document.getElementById("composer-reply-chip")!;
    const row = document.getElementById("composer-input-row")!;
    expect(document.getElementById("composer-field")!.contains(chip)).toBe(true);
    expect(chip.compareDocumentPosition(row) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("describes the textarea by the reply chip while a chip exists", () => {
    const { rerender } = renderComposer({ replySlot: <ReplyChip name="Alex" onCancel={() => {}} /> });
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveAttribute("aria-describedby", "composer-reply-chip");
    rerender(<Composer draft="" attachments={[]} sendSupported onDraftChange={() => {}} onSend={() => {}} />);
    expect(screen.getByRole("textbox", { name: "Message" })).not.toHaveAttribute("aria-describedby");
  });

  it("counts the prefix for the SMS counter but keeps send disabled on an empty draft", () => {
    renderComposer({ counterPrefix: "a".repeat(140) });
    expect(document.getElementById("sms-counter")).toHaveTextContent("140/160");
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  });

  it("calls onEscapeWhenEmpty only when the textarea is empty", () => {
    const onEscapeWhenEmpty = vi.fn();
    const { rerender } = render(<Composer draft="" attachments={[]} sendSupported onDraftChange={() => {}} onSend={() => {}} onEscapeWhenEmpty={onEscapeWhenEmpty} />);
    const textarea = screen.getByRole("textbox", { name: "Message" });
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(onEscapeWhenEmpty).toHaveBeenCalledTimes(1);

    rerender(<Composer draft="hi" attachments={[]} sendSupported onDraftChange={() => {}} onSend={() => {}} onEscapeWhenEmpty={onEscapeWhenEmpty} />);
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Message" }), { key: "Escape" });
    expect(onEscapeWhenEmpty).toHaveBeenCalledTimes(1);
  });
});
