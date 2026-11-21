import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  NAME_FORMAT_STORAGE_KEY,
  formatContactName,
  persistNameFormat,
  readNameFormat,
} from "./contact-name-format";

function memoryStorage(): Storage {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
    clear: () => values.clear(),
    key: (index: number) => [...values.keys()][index] ?? null,
    get length() { return values.size; },
  };
}

describe("formatContactName", () => {
  const both = { displayName: "Ada Lovelace", givenName: "Ada", familyName: "Lovelace" };

  it("keeps the phone display name first-last", () => {
    expect(formatContactName(both, "first-last")).toBe("Ada Lovelace");
  });

  it("orders given and family names last-first with a comma", () => {
    expect(formatContactName(both, "last-first")).toBe("Lovelace, Ada");
  });

  it.each(["first-last", "last-first"] as const)("keeps a custom display name such as a nickname (%s)", format => {
    expect(formatContactName({ displayName: "Becca (CFA)", givenName: "Rebecca", familyName: "Cole" }, format))
      .toBe(format === "last-first" ? "Cole, Rebecca" : "Becca (CFA)");
    expect(formatContactName({ displayName: "Mom", givenName: "Ada" }, format)).toBe("Mom");
  });

  it("joins given and family first-last when there is no display name", () => {
    expect(formatContactName({ displayName: "", givenName: "Ada", familyName: "Lovelace" }, "first-last")).toBe("Ada Lovelace");
    expect(formatContactName({ displayName: "  ", givenName: " Ada ", familyName: " Lovelace\t" }, "first-last")).toBe("Ada Lovelace");
  });

  it("trims whitespace around structured parts before joining", () => {
    const padded = { displayName: "", givenName: "  Ada ", familyName: " Lovelace\t" };
    expect(formatContactName(padded, "first-last")).toBe("Ada Lovelace");
    expect(formatContactName(padded, "last-first")).toBe("Lovelace, Ada");
  });

  it("shows only the given name alone when there is no family name", () => {
    expect(formatContactName({ displayName: "", givenName: "Ada" }, "first-last")).toBe("Ada");
    expect(formatContactName({ displayName: "Ada", givenName: "Ada" }, "last-first")).toBe("Ada");
  });

  it("shows only the family name alone with no comma", () => {
    expect(formatContactName({ displayName: "", familyName: "Lovelace" }, "first-last")).toBe("Lovelace");
    expect(formatContactName({ displayName: "Lovelace", familyName: "Lovelace" }, "last-first")).toBe("Lovelace");
  });

  it("treats whitespace-only parts as missing", () => {
    expect(formatContactName({ displayName: "", givenName: "Ada", familyName: "   " }, "last-first")).toBe("");
    expect(formatContactName({ displayName: "Ada", givenName: " ", familyName: "Lovelace" }, "last-first")).toBe("Ada");
  });

  it.each(["first-last", "last-first"] as const)("keeps the display name when neither part is present (%s)", format => {
    expect(formatContactName({ displayName: "  Team Chat  " }, format)).toBe("  Team Chat  ");
    expect(formatContactName({ displayName: "Mum", givenName: "  ", familyName: "" }, format)).toBe("Mum");
  });
});

describe("name format storage", () => {
  beforeEach(() => {
    vi.stubGlobal("localStorage", memoryStorage());
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("defaults to first-last when nothing is stored", () => {
    expect(readNameFormat()).toBe("first-last");
  });

  it("reads a stored last-first choice", () => {
    localStorage.setItem(NAME_FORMAT_STORAGE_KEY, "last-first");
    expect(readNameFormat()).toBe("last-first");
  });

  it("falls back to the default for an invalid stored value", () => {
    localStorage.setItem(NAME_FORMAT_STORAGE_KEY, "surname-first");
    expect(readNameFormat()).toBe("first-last");
  });

  it("persists the chosen format under the versioned key", () => {
    persistNameFormat("last-first");
    expect(localStorage.getItem("peppy.contacts.nameFormat.v1")).toBe("last-first");
    expect(readNameFormat()).toBe("last-first");
  });

  it("falls back to the default when storage cannot be read", () => {
    vi.spyOn(localStorage, "getItem").mockImplementation(() => { throw new Error("denied"); });
    expect(readNameFormat()).toBe("first-last");
  });

  it("does not throw when storage cannot be written", () => {
    vi.spyOn(localStorage, "setItem").mockImplementation(() => { throw new Error("quota"); });
    expect(() => persistNameFormat("last-first")).not.toThrow();
  });
});
