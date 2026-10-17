import { describe, expect, it } from "vitest";
import {
  AVATAR_PALETTE_SIZE,
  contactSeed,
  conversationAvatar,
  fnv1a32,
  initials,
  paletteIndex,
  seedKey,
} from "./avatar";

describe("seedKey", () => {
  it.each([
    ["+18015551234", "+18015551234", "6bbd46e1", 81],
    ["(801) 555-1234", "+18015551234", "6bbd46e1", 81],
    ["801-555-1234", "+18015551234", "6bbd46e1", 81],
    ["1 801 555 1234", "+18015551234", "6bbd46e1", 81],
    ["  +1 (801) 555-1234 ", "+18015551234", "6bbd46e1", 81],
    ["5556", "5556", "d76d9558", 96],
    ["+15555550199", "+15555550199", "b855db52", 90],
    ["+44 20 7946 0958", "+442079460958", "63b40aa8", 112],
    ["020 7946 0958", "02079460958", "3bec62f1", 49],
    ["AMAZON", "amazon", "0e9c0ded", 45],
    ["Amazon", "amazon", "0e9c0ded", 45],
    ["alerts@bank.example", "alerts@bank.example", "1f64b798", 16],
    ["Ünïcode Name", "Ünïcode name", "d274dcaa", 26],
  ])("normalizes %j", (address, expectedKey, expectedHash, expectedIndex) => {
    const key = seedKey(address);

    expect(key).toBe(expectedKey);
    expect(fnv1a32(key).toString(16).padStart(8, "0")).toBe(expectedHash);
    expect(paletteIndex(key)).toBe(expectedIndex);
  });

  it("returns an empty key for empty input", () => {
    expect(seedKey("")).toBe("");
  });
});

describe("fnv1a32", () => {
  it("hashes UTF-8 text with FNV-1a", () => {
    expect(fnv1a32("").toString(16)).toBe("811c9dc5");
    expect(fnv1a32("a").toString(16)).toBe("e40c292c");
  });
});

describe("conversationAvatar", () => {
  it.each([
    [{ id: "c1", name: "+18015551234" }, "+18015551234", "person", 81],
    [
      { id: "c2", name: "(801) 555-1234, +15555550199" },
      "group:+15555550199,+18015551234",
      "group",
      67,
    ],
    [
      {
        id: "c3",
        name: "+15555550199",
        participants: ["+15555550199", "+18015551234"],
      },
      "group:+15555550199,+18015551234",
      "group",
      67,
    ],
    [{ id: "c4", name: "New message", recipientless: true }, "conversation:c4", "person", 115],
    [{ id: "c5", name: "", participants: [] }, "conversation:c5", "person", 56],
    [
      { id: "c6", name: "x", participants: ["😀", "豈"] },
      "group:豈,😀",
      "group",
      84,
    ],
  ])("derives identity for %j", (input, seed, kind, index) => {
    const avatar = conversationAvatar(input);

    expect(avatar).toEqual({ seed, kind });
    expect(paletteIndex(avatar.seed)).toBe(index);
  });
});

describe("contactSeed", () => {
  it("uses the first phone with a non-empty normalized key", () => {
    expect(
      contactSeed([{ number: "" }, { number: "(801) 555-1234" }, { number: "5556" }], "c1"),
    ).toBe("+18015551234");
  });

  it.each([
    [[], "c2"],
    [undefined, "c3"],
    [[{ number: "" }], "c4"],
  ])("falls back to contact ID for %j", (phones, contactId) => {
    expect(contactSeed(phones, contactId)).toBe(`contact:${contactId}`);
  });
});

describe("initials", () => {
  it.each([
    ["Ada Lovelace", 2, "AL"],
    ["Ada\u00a0Lovelace", 2, "AL"],
    ["ß x", 1, "S"],
    ["ß x", 2, "SS"],
    ["+18016944617", 2, ""],
    ["  7 Bob  Élodie", 2, "BÉ"],
    ["李 小龍", 2, "李小"],
    ["straße", 2, "S"],
    ["Ada", 2, "A"],
  ])("extracts initials from %j", (name, max, expected) => {
    expect(initials(name, max)).toBe(expected);
  });
});

describe("paletteIndex", () => {
  it("always returns a palette index", () => {
    for (const seed of ["", "alice", "😀", "group:豈,😀"]) {
      expect(paletteIndex(seed)).toBeGreaterThanOrEqual(0);
      expect(paletteIndex(seed)).toBeLessThan(AVATAR_PALETTE_SIZE);
    }
  });
});
