import { describe, expect, it } from "vitest";
import type { WebhookFilterEntry } from "@peppy/desktop-ui/bridge-contracts";
import { homeAssistant, zapier } from "./webhookTestFixtures";
import {
  draftFromWebhook,
  draftSignature,
  filterEntriesFromChipIds,
  filterRecipientChips,
  toWebhookInput,
  webhookDraftErrors,
  webhookInputFor,
  webhookUrlIsValid,
  type ChipSource,
  type WebhookDraft,
} from "./webhookForm";

const draft = (overrides: Partial<WebhookDraft> = {}): WebhookDraft => ({
  name: "",
  url: "https://example.com/hook",
  filterMode: "all",
  filter: [],
  paused: false,
  ...overrides,
});

const aliceEntry: WebhookFilterEntry = { address: "+12025550101", label: "Alice Chen", contactId: "contact-alice", bookId: "book-1" };

describe("toWebhookInput", () => {
  it("sends a blank name as null and trims the URL", () => {
    expect(toWebhookInput(draft({ name: "   ", url: " https://example.com/hook " }))).toEqual({
      name: null,
      url: "https://example.com/hook",
      events: ["message.received"],
      filterMode: "all",
      filter: [],
      paused: false,
    });
  });

  it("keeps filter entries only for filtered modes", () => {
    const only = toWebhookInput(draft({ name: "Team", filterMode: "only", filter: [aliceEntry] }));
    expect(only).toMatchObject({ name: "Team", filterMode: "only", filter: [aliceEntry] });

    const all = toWebhookInput(draft({ filterMode: "all", filter: [aliceEntry] }));
    expect(all.filter).toEqual([]);
  });
});

describe("webhookInputFor", () => {
  it("builds a full update for a derived-name webhook, sending the name as null", () => {
    expect(webhookInputFor(homeAssistant, true)).toEqual({
      name: null,
      url: homeAssistant.url,
      events: ["message.received"],
      filterMode: "all",
      filter: [],
      paused: true,
    });
  });

  it("keeps an explicit name and the filter for a filtered webhook", () => {
    expect(webhookInputFor(zapier, false)).toEqual({
      name: "Zapier: Alice & Bob",
      url: zapier.url,
      events: ["message.received"],
      filterMode: "only",
      filter: zapier.filter,
      paused: false,
    });
  });
});

describe("webhookUrlIsValid", () => {
  it.each(["https://example.com/hook", "http://192.168.1.10/hook"])("accepts %s", (value) => {
    expect(webhookUrlIsValid(value)).toBe(true);
  });

  it.each(["", "example.com/hook", "ftp://example.com", "https://", "not a url"])("rejects %j", (value) => {
    expect(webhookUrlIsValid(value)).toBe(false);
  });
});

describe("webhookDraftErrors", () => {
  it("accepts a valid draft with no filter", () => {
    expect(webhookDraftErrors(draft())).toEqual({});
  });

  it("flags an invalid URL", () => {
    expect(webhookDraftErrors(draft({ url: "nope" }))).toEqual({ url: "Enter a valid URL." });
  });

  it("allows exactly 80 name characters and rejects 81", () => {
    expect(webhookDraftErrors(draft({ name: "n".repeat(80) }))).toEqual({});
    expect(webhookDraftErrors(draft({ name: "n".repeat(81) }))).toEqual({ name: "Use 80 characters or fewer." });
  });

  it("requires at least one entry for filtered modes only", () => {
    expect(webhookDraftErrors(draft({ filterMode: "only" }))).toEqual({ filter: "Add at least one contact or number." });
    expect(webhookDraftErrors(draft({ filterMode: "except" }))).toEqual({ filter: "Add at least one contact or number." });
    expect(webhookDraftErrors(draft({ filterMode: "all" }))).toEqual({});
  });

  it("allows exactly 50 filter entries and rejects 51", () => {
    const entries = (count: number) => Array.from({ length: count }, (_, index) => entry(`+1202555${String(index).padStart(4, "0")}`));
    expect(webhookDraftErrors(draft({ filterMode: "only", filter: entries(50) }))).toEqual({});
    expect(webhookDraftErrors(draft({ filterMode: "only", filter: entries(51) }))).toEqual({ filter: "Use 50 contacts or numbers or fewer." });
  });
});

describe("draftFromWebhook and draftSignature", () => {
  it("starts a derived-name webhook with a blank name", () => {
    expect(draftFromWebhook(homeAssistant)).toMatchObject({ name: "", url: homeAssistant.url, filterMode: "all", paused: false });
    expect(draftFromWebhook(zapier).name).toBe("Zapier: Alice & Bob");
  });

  it("treats whitespace-only edits to text fields as no change", () => {
    const base = draftFromWebhook(zapier);
    expect(draftSignature({ ...base, name: `  ${base.name}  `, url: ` ${base.url} ` })).toBe(draftSignature(base));
  });

  it("detects name, URL, mode and filter changes", () => {
    const base = draftFromWebhook(zapier);
    const signature = draftSignature(base);
    expect(draftSignature({ ...base, name: "Renamed" })).not.toBe(signature);
    expect(draftSignature({ ...base, url: "https://example.com/other" })).not.toBe(signature);
    expect(draftSignature({ ...base, filterMode: "except" })).not.toBe(signature);
    expect(draftSignature({ ...base, filter: [aliceEntry] })).not.toBe(signature);
  });

  it("ignores the filter while the mode is Anyone", () => {
    const anyone = draft({ filterMode: "all", filter: [aliceEntry] });
    expect(draftSignature(anyone)).toBe(draftSignature(draft({ filterMode: "all", filter: [] })));
  });
});

describe("filterRecipientChips", () => {
  it("labels chips with the contact name or the formatted number", () => {
    expect(filterRecipientChips([aliceEntry, entry("+12025550127")])).toEqual([
      { id: "+12025550101", label: "Alice Chen" },
      { id: "+12025550127", label: "(202) 555-0127" },
    ]);
  });
});

describe("filterEntriesFromChipIds", () => {
  const sources = new Map<string, ChipSource>([
    ["+12025550199", { address: "+12025550199", displayName: "Jordan Lee", contactId: "contact-jordan", bookId: "book-2" }],
  ]);

  it("turns a chosen contact suggestion into a contact entry", () => {
    expect(filterEntriesFromChipIds(["+12025550199"], [], sources)).toEqual([
      { address: "+12025550199", label: "Jordan Lee", contactId: "contact-jordan", bookId: "book-2" },
    ]);
  });

  it("turns a typed number into an address-only entry, normalized when valid", () => {
    expect(filterEntriesFromChipIds(["+12025550127"], [], new Map())).toEqual([
      { address: "+12025550127", label: null, contactId: null, bookId: null },
    ]);
  });

  it("keeps the metadata of entries that were already in the filter", () => {
    expect(filterEntriesFromChipIds(["+12025550101", "+12025550199"], [aliceEntry], sources)).toEqual([
      aliceEntry,
      { address: "+12025550199", label: "Jordan Lee", contactId: "contact-jordan", bookId: "book-2" },
    ]);
  });

  it("drops blank ids", () => {
    expect(filterEntriesFromChipIds(["   "], [], new Map())).toEqual([]);
  });
});

function entry(address: string): WebhookFilterEntry {
  return { address, label: null, contactId: null, bookId: null };
}
