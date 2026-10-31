import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { historyPresentation, OmniSearch } from "./OmniSearch";
import type { HistorySearchSnapshot, LocalSearchHit, LocalSearchPage } from "./bridge-contracts";
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

const history = (overrides: Partial<HistorySearchSnapshot> = {}): HistorySearchSnapshot => ({
  state: "idle",
  canSearchFurther: true,
  progressVersion: "progress-1",
  indexVersion: "index-1",
  ...overrides,
});

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
    vi.useFakeTimers();
    let resolveOld: (value: ReturnType<typeof page>) => void = () => {};
    const onSearch = vi.fn((request) => request.query === "a"
      ? new Promise<ReturnType<typeof page>>((resolve) => { resolveOld = resolve; })
      : Promise.resolve(page("au", [{ kind: "contact", bookId: "b", contactId: "new", title: "Current", subtitle: "Contact" }])));
    const onActivate = vi.fn();
    render(<OmniSearch sessionKey="one" onSearch={onSearch} onActivate={onActivate} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(110); });
    fireEvent.change(input, { target: { value: "au" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(110); });
    expect(screen.getByRole("option", { name: /Current/ })).toBeInTheDocument();
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

  it("starts a conversation with a valid typed phone number", async () => {
    vi.useFakeTimers();
    const onStartConversation = vi.fn(async () => true);
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} onStartConversation={onStartConversation} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "(415) 555-2671" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    const candidate = screen.getByRole("option", { name: /Start conversation with/ });
    expect(candidate).toHaveAttribute("aria-selected", "true");
    fireEvent.click(candidate);
    expect(onStartConversation).toHaveBeenCalledWith("+14155552671", expect.any(AbortSignal));
  });

  it("dismisses search after transferring a start to an external guard", async () => {
    vi.useFakeTimers();
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} onStartConversation={async () => "deferred"} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "4155552671" } });
    fireEvent.click(screen.getByRole("option", { name: /Start conversation with/ }));
    await act(async () => { await Promise.resolve(); });
    expect(input).toHaveValue("");
    expect(input).toHaveAttribute("aria-expanded", "false");
  });

  it("resolves a contact number only after its conversation action is activated", async () => {
    vi.useFakeTimers();
    const resolveContactNumbers = vi.fn(async () => [{ address: "stored-address", display: "(415) 555-2671" }]);
    const onStartConversation = vi.fn(async () => false);
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [{ kind: "contact", bookId: "book", contactId: "contact", title: "Ada", subtitle: "Friend" }])} resolveContactNumbers={resolveContactNumbers} onStartConversation={onStartConversation} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "ada" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(resolveContactNumbers).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("option", { name: "Start conversation with Ada" }));
    await act(async () => {});
    expect(resolveContactNumbers).toHaveBeenCalledWith(expect.objectContaining({ contactId: "contact" }), expect.any(AbortSignal));
    expect(onStartConversation).toHaveBeenCalledWith("stored-address", expect.any(AbortSignal));
  });

  it("shows a chooser for contacts with multiple phone numbers and returns to search on Escape", async () => {
    vi.useFakeTimers();
    const onStartConversation = vi.fn(async () => false);
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [{ kind: "contact", bookId: "book", contactId: "contact", title: "Ada", subtitle: "Friend" }])} resolveContactNumbers={async () => [{ address: "+14155552671", display: "Mobile" }, { address: "+14155552672", display: "Work" }]} onStartConversation={onStartConversation} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "ada" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("option", { name: "Start conversation with Ada" }));
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByRole("option", { name: "Mobile" })).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.getByRole("option", { name: "Ada Friend" })).toBeInTheDocument();
  });

  it("keeps stored contact addresses unchanged and reports contacts without phone numbers", async () => {
    vi.useFakeTimers();
    const onStartConversation = vi.fn(async () => false);
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [{ kind: "contact", bookId: "book", contactId: "contact", title: "Ada", subtitle: "Friend" }])} resolveContactNumbers={async () => []} onStartConversation={onStartConversation} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "ada" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("option", { name: "Start conversation with Ada" }));
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByText("This contact has no phone numbers.")).toBeInTheDocument();
    expect(onStartConversation).not.toHaveBeenCalled();
  });

  it("aborts a pending recipient action when the query changes", async () => {
    vi.useFakeTimers();
    let signal: AbortSignal | undefined;
    const pending = new Promise<boolean>(() => {});
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} onStartConversation={(_address, nextSignal) => { signal = nextSignal; return pending; }} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "4155552671" } });
    fireEvent.click(screen.getByRole("option", { name: /Start conversation with/ }));
    fireEvent.change(input, { target: { value: "4155552672" } });
    expect(signal?.aborted).toBe(true);
  });

  it("aborts a pending recipient action when search closes", () => {
    let signal: AbortSignal | undefined;
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} onStartConversation={(_address, nextSignal) => { signal = nextSignal; return new Promise<boolean>(() => {}); }} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "4155552671" } });
    fireEvent.click(screen.getByRole("option", { name: /Start conversation with/ }));
    fireEvent.keyDown(input, { key: "Escape" });
    expect(signal?.aborted).toBe(true);
  });

  it("prevents duplicate activation while a conversation start is pending", () => {
    const onStartConversation = vi.fn(() => new Promise<boolean>(() => {}));
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} onStartConversation={onStartConversation} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "4155552671" } });
    const candidate = screen.getByRole("option", { name: /Start conversation with/ });
    fireEvent.click(candidate);
    fireEvent.click(candidate);
    expect(onStartConversation).toHaveBeenCalledOnce();
  });

  it.each([
    ["session changes", { sessionKey: "two" }],
    ["is disabled", { sessionKey: "one", disabled: true }],
  ])("aborts a pending recipient action when %s", (_name, nextProps) => {
    let signal: AbortSignal | undefined;
    const { rerender } = render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} onStartConversation={(_address, nextSignal) => { signal = nextSignal; return new Promise<boolean>(() => {}); }} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "4155552671" } });
    fireEvent.click(screen.getByRole("option", { name: /Start conversation with/ }));
    rerender(<OmniSearch {...nextProps} onActivate={() => {}} onSearch={async (request) => page(request.query, [])} onStartConversation={(_address, nextSignal) => { signal = nextSignal; return new Promise<boolean>(() => {}); }} />);
    expect(signal?.aborted).toBe(true);
  });

  it("retries a rejected chooser selection with the keyboard", async () => {
    vi.useFakeTimers();
    const onStartConversation = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(false);
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [{ kind: "contact", bookId: "book", contactId: "contact", title: "Ada", subtitle: "Friend" }])} resolveContactNumbers={async () => [{ address: "12345", display: "Short code" }, { address: "+14155552672", display: "Work" }]} onStartConversation={onStartConversation} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "ada" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("option", { name: "Start conversation with Ada" }));
    await act(async () => { await Promise.resolve(); });
    fireEvent.keyDown(input, { key: "Enter" });
    await act(async () => { await Promise.resolve(); });
    fireEvent.keyDown(input, { key: "End" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onStartConversation).toHaveBeenNthCalledWith(1, "12345", expect.any(AbortSignal));
    expect(onStartConversation).toHaveBeenCalledTimes(2);
  });

  it("does not render an orphan typed-number candidate in a category drill-in", async () => {
    vi.useFakeTimers();
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => request.kind === "all" ? { ...page(request.query, [{ kind: "contact", bookId: "book", contactId: "contact", title: "Ada", subtitle: "Friend" }]), hasMore: { conversation: false, message: false, contact: true } } : page(request.query, [])} onStartConversation={async () => false} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "4155552671" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Show all contacts"));
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(screen.queryByRole("option", { name: /Start conversation with/ })).not.toBeInTheDocument();
  });

  it("preserves a selected contact action through an indexing refresh", async () => {
    vi.useFakeTimers();
    const contact = { kind: "contact" as const, bookId: "book", contactId: "contact", title: "Ada", subtitle: "Friend" };
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={async (request) => indexingPage(request.query, [contact])} resolveContactNumbers={async () => []} onStartConversation={async () => false} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "ada" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={async (request) => indexingPage(request.query, [contact])} resolveContactNumbers={async () => []} onStartConversation={async () => false} />);
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(input).toHaveAttribute("aria-activedescendant", expect.stringContaining("start-contact-book-contact"));
  });

  it("shows distinct inline contact lookup errors with a retry action", async () => {
    vi.useFakeTimers();
    const contact = { kind: "contact" as const, bookId: "book", contactId: "contact", title: "Ada", subtitle: "Friend" };
    const resolveContactNumbers = vi.fn().mockRejectedValueOnce({ code: "contact-unavailable" }).mockRejectedValueOnce({ code: "contact-oversized" });
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [contact])} resolveContactNumbers={resolveContactNumbers} onStartConversation={async () => false} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "ada" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("option", { name: "Start conversation with Ada" }));
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByText("This contact is no longer available.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("option", { name: "Retry contact phone lookup" }));
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByText("This contact book is too large to search.")).toBeInTheDocument();
  });

  it("filters blank phones and encodes stored raw addresses in chooser IDs", async () => {
    vi.useFakeTimers();
    const onStartConversation = vi.fn(async () => false);
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [{ kind: "contact", bookId: "book", contactId: "contact", title: "Ada", subtitle: "Friend" }])} resolveContactNumbers={async () => [{ address: " ", display: "Blank" }, { address: "short code / one", display: "Short code" }, { address: "work two", display: "Work" }]} onStartConversation={onStartConversation} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "ada" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("option", { name: "Start conversation with Ada" }));
    await act(async () => { await Promise.resolve(); });
    const choice = screen.getByRole("option", { name: "Short code" });
    expect(choice.id).toContain(encodeURIComponent("short code / one"));
    expect(choice).toHaveAttribute("aria-selected", "true");
    fireEvent.click(choice);
    expect(onStartConversation).toHaveBeenCalledWith("short code / one", expect.any(AbortSignal));
  });

  it("keeps the selected chooser number through a background indexing refresh", async () => {
    vi.useFakeTimers();
    const contact = { kind: "contact" as const, bookId: "book", contactId: "contact", title: "Ada", subtitle: "Friend" };
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={async (request) => indexingPage(request.query, [contact])} resolveContactNumbers={async () => [{ address: "one", display: "One" }, { address: "two", display: "Two" }]} onStartConversation={async () => false} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "ada" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("option", { name: "Start conversation with Ada" }));
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByRole("option", { name: "One" })).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "ArrowDown" });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={async (request) => indexingPage(request.query, [contact])} resolveContactNumbers={async () => [{ address: "one", display: "One" }, { address: "two", display: "Two" }]} onStartConversation={async () => false} />);
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(screen.getByRole("option", { name: "Two" })).toHaveAttribute("aria-selected", "true");
  });

  it.each([
    [history({ state: "catchingUp" }), true, false, "Cancel"],
    [history({ state: "partial" }), false, false, "Search further back"],
    [history({ state: "blocked", reason: "needsUnlock" }), false, false, "Retry"],
    [history({ state: "completed", canSearchFurther: false }), false, false, null],
    [history({ state: "completed" }), false, false, "Search further back"],
    [history({ state: "blocked", reason: "unsupported", canSearchFurther: false }), false, false, null],
    [history({ state: "blocked", reason: "unsupported" }), false, false, "Search further back"],
    [history({ state: "blocked", reason: "unsupported" }), false, true, null],
    [history({ state: "idle", canSearchFurther: false }), false, false, null],
  ])("maps history state to its available action", (snapshot, indexing, hasMatches, action) => {
    const presentation = historyPresentation({ snapshot, indexing, hasMatches, queryCurrent: true, hasCancel: true, startFailed: false });
    expect(presentation === null || presentation.intent === null ? null : presentation.intent === "cancel" ? "Cancel" : presentation.intent === "retry" ? "Retry" : "Search further back").toBe(action);
  });

  it("offers history only for an actionable no-result query without changing legacy empty behavior", async () => {
    vi.useFakeTimers();
    const onSearchFurtherBack = vi.fn(async () => {});
    const { rerender } = render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "missing" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(screen.getByText("No matches.")).toBeInTheDocument();
    rerender(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} historySearch={history()} onSearchFurtherBack={onSearchFurtherBack} />);
    expect(screen.getByRole("option", { name: "Search further back" })).toBeInTheDocument();
    expect(screen.queryByText("No matches.")).not.toBeInTheDocument();
    fireEvent.change(input, { target: { value: "missing again" } });
    expect(onSearchFurtherBack).not.toHaveBeenCalled();
  });

  it("offers a re-armed completed search only when the flag changes, and starts it only on explicit activation", async () => {
    vi.useFakeTimers();
    const onSearchFurtherBack = vi.fn(async () => {});
    const search = (historySearch: HistorySearchSnapshot) => (
      <OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} historySearch={historySearch} onSearchFurtherBack={onSearchFurtherBack} />
    );
    const { rerender } = render(search(history({ state: "completed", canSearchFurther: false })));
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "missing" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(document.getElementById("omni-search-history-status")).toHaveTextContent("No matches found");
    expect(screen.queryByRole("option", { name: "Search further back" })).not.toBeInTheDocument();
    expect(onSearchFurtherBack).not.toHaveBeenCalled();

    rerender(search(history({ state: "completed", canSearchFurther: true, progressVersion: "progress-2" })));
    expect(screen.getByRole("option", { name: "Search further back" })).toBeInTheDocument();
    expect(onSearchFurtherBack).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("option", { name: "Search further back" }));
    await act(async () => {});
    expect(onSearchFurtherBack).toHaveBeenCalledOnce();
  });

  it("uses one pending history activation across keyboard and pointer and changes to Cancel", async () => {
    vi.useFakeTimers();
    const start = deferred<void>();
    const onSearchFurtherBack = vi.fn(() => start.promise);
    const { rerender } = render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} historySearch={history()} onSearchFurtherBack={onSearchFurtherBack} onCancelHistorySearch={async () => {}} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "missing" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.keyDown(input, { key: "End" });
    fireEvent.keyDown(input, { key: "Enter" });
    await act(async () => {});
    fireEvent.click(screen.getByRole("option", { name: "Search further back" }));
    expect(onSearchFurtherBack).toHaveBeenCalledOnce();
    rerender(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} historySearch={history({ state: "waitingForPhone", progressVersion: "progress-2" })} onSearchFurtherBack={onSearchFurtherBack} onCancelHistorySearch={async () => {}} />);
    expect(screen.getByRole("option", { name: "Cancel searching further back" })).toHaveAttribute("aria-disabled", "true");
    expect(input).toHaveAttribute("aria-activedescendant", expect.stringContaining("history"));
    await act(async () => start.resolve());
  });

  it("starts history only when Enter activates its default selected action", async () => {
    vi.useFakeTimers();
    const onSearchFurtherBack = vi.fn(async () => {});
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} historySearch={history()} onSearchFurtherBack={onSearchFurtherBack} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "missing" } });
    expect(onSearchFurtherBack).not.toHaveBeenCalled();
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(onSearchFurtherBack).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "Enter" });
    await act(async () => {});
    expect(onSearchFurtherBack).toHaveBeenCalledOnce();
  });

  it("cancels and retries history through the shared action path", async () => {
    vi.useFakeTimers();
    const cancel = vi.fn(async () => {});
    const retry = vi.fn(async () => {});
    const { rerender } = render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} historySearch={history({ state: "catchingUp" })} onSearchFurtherBack={retry} onCancelHistorySearch={cancel} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "missing" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("option", { name: "Cancel searching further back" }));
    await act(async () => {});
    expect(cancel).toHaveBeenCalledOnce();
    rerender(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} historySearch={history({ state: "blocked", reason: "offline", progressVersion: "progress-2" })} onSearchFurtherBack={retry} />);
    fireEvent.keyDown(input, { key: "End" });
    fireEvent.keyDown(input, { key: "Enter" });
    await act(async () => {});
    expect(retry).toHaveBeenCalledOnce();
  });

  it("keeps active history progress beside hits but hides it during composition and never cancels on Escape", async () => {
    vi.useFakeTimers();
    const cancel = vi.fn(async () => {});
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query)} historySearch={history({ state: "indexing" })} onSearchFurtherBack={async () => {}} onCancelHistorySearch={cancel} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "aurora" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    expect(screen.getByRole("option", { name: "Cancel searching further back" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /Aurora/ })).toBeInTheDocument();
    fireEvent.compositionStart(input);
    expect(screen.queryByRole("option", { name: "Cancel searching further back" })).not.toBeInTheDocument();
    fireEvent.compositionEnd(input);
    fireEvent.keyDown(input, { key: "Escape" });
    expect(cancel).not.toHaveBeenCalled();
  });

  it("refreshes a ready query only when the history index version changes", async () => {
    vi.useFakeTimers();
    const onSearch = vi.fn(async (request) => page(request.query, []));
    const { rerender } = render(<OmniSearch sessionKey="one" refreshKey={0} onActivate={() => {}} onSearch={onSearch} historySearch={history()} onSearchFurtherBack={async () => {}} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "missing" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} historySearch={history({ progressVersion: "progress-2" })} onSearchFurtherBack={async () => {}} />);
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(onSearch).toHaveBeenCalledOnce();
    rerender(<OmniSearch sessionKey="one" refreshKey={1} onActivate={() => {}} onSearch={onSearch} historySearch={history({ progressVersion: "progress-3", indexVersion: "index-2" })} onSearchFurtherBack={async () => {}} />);
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(onSearch).toHaveBeenCalledTimes(2);
  });

  it("coalesces one ready-index rerun when indexVersion changes during a deferred search", async () => {
    vi.useFakeTimers();
    const first = deferred<ReturnType<typeof page>>();
    const onSearch = vi.fn()
      .mockReturnValueOnce(first.promise)
      .mockImplementation(async (request) => page(request.query, [{ kind: "conversation", conversationId: "imported", title: "Imported", subtitle: "Ready" }]));
    const { rerender } = render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={onSearch} historySearch={history()} onSearchFurtherBack={async () => {}} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "missing" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    rerender(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={onSearch} historySearch={history({ indexVersion: "index-2", progressVersion: "progress-2" })} onSearchFurtherBack={async () => {}} />);
    await act(async () => first.resolve(page("missing", [])));
    await act(async () => { vi.advanceTimersByTime(1_000); });
    expect(onSearch).toHaveBeenCalledTimes(2);
    expect(onSearch).toHaveBeenLastCalledWith({ query: "missing", kind: "all", limit: 6 });
    expect(screen.getByRole("option", { name: /Imported/ })).toBeInTheDocument();
  });

  it.each([
    { hasMatches: true, queryCurrent: true },
    { hasMatches: false, queryCurrent: false },
  ])("hides unsupported history status when quiet-state requirements are not met", ({ hasMatches, queryCurrent }) => {
    expect(historyPresentation({ snapshot: history({ state: "failed", reason: "unsupported", canSearchFurther: false }), indexing: false, hasMatches, queryCurrent, hasCancel: false, startFailed: false })).toBeNull();
  });

  it("clears a local history failure before a successful retry with an unchanged snapshot", async () => {
    vi.useFakeTimers();
    const start = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(undefined);
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} historySearch={history()} onSearchFurtherBack={start} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "missing" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("option", { name: "Search further back" }));
    await act(async () => {});
    fireEvent.click(screen.getByRole("option", { name: "Retry loading history" }));
    await act(async () => {});
    expect(screen.getByRole("option", { name: "Search further back" })).toBeInTheDocument();
  });

  it("releases the history action guard when its callback throws synchronously", async () => {
    vi.useFakeTimers();
    const start = vi.fn().mockImplementationOnce(() => { throw new Error("sync"); }).mockResolvedValueOnce(undefined);
    render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query, [])} historySearch={history()} onSearchFurtherBack={start} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(input, { target: { value: "missing" } });
    await act(async () => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("option", { name: "Search further back" }));
    await act(async () => {});
    fireEvent.click(screen.getByRole("option", { name: "Retry loading history" }));
    await act(async () => {});
    expect(start).toHaveBeenCalledTimes(2);
  });

  it("reports only popup visibility transitions through a stable open callback", () => {
    const onOpenChange = vi.fn();
    const { rerender } = render(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query)} onOpenChange={onOpenChange} />);
    const input = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.focus(input);
    expect(onOpenChange).toHaveBeenLastCalledWith(true);
    rerender(<OmniSearch sessionKey="one" onActivate={() => {}} onSearch={async (request) => page(request.query)} onOpenChange={onOpenChange} />);
    expect(onOpenChange).toHaveBeenCalledTimes(1);
    fireEvent.pointerDown(document.body);
    expect(onOpenChange).toHaveBeenLastCalledWith(false);
    fireEvent.focus(input);
    expect(onOpenChange).toHaveBeenLastCalledWith(true);
    fireEvent.keyDown(input, { key: "Escape" });
    expect(onOpenChange).toHaveBeenLastCalledWith(false);
    expect(onOpenChange).toHaveBeenCalledTimes(4);
  });
});
