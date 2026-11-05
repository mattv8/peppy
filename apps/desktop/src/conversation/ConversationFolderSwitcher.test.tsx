import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ConversationFolderSwitcher } from "./ConversationFolderSwitcher";

describe("ConversationFolderSwitcher", () => {
  afterEach(cleanup);
  const props = () => ({ value: "inbox" as const, counts: { inbox: 3, archived: 0, spam: 2 }, unread: { inbox: 0, archived: 1, spam: 1 }, onChange: vi.fn() });

  it("opens and supports roving keyboard navigation and selection", async () => {
    const values = props(); render(<ConversationFolderSwitcher {...values} />);
    const trigger = screen.getByRole("button", { name: /folder: inbox/i });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    expect(screen.getByRole("menu")).toBeVisible();
    expect(screen.getByRole("menuitemradio", { name: /inbox/i })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "End" });
    expect(screen.getByRole("menuitemradio", { name: /spam/i })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "Enter" });
    expect(values.onChange).toHaveBeenCalledWith("spam");
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("sets menuitemradio state, renders counts and unread dots, and closes on Escape and Tab", () => {
    render(<ConversationFolderSwitcher {...props()} />);
    const trigger = screen.getByRole("button", { name: /folder: inbox/i });
    fireEvent.click(trigger);
    expect(screen.getByRole("menuitemradio", { name: /inbox/i })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("menuitemradio", { name: /archived/i })).toHaveTextContent("0");
    expect(document.querySelectorAll(".conversation-folder-unread-dot")).toHaveLength(2);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.click(trigger);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Tab" });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("exposes the selected folder on the trigger", () => {
    render(<ConversationFolderSwitcher {...props()} value="spam" />);
    expect(screen.getByRole("button", { name: /folder: spam/i })).toHaveAttribute("data-folder", "spam");
  });

  it("closes when focus leaves the switcher and stays open for focus moves inside it", () => {
    render(<><ConversationFolderSwitcher {...props()} /><button>Elsewhere</button></>);
    const trigger = screen.getByRole("button", { name: /folder: inbox/i });
    fireEvent.click(trigger);
    const inbox = screen.getByRole("menuitemradio", { name: /inbox/i });
    fireEvent.focusOut(inbox, { relatedTarget: screen.getByRole("menuitemradio", { name: /spam/i }) });
    expect(screen.getByRole("menu")).toBeVisible();
    fireEvent.focusOut(inbox, { relatedTarget: trigger });
    expect(screen.getByRole("menu")).toBeVisible();
    fireEvent.focusOut(inbox, { relatedTarget: screen.getByRole("button", { name: "Elsewhere" }) });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("closes on focus loss with no related target", () => {
    render(<ConversationFolderSwitcher {...props()} />);
    fireEvent.click(screen.getByRole("button", { name: /folder: inbox/i }));
    fireEvent.focusOut(screen.getByRole("menuitemradio", { name: /inbox/i }), { relatedTarget: null });
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
