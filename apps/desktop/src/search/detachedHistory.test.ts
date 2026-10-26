import { describe, expect, it } from "vitest";
import type { MessageView } from "../bridge";
import { boundedAroundMessages, mergeDetachedMessages, searchSessionKey } from "./detachedHistory";

const messages = (prefix: string, count: number): MessageView[] => Array.from({ length: count }, (_, index) => ({
  id: `${prefix}-${index}`,
  revision: "1",
  sender: "other",
  body: `${prefix}-${index}`,
  timestamp: String(index),
  attachments: [],
}));

describe("detached history bounds", () => {
  it("keeps the exact around target in a capped window", () => {
    const bounded = boundedAroundMessages(messages("row", 300), "row-5");
    expect(bounded).toHaveLength(150);
    expect(bounded.some(message => message.id === "row-5")).toBe(true);
  });

  it("keeps the oldest side for older paging and reports a dropped newer edge", () => {
    const merged = mergeDetachedMessages(messages("current", 150), messages("older", 50), "older");
    expect(merged.messages).toHaveLength(150);
    expect(merged.messages[0]?.id).toBe("older-0");
    expect(merged.droppedNewer).toBe(true);
  });

  it("includes the profile fingerprint in the transient search session identity", () => {
    const base = { connection: { origin: "https://example.test" }, mode: "browser", encryption: { state: "unlocked", profileFingerprint: "one" } };
    expect(searchSessionKey(base, false)).not.toBe(searchSessionKey({ ...base, encryption: { ...base.encryption, profileFingerprint: "two" } }, false));
  });
});
