import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { OmniSearch } from "./OmniSearch";
import type { LocalSearchHit, LocalSearchPage } from "./bridge-contracts";
import { formatDateTime } from "./datetime";

const page = (query: string, items: LocalSearchHit[] = [{ kind: "conversation", conversationId: "c1", title: "Aurora", subtitle: "Recent" }]) => ({
  query,
  items,
  indexState: "ready" as const,
  hasMore: { conversation: false, message: false, contact: false },
});

const indexingPage = (query: string, items: LocalSearchHit[] = [{ kind: "conversation", conversationId: "partial", title: "Partial", subtitle: "Indexing" }]) => ({
  ...page(query, items),
  indexState: "indexing" as const,
});

const conversationHits = (count: number, start = 1): LocalSearchHit[] => Array.from({ length: count }, (_, offset) => ({
  kind: "conversation",
  conversationId: `c${start + offset}`,
  title: `Conversation ${start + offset}`,
  subtitle: "Recent",
}));

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

  it("restarts the trailing debounce from the last text edit", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query));
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(99); });
    fireEvent.change(input, { target: { value: "aur" } });
    await act(async () => { vi.advanceTimersByTime(99); });
    expect(onSearch).not.toHaveBeenCalled();
    await act(async () => { vi.advanceTimersByTime(1); });
    expect(onSearch).toHaveBeenCalledOnce();
    expect(onSearch).toHaveBeenLastCalledWith({ query: "aur", kind: "all", limit: 6 });
  });

  it("searches again after an edit that keeps the same trimmed query", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query));
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "aur" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(screen.getByRole("option", { name: /Aurora/ })).toBeInTheDocument();
    fireEvent.change(input, { target: { value: " aur " } });
    await act(async () => { vi.advanceTimersByTime(99); });
    expect(onSearch).toHaveBeenCalledOnce();
    await act(async () => { vi.advanceTimersByTime(1); });
    expect(onSearch).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("option", { name: /Aurora/ })).toBeInTheDocument();
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

  it("ignores a superseded rebase timer after a newer More request settles", async () => {
    vi.useFakeTimers();
    let cursorCalls = 0;
    const onSearch = vi.fn(async (request) => {
      if (request.kind === "all") return { ...page(request.query), hasMore: { conversation: true, message: false, contact: false } };
      if (onSearch.mock.calls.length === 5) return page(request.query, [{ kind: "conversation", conversationId: "c1", title: "Aurora", subtitle: "Recent" }, { kind: "conversation", conversationId: "c2", title: "Bea", subtitle: "New" }]);
      if (!request.cursor) return { ...page(request.query), nextCursor: "stale", hasMore: { conversation: true, message: false, contact: false } };
      cursorCalls += 1;
      if (cursorCalls === 1) throw { code: "invalid-request" };
      return indexingPage(request.query, [{ kind: "conversation", conversationId: "c1", title: "Aurora", subtitle: "Recent" }, { kind: "conversation", conversationId: "c2", title: "Bea", subtitle: "New" }]);
    });
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearch).toHaveBeenCalledTimes(4);
    expect(screen.queryByText("Searching…")).not.toBeInTheDocument();
    expect(screen.getByText(/Search index is updating/)).toBeInTheDocument();
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onSearch={onSearch} onActivate={() => {}} />);
    await act(async () => { vi.advanceTimersByTime(900); await Promise.resolve(); await Promise.resolve(); });
    expect(onSearch).toHaveBeenCalledTimes(5);
    expect(onSearch).toHaveBeenLastCalledWith({ query: "a", kind: "conversation", limit: 7 });
    expect(screen.queryByText("Searching…")).not.toBeInTheDocument();
    expect(screen.queryByText(/Search index is updating/)).not.toBeInTheDocument();
    expect(screen.getByRole("option", { name: /Bea/ })).toBeInTheDocument();
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

  it("settles a ready query despite hints before, during, and after its response", async () => {
    vi.useFakeTimers();
    const first = deferred<ReturnType<typeof page>>();
    const onSearch = vi.fn().mockReturnValue(first.promise);
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} />);
    await act(async () => { vi.advanceTimersByTime(100); });
    for (let refreshKey = 2; refreshKey <= 6; refreshKey += 1) {
      await act(async () => { vi.advanceTimersByTime(50); });
      rerender(<OmniSearch sessionKey="one" refreshKey={refreshKey} onActivate={() => {}} onSearch={onSearch} />);
    }
    await act(async () => first.resolve(page("a")));
    for (let refreshKey = 7; refreshKey <= 12; refreshKey += 1) {
      await act(async () => { vi.advanceTimersByTime(50); });
      rerender(<OmniSearch sessionKey="one" refreshKey={refreshKey} onActivate={() => {}} onSearch={onSearch} />);
    }
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(onSearch).toHaveBeenCalledOnce();
    expect(screen.getByRole("option", { name: /Aurora/ })).toBeInTheDocument();
  });

  it("refreshes indexing results silently at most once per hint interval and stops when ready", async () => {
    vi.useFakeTimers();
    const first = deferred<ReturnType<typeof indexingPage>>();
    const background = deferred<ReturnType<typeof page>>();
    const onSearch = vi.fn()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(background.promise);
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} />);
    rerender(<OmniSearch sessionKey="one" refreshKey={2} onActivate={() => {}} onSearch={onSearch} />);
    await act(async () => { vi.advanceTimersByTime(1_500); });
    await act(async () => first.resolve(indexingPage("a")));
    expect(screen.getByRole("option", { name: /Partial/ })).toBeInTheDocument();
    await act(async () => { vi.advanceTimersByTime(999); });
    expect(onSearch).toHaveBeenCalledOnce();
    await act(async () => { vi.advanceTimersByTime(1); });
    expect(onSearch).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("option", { name: /Partial/ })).toBeInTheDocument();
    rerender(<OmniSearch sessionKey="one" refreshKey={3} onActivate={() => {}} onSearch={onSearch} />);
    rerender(<OmniSearch sessionKey="one" refreshKey={4} onActivate={() => {}} onSearch={onSearch} />);
    await act(async () => background.resolve(page("a", [{ kind: "conversation", conversationId: "ready", title: "Complete", subtitle: "Ready" }])));
    await act(async () => { vi.advanceTimersByTime(2_000); });
    expect(onSearch).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("option", { name: /Complete/ })).toBeInTheDocument();
  });

  it("lets a pending More request finish before an armed indexing refresh", async () => {
    vi.useFakeTimers();
    const more = deferred<LocalSearchPage>();
    const firstCategoryPage = {
      ...indexingPage("a", conversationHits(6)),
      nextCursor: "next",
      hasMore: { conversation: true, message: false, contact: false },
    };
    const onSearch = vi.fn((request) => {
      if (request.kind === "all") return Promise.resolve({ ...indexingPage(request.query), hasMore: { conversation: true, message: false, contact: false } });
      if (request.cursor === "next") return more.promise;
      return Promise.resolve(firstCategoryPage);
    });
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => { vi.advanceTimersByTime(100); });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} />);
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(onSearch).toHaveBeenCalledTimes(3);
    await act(async () => more.resolve({ ...page("a", conversationHits(1, 7)), hasMore: { conversation: false, message: false, contact: false } }));
    expect(screen.getByRole("option", { name: /Conversation 7/ })).toBeInTheDocument();
    expect(screen.queryByText("Searching…")).not.toBeInTheDocument();
  });

  it("keeps the loaded category extent when a hint arrives during More", async () => {
    vi.useFakeTimers();
    const more = deferred<ReturnType<typeof indexingPage>>();
    const background = deferred<ReturnType<typeof indexingPage>>();
    const firstCategoryPage = {
      ...indexingPage("a", conversationHits(6)),
      nextCursor: "next",
      hasMore: { conversation: true, message: false, contact: false },
    };
    const onSearch = vi.fn((request) => {
      if (request.kind === "all") return Promise.resolve({ ...indexingPage(request.query), hasMore: { conversation: true, message: false, contact: false } });
      if (request.cursor === "next") return more.promise;
      if (onSearch.mock.calls.length >= 4) return background.promise;
      return Promise.resolve(firstCategoryPage);
    });
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Show all conversations"));
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Show all conversations"));
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} />);
    await act(async () => more.resolve({ ...indexingPage("a", conversationHits(6, 7)), hasMore: { conversation: true, message: false, contact: false } }));
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(onSearch).toHaveBeenLastCalledWith({ query: "a", kind: "conversation", limit: 12 });
    await act(async () => background.resolve(indexingPage("a", conversationHits(12))));
    expect(screen.getAllByRole("option", { name: /Conversation/ })).toHaveLength(12);
  });

  it("cancels queued and in-flight indexing refresh work on unmount", async () => {
    vi.useFakeTimers();
    const queuedSearch = vi.fn(async (request) => indexingPage(request.query));
    const queued = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={queuedSearch} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    queued.rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={queuedSearch} />);
    expect(vi.getTimerCount()).toBe(1);
    queued.unmount();
    expect(vi.getTimerCount()).toBe(0);

    const first = deferred<ReturnType<typeof indexingPage>>();
    const inFlightSearch = vi.fn().mockReturnValue(first.promise);
    const inFlight = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={inFlightSearch} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    inFlight.rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={inFlightSearch} />);
    inFlight.unmount();
    await act(async () => first.resolve(indexingPage("a")));
    expect(vi.getTimerCount()).toBe(0);
    await act(async () => { vi.advanceTimersByTime(2_000); });
    expect(queuedSearch).toHaveBeenCalledOnce();
    expect(inFlightSearch).toHaveBeenCalledOnce();
  });

  it.each([
    ["session replacement", { sessionKey: "two", disabled: false }],
    ["disable", { sessionKey: "one", disabled: true }],
  ])("cancels a queued indexing refresh on %s", async (_name, nextProps) => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => indexingPage(request.query));
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    rerender(<OmniSearch {...nextProps} refreshKey={1} onActivate={() => {}} onSearch={onSearch} />);
    expect(vi.getTimerCount()).toBe(0);
    await act(async () => { vi.advanceTimersByTime(2_000); });
    expect(onSearch).toHaveBeenCalledOnce();
  });

  it("keeps indexing results and retry state unchanged when a background refresh fails", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn()
      .mockResolvedValueOnce(indexingPage("a"))
      .mockRejectedValueOnce(new Error("offline"));
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} />);
    await act(async () => { vi.advanceTimersByTime(1_000); await Promise.resolve(); });
    expect(screen.getByRole("option", { name: /Partial/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "Retry search" })).not.toBeInTheDocument();
  });

  it("discards hints during typing and composition and cancels a queued refresh on close", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => indexingPage(request.query));
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} />);
    fireEvent.compositionStart(input);
    rerender(<OmniSearch sessionKey="one" refreshKey={2} onActivate={() => {}} onSearch={onSearch} />);
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(onSearch).not.toHaveBeenCalled();
    fireEvent.compositionEnd(input);
    await act(async () => { vi.advanceTimersByTime(100); });
    rerender(<OmniSearch sessionKey="one" refreshKey={3} onActivate={() => {}} onSearch={onSearch} />);
    fireEvent.keyDown(input, { key: "Tab" });
    expect(vi.getTimerCount()).toBe(0);
    await act(async () => { vi.advanceTimersByTime(2_000); });
    expect(onSearch).toHaveBeenCalledOnce();
  });

  it("preserves a selected hit by durable identity across an indexing refresh", async () => {
    vi.useFakeTimers();
    let calls = 0;
    const onSearch = vi.fn(async (request) => {
      calls += 1;
      return indexingPage(request.query, calls === 1
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
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(onSearch).toHaveBeenCalledTimes(2);
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

  it("clears the query from the inset button without sending an empty-query request", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query));
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "aur" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(screen.getByRole("option", { name: /Aurora/ })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));
    expect(input).toHaveValue("");
    expect(input).toHaveFocus();
    expect(screen.queryByRole("option")).not.toBeInTheDocument();
    expect(screen.getByText("Search locally synced conversations, messages, and contacts.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Clear search" })).not.toBeInTheDocument();
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearch).toHaveBeenCalledOnce();
    expect(onSearch).not.toHaveBeenCalledWith(expect.objectContaining({ query: "" }));
  });

  it("shows the clear button for whitespace-only input and clears it", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query));
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));
    expect(input).toHaveValue("");
    expect(input).toHaveFocus();
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearch).not.toHaveBeenCalled();
  });

  it("cancels a debounced search when the clear button is used", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query));
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "aur" } });
    await act(async () => { vi.advanceTimersByTime(50); });
    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearch).not.toHaveBeenCalled();
  });

  it("ignores an in-flight response that settles after the query was cleared", async () => {
    vi.useFakeTimers();
    const pending = deferred<ReturnType<typeof page>>();
    const onSearch = vi.fn().mockReturnValue(pending.promise);
    const onActivate = vi.fn();
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={onActivate} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "aur" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearch).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));
    await act(async () => pending.resolve(page("aur")));
    expect(screen.queryByRole("option")).not.toBeInTheDocument();
    expect(input).toHaveValue("");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onActivate).not.toHaveBeenCalled();
  });

  it("cancels a queued indexing refresh when the clear button is used", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => indexingPage(request.query));
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "a" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} />);
    expect(vi.getTimerCount()).toBe(1);
    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));
    await act(async () => { vi.advanceTimersByTime(2_000); });
    expect(onSearch).toHaveBeenCalledOnce();
  });

  it("keeps the clear button keyboard reachable while a query is present and hides it when disabled", () => {
    const onSearch = vi.fn(async (request) => page(request.query));
    const { rerender } = render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    const clear = screen.getByRole("button", { name: "Clear search" });
    expect(clear).toHaveAttribute("type", "button");
    clear.focus();
    expect(clear).toHaveFocus();
    rerender(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} disabled />);
    expect(screen.queryByRole("button", { name: "Clear search" })).not.toBeInTheDocument();
  });

  it("restores the external shortcut opener after the internal clear button refocuses the input", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query));
    render(<><button type="button">Open</button><OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} /></>);
    const opener = screen.getByRole("button", { name: "Open" });
    opener.focus();
    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    expect(input).toHaveFocus();
    fireEvent.change(input, { target: { value: "aur" } });
    const clear = screen.getByRole("button", { name: "Clear search" });
    clear.focus();
    fireEvent.click(clear);
    expect(input).toHaveFocus();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(opener).toHaveFocus();
  });

  it("clears an active composition so typing after Clear searches again", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query));
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={() => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.compositionStart(input);
    fireEvent.change(input, { target: { value: "あ" } });
    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));
    fireEvent.change(input, { target: { value: "new" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearch).toHaveBeenCalledWith({ query: "new", kind: "all", limit: 6 });
  });
});
