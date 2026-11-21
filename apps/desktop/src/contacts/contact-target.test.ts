// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import type { ContactView } from "../bridge/bridge";
import { findContactTarget } from "./contact-target";

const contact = { id: "target", bookId: "book", revision: "1", displayName: "Target", phones: [], emails: [], addresses: [] } satisfies ContactView;
const sort = { key: "given", direction: "desc" } as const;

describe("findContactTarget", () => {
  it("looks through the target in one call and preserves continuation", async () => {
    const list = vi.fn().mockResolvedValue({ contacts: [contact], total: 10, nextCursor: "next", throughFound: true });
    await expect(findContactTarget({ bookId: "book", contactId: "target" }, list, new AbortController().signal, undefined, sort))
      .resolves.toEqual({ state: "found", contact, contacts: [contact], total: 10, nextCursor: "next" });
    expect(list).toHaveBeenCalledOnce();
    expect(list).toHaveBeenCalledWith("book", { through: "target", sort });
  });

  it("reports missing targets and capped lookups separately", async () => {
    const list = vi.fn().mockResolvedValueOnce({ contacts: [], total: 0, throughFound: false })
      .mockResolvedValueOnce({ contacts: Array.from({ length: 200 }, () => contact), total: 2500, throughFound: false })
      .mockResolvedValueOnce({ contacts: Array.from({ length: 150 }, () => contact), total: 150, throughFound: false });
    const target = { bookId: "book", contactId: "missing" };
    await expect(findContactTarget(target, list, new AbortController().signal)).resolves.toEqual({ state: "unavailable" });
    await expect(findContactTarget(target, list, new AbortController().signal)).resolves.toEqual({ state: "oversized" });
    await expect(findContactTarget(target, list, new AbortController().signal)).resolves.toEqual({ state: "unavailable" });
  });

  it("discards results after cancellation or book removal", async () => {
    const controller = new AbortController();
    const list = vi.fn(async () => {
      controller.abort();
      return { contacts: [contact], total: 1, throughFound: true };
    });
    await expect(findContactTarget({ bookId: "book", contactId: "target" }, list, controller.signal)).resolves.toEqual({ state: "cancelled" });
    const removed = vi.fn(async () => ({ contacts: [contact], total: 1, throughFound: true }));
    await expect(findContactTarget({ bookId: "book", contactId: "target" }, removed, new AbortController().signal, () => false)).resolves.toEqual({ state: "cancelled" });
    expect(removed).not.toHaveBeenCalled();
  });
});
