import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { OmniSearch } from "./OmniSearch";
import type { LocalSearchHit } from "./bridge-contracts";
import { formatDateTime } from "./datetime";

const page = (query: string, items: LocalSearchHit[] = [{ kind: "conversation", conversationId: "c1", title: "Aurora", subtitle: "Recent" }]) => ({
  query,
  items,
  indexState: "ready" as const,
  hasMore: { conversation: false, message: false, contact: false },
});

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((accept) => { resolve = accept; });
  return { promise, resolve };
};

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("OmniSearch", () => {
  it("debounces a completed query before searching", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query));
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "aur" } });
    expect(onSearch).not.toHaveBeenCalled();
    await act(async () => { vi.advanceTimersByTime(100); await Promise.resolve(); });
    expect(onSearch).toHaveBeenCalledWith({ query: "aur", kind: "all", limit: 6 });
  });

  it("does not search or activate while IME composition is active", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query));
    const onActivate = vi.fn();
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={onActivate} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.compositionStart(input);
    fireEvent.change(input, { target: { value: "あ" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearch).not.toHaveBeenCalled();
    expect(onActivate).not.toHaveBeenCalled();
    fireEvent.compositionEnd(input);
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearch).toHaveBeenCalledOnce();
  });

  it("ignores a stale response and activates only the current query result", async () => {
    let resolveOld: (value: ReturnType<typeof page>) => void = () => {};
    const onSearch = vi.fn((request) => request.query === "a"
      ? new Promise<ReturnType<typeof page>>((resolve) => { resolveOld = resolve; })
      : Promise.resolve(page("au", [{ kind: "contact", bookId: "b", contactId: "new", title: "Current", subtitle: "Contact" }])));
    const onActivate = vi.fn();
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={onActivate} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 110)); });
    fireEvent.change(input, { target: { value: "au" } });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 110)); });
    await screen.findByRole("option", { name: /Current/ });
    await act(async () => resolveOld(page("a")));
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onActivate).toHaveBeenCalledWith(expect.objectContaining({ contactId: "new" }));
  });

  it("clears results when disabled or the session changes", async () => {
    const onSearch = vi.fn(async (request) => page(request.query));
    const { rerender } = render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "aur" } });
    await screen.findByRole("option");
    rerender(<OmniSearch sessionKey="two" onSearch={onSearch} onActivate={() => {}} disabled />);
    expect(input).toHaveValue("");
    expect(screen.queryByRole("option")).not.toBeInTheDocument();
  });

  it("opens a category from all results, then appends its cursor page without duplicates", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => {
      if (request.kind === "all") return { ...page(request.query), hasMore: { conversation: true, message: false, contact: false } };
      if (!request.cursor) return { ...page(request.query), nextCursor: "next", hasMore: { conversation: true, message: false, contact: false } };
      return { ...page(request.query, [{ kind: "conversation", conversationId: "c1", title: "Aurora", subtitle: "Recent" }, { kind: "conversation", conversationId: "c2", title: "Bea", subtitle: "New" }]), hasMore: { conversation: false, message: false, contact: false } };
    });
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(screen.getByText("Show all conversations")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => { vi.advanceTimersByTime(100); });
    await act(async () => {});
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => {});
    expect(onSearch).toHaveBeenLastCalledWith({ query: "a", kind: "conversation", cursor: "next", limit: 6 });
    expect(screen.getAllByRole("option", { name: /Aurora/ })).toHaveLength(1);
    expect(screen.getByRole("option", { name: /Bea/ })).toBeInTheDocument();
  });

  it("quietly rebases a stale category cursor from the loaded prefix", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => {
      if (request.kind === "all") return { ...page(request.query), hasMore: { conversation: true, message: false, contact: false } };
      if (!request.cursor) return { ...page(request.query), nextCursor: "stale", hasMore: { conversation: true, message: false, contact: false } };
      throw { code: "invalid-request" };
    });
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    await act(async () => { vi.advanceTimersByTime(100); await Promise.resolve(); });
    expect(onSearch).toHaveBeenLastCalledWith({ query: "a", kind: "conversation", limit: 7 });
    expect(screen.queryByText(/Search could not be completed/)).not.toBeInTheDocument();
  });

  it("closes on outside pointer and restores the shortcut opener on Escape", () => {
    const onSearch = vi.fn(async (request) => page(request.query));
    render(<><button type="button">Open</button><OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} /></>);
    const opener = screen.getByRole("button", { name: "Open" });
    opener.focus();
    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    const input = screen.getByRole("combobox");
    expect(input).toHaveFocus();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(opener).toHaveFocus();
    fireEvent.focus(input);
    fireEvent.pointerDown(document.body);
    expect(input).toHaveAttribute("aria-expanded", "false");
  });

  it("moves through the rendered group order when More is interleaved", async () => {
    vi.useFakeTimers();
    const onActivate = vi.fn();
    render(<OmniSearch sessionKey="one" onActivate={onActivate} onSearch={async (request) => ({
      ...page(request.query, [
        { kind: "conversation", conversationId: "c1", title: "Aurora", subtitle: "Recent" },
        { kind: "message", conversationId: "c1", messageId: "m1", title: "Aurora", snippet: "Hello", timestamp: "Now" },
      ]),
      hasMore: { conversation: true, message: false, contact: false },
    })} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    const label = screen.getByText("Conversations");
    const mouseDown = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    label.dispatchEvent(mouseDown);
    expect(mouseDown.defaultPrevented).toBe(true);
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onActivate).toHaveBeenCalledWith(expect.objectContaining({ kind: "message", messageId: "m1" }));
  });

  it("closes on Tab and ignores keyCode 229 activation", async () => {
    vi.useFakeTimers();
    const onActivate = vi.fn();
    render(<OmniSearch sessionKey="one" onActivate={onActivate} onSearch={async (request) => page(request.query)} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
    expect(onActivate).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "Tab" });
    expect(input).toHaveAttribute("aria-expanded", "false");
  });

  it("revalidates an unchanged query when reopened without activating its hidden result", async () => {
    vi.useFakeTimers();
    const onActivate = vi.fn();
    let calls = 0;
    const onSearch = vi.fn(async (request) => {
      calls += 1;
      return page(request.query, [{ kind: "conversation", conversationId: `c${calls}`, title: `Result ${calls}`, subtitle: "Recent" }]);
    });
    render(<OmniSearch sessionKey="one" onActivate={onActivate} onSearch={onSearch} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.keyDown(input, { key: "Tab" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onActivate).not.toHaveBeenCalled();
    await act(async () => { vi.advanceTimersByTime(100); await Promise.resolve(); });
    expect(onSearch).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("option", { name: /Result 2/ })).toBeInTheDocument();
  });

  it("coalesces readiness hints received during a query into one follow-up refresh", async () => {
    vi.useFakeTimers();
    const first = deferred<ReturnType<typeof page>>();
    const onSearch = vi.fn()
      .mockReturnValueOnce(first.promise)
      .mockImplementation(async (request) => page(request.query, [{ kind: "conversation", conversationId: "ready", title: "Complete", subtitle: "Ready" }]));
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); await Promise.resolve(); });
    for (let refreshKey = 1; refreshKey <= 10; refreshKey += 1)
      rerender(<OmniSearch sessionKey="one" refreshKey={refreshKey} onActivate={() => {}} onSearch={onSearch} />);
    await act(async () => first.resolve(page("a", [{ kind: "conversation", conversationId: "partial", title: "Partial", subtitle: "Indexing" }])));
    await act(async () => { vi.advanceTimersByTime(100); await Promise.resolve(); });
    expect(onSearch).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("option", { name: /Complete/ })).toBeInTheDocument();
  });

  it("preserves a selected hit by durable identity across a same-query refresh", async () => {
    vi.useFakeTimers();
    let calls = 0;
    const onSearch = vi.fn(async (request) => {
      calls += 1;
      return page(request.query, calls === 1
        ? [
            { kind: "conversation", conversationId: "first", title: "First", subtitle: "Recent" },
            { kind: "conversation", conversationId: "selected", title: "Selected", subtitle: "Recent" },
          ]
        : [
            { kind: "conversation", conversationId: "selected", title: "Selected", subtitle: "Recent" },
            { kind: "conversation", conversationId: "first", title: "First", subtitle: "Recent" },
          ]);
    });
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} />);
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(screen.getByRole("combobox")).toHaveAttribute("aria-activedescendant", expect.stringContaining("conversation-selected"));
  });

  it("retries a failed query and preserves its original shortcut opener", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn()
      .mockRejectedValueOnce(new Error("offline"))
      .mockImplementation(async (request) => page(request.query));
    render(<><button type="button">Open</button><OmniSearch sessionKey="one" onActivate={() => {}} onSearch={onSearch} /></>);
    const opener = screen.getByRole("button", { name: "Open" });
    opener.focus();
    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => {
      vi.advanceTimersByTime(100);
      await Promise.resolve();
    });
    expect(screen.getByText(/Search could not be completed/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("option", { name: "Retry search" }));
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearch).toHaveBeenCalledTimes(2);
    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(opener).toHaveFocus();
  });

  it("allows the shortcut from a textarea but excludes modal and password inputs", () => {
    render(<><textarea aria-label="Draft" /><input aria-label="Password" type="password" /><div role="dialog"><button type="button">Modal action</button></div><OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query)} /></>);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    const textarea = screen.getByRole("textbox", { name: "Draft" });
    textarea.focus();
    fireEvent.keyDown(textarea, { key: "k", ctrlKey: true });
    expect(input).toHaveFocus();
    const password = screen.getByLabelText("Password");
    password.focus();
    fireEvent.keyDown(password, { key: "k", ctrlKey: true });
    expect(input).not.toHaveFocus();
    const modalAction = screen.getByRole("button", { name: "Modal action" });
    modalAction.focus();
    fireEvent.keyDown(modalAction, { key: "k", ctrlKey: true });
    expect(input).not.toHaveFocus();
  });

  it("formats valid message timestamps and omits invalid values", async () => {
    vi.useFakeTimers();
    const validTimestamp = "1788434460000";
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [
      { kind: "message", conversationId: "c1", messageId: "valid", title: "Aurora", snippet: "Scheduled", timestamp: validTimestamp },
      { kind: "message", conversationId: "c1", messageId: "invalid", title: "Bea", snippet: "Unknown", timestamp: "not a timestamp" },
    ])} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(screen.getByRole("option", { name: /Scheduled/ })).toHaveTextContent(formatDateTime(validTimestamp)!);
    expect(screen.getByRole("option", { name: /Unknown/ })).toHaveTextContent("Unknown");
    expect(screen.getByRole("option", { name: /Unknown/ })).not.toHaveTextContent("not a timestamp");
  });
});
