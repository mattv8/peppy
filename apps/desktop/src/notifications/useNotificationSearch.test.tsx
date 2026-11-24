import "@testing-library/jest-dom/vitest";
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useNotificationSearch } from "./useNotificationSearch";

afterEach(cleanup);

const notification = {
  target: { sourceDeviceId: "phone-1", notificationKey: "notice-1", lifetime: "7" },
  packageName: "com.example.chat",
  appName: "Chat",
  title: "Searchable",
  text: "body",
  postedAt: Date.now(),
  dismissible: true,
  seen: false,
  dismissalPending: false,
};

it("keeps results stable across callback replacement while using the latest callbacks", () => {
  const onOpenHit = vi.fn();
  const onShowAll = vi.fn();
  const onUnavailable = vi.fn();
  const options = {
    active: false,
    locked: false,
    notifications: [notification],
    filters: [],
    sources: [{ id: "phone-1", name: "Phone" }],
    displayTitle: (title: string) => title,
    onOpenHit,
    onShowAll,
    onUnavailable,
  };
  const { result, rerender } = renderHook(props => useNotificationSearch(props), { initialProps: options });
  const results = result.current.notificationResults!;
  const nextOpen = vi.fn();
  const nextShowAll = vi.fn();
  rerender({ ...options, onOpenHit: nextOpen, onShowAll: nextShowAll, onUnavailable: vi.fn() });

  expect(result.current.notificationResults).toBe(results);
  results.onActivate(results.search("searchable").hits[0].key);
  results.onShowAll("searchable");
  expect(nextOpen).toHaveBeenCalledWith(notification.target);
  expect(nextShowAll).toHaveBeenCalledWith("searchable");
});

it("rebuilds results when contact names resolve for an unchanged query", () => {
  const options = {
    active: false,
    locked: false,
    notifications: [{ ...notification, title: "+15555550100" }],
    filters: [],
    sources: [{ id: "phone-1", name: "Phone" }],
    displayTitle: (title: string) => title,
    onOpenHit: vi.fn(),
    onShowAll: vi.fn(),
    onUnavailable: vi.fn(),
  };
  const { result, rerender } = renderHook(props => useNotificationSearch(props), { initialProps: options });
  expect(result.current.notificationResults!.search("morgan").total).toBe(0);

  rerender({ ...options, displayTitle: (title: string) => title === "+15555550100" ? "Morgan" : title });

  const search = result.current.notificationResults!.search("morgan");
  expect(search.total).toBe(1);
  expect(search.hits[0].title.map(segment => segment.text).join("")).toBe("Morgan");
});
