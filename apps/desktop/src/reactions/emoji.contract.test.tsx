import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { EMOJI_BYTE_LIMIT, firstEmoji, isEmojiGrapheme, isSingleEmojiValue } from "./emoji";
import { ReactionCapture } from "./ReactionCapture";

type EmojiContract = { accept: string[]; reject: string[] };

/**
 * The shared contract. No fallback: a missing file fails this suite, so the UI cannot drift from core and Go
 * without anyone noticing.
 */
const SHARED = resolve(__dirname, "../../../../packages/contracts/fixtures/reaction-emoji.json");
const contract: EmojiContract = JSON.parse(readFileSync(SHARED, "utf8"));

/**
 * Boundary cases from review round two. They are pinned here as well as in the shared file, so the boundary is
 * checked even while the shared file is being extended.
 */
const ROUND_TWO = {
  accept: ["👩🏽‍💻", "🏳️‍🌈"],
  reject: ["🇨", "☺︎", "👍\u200D", "\u200D👍", "👨🏽‍👩🏽‍👧🏽‍👦🏽"],
};

/** A pasted run: the first valid emoji is used, so these are the expected `firstEmoji` results. */
const PASTED_RUN_FIRST: Record<string, string | undefined> = {
  "👍👍": "👍",
  "\u200D👍": "👍",
  a: undefined,
  "": undefined,
};

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("guards an open emoji capture when sending becomes paused", () => {
  const onCommit = vi.fn();
  const { rerender } = render(<ReactionCapture route="native" platform="linux" coarse={false} onCommit={onCommit} onBack={vi.fn()} announce={vi.fn()} />);
  rerender(<ReactionCapture route="native" platform="linux" coarse={false} writesPaused onCommit={onCommit} onBack={vi.fn()} announce={vi.fn()} />);
  const input = screen.getByRole("textbox", { name: "Emoji reaction" });
  expect(input).toHaveAttribute("aria-disabled", "true");
  fireEvent.input(input, { target: { value: "👍" } });
  expect(onCommit).not.toHaveBeenCalled();
});

describe("emoji contract (shared fixture)", () => {
  it.each(contract.accept)("accepts %s as one emoji", (emoji) => {
    expect(isSingleEmojiValue(emoji)).toBe(true);
    expect(firstEmoji(emoji)).toBe(emoji);
  });

  it.each(contract.reject)("rejects %j as an emoji value", (value) => {
    expect(isSingleEmojiValue(value)).toBe(false);
    expect(firstEmoji(value)).toBe(PASTED_RUN_FIRST[value] ?? undefined);
  });

  it("the shared file lists the boundary cases this round defines", () => {
    expect(contract.accept).toEqual(expect.arrayContaining(["👍", "❤️", "🙏", "👍🏽", "👨‍👩‍👧", "🇨🇦", "☺️"]));
    expect(contract.reject).toEqual(expect.arrayContaining(["1️⃣", "#️⃣", "a", "👍👍", ""]));
  });
});

describe("emoji contract (review round 2 boundaries, pinned)", () => {
  it.each(ROUND_TWO.accept)("accepts %s", (emoji) => {
    expect(isSingleEmojiValue(emoji)).toBe(true);
    expect(firstEmoji(emoji)).toBe(emoji);
  });

  it.each(ROUND_TWO.reject)("rejects %j", (value) => {
    expect(isSingleEmojiValue(value)).toBe(false);
  });

  it("refuses a single regional indicator, a text-presentation selector, a trailing joiner and an over-long sequence as graphemes", () => {
    expect(isEmojiGrapheme("🇨")).toBe(false);
    expect(isEmojiGrapheme("☺︎")).toBe(false);
    expect(isEmojiGrapheme("👍\u200D")).toBe(false);
    expect(isEmojiGrapheme("👨🏽‍👩🏽‍👧🏽‍👦🏽")).toBe(false);
  });

  it("uses a 32 UTF-8 byte limit, counted in bytes", () => {
    expect(EMOJI_BYTE_LIMIT).toBe(32);
    expect(new TextEncoder().encode("👨🏽‍👩🏽‍👧🏽‍👦🏽").length).toBeGreaterThan(EMOJI_BYTE_LIMIT);
    expect(new TextEncoder().encode("👩🏽‍💻").length).toBeLessThanOrEqual(EMOJI_BYTE_LIMIT);
  });

  it("rejects a keycap and a subdivision flag outright, and a bare digit is text", () => {
    expect(isEmojiGrapheme("1️⃣")).toBe(false);
    expect(isEmojiGrapheme("🏴󠁧󠁢󠁳󠁣󠁴󠁿")).toBe(false);
    expect(firstEmoji("5")).toBeUndefined();
  });
});

describe("capture field against the contract", () => {
  it("reports a rejected keycap as text and commits nothing", () => {
    const onCommit = vi.fn();
    const announce = vi.fn();
    render(<ReactionCapture route="native" platform="macos" coarse={false} onCommit={onCommit} onBack={() => undefined} announce={announce} />);
    fireEvent.input(screen.getByRole("textbox", { name: "Emoji reaction" }), { target: { value: "1️⃣" } });
    expect(onCommit).not.toHaveBeenCalled();
    expect(announce).toHaveBeenCalledWith("Pick an emoji, not text.");
  });

  it("commits an accepted flag as one value", () => {
    const onCommit = vi.fn();
    render(<ReactionCapture route="native" platform="macos" coarse={false} onCommit={onCommit} onBack={() => undefined} announce={() => undefined} />);
    fireEvent.input(screen.getByRole("textbox", { name: "Emoji reaction" }), { target: { value: "🇨🇦" } });
    expect(onCommit).toHaveBeenCalledWith("🇨🇦", false);
  });

  it("takes the first valid emoji of a pasted run, and says so", () => {
    const onCommit = vi.fn();
    render(<ReactionCapture route="native" platform="macos" coarse={false} onCommit={onCommit} onBack={() => undefined} announce={() => undefined} />);
    fireEvent.input(screen.getByRole("textbox", { name: "Emoji reaction" }), { target: { value: "🇨\u200D👍🏽" } });
    // The single indicator is refused; the first valid grapheme is the skin-toned thumb.
    expect(onCommit).toHaveBeenCalledWith("👍🏽", false);
  });
});
