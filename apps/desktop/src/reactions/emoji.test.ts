import { describe, expect, it } from "vitest";
import { emojiGraphemes, firstEmoji, pickerHint } from "./emoji";

describe("firstEmoji", () => {
  it("keeps skin tones and ZWJ sequences as one grapheme", () => {
    expect(firstEmoji("👍🏽")).toBe("👍🏽");
    expect(firstEmoji("👨‍👩‍👧")).toBe("👨‍👩‍👧");
  });

  it("takes the first of several emoji and ignores trailing text", () => {
    expect(firstEmoji("😂😮")).toBe("😂");
    expect(firstEmoji("😂 lol")).toBe("😂");
    expect(emojiGraphemes("😂😮")).toEqual(["😂", "😮"]);
  });

  it("rejects text, digits and keycap-less symbols", () => {
    expect(firstEmoji("hello")).toBeUndefined();
    expect(firstEmoji("123")).toBeUndefined();
    expect(firstEmoji("#")).toBeUndefined();
  });
});

describe("pickerHint", () => {
  it("names the platform shortcut in the browser and the panel in the desktop shell", () => {
    expect(pickerHint({ platform: "macos" })).toBe("Press Control+Command+Space to open your emoji panel, then choose one.");
    expect(pickerHint({ platform: "windows", nativePicker: true })).toBe("Choose an emoji in the panel that opened. If nothing opened, press Windows key + period.");
  });

  it("tells the user the native open failed, with the shortcut substituted", () => {
    expect(pickerHint({ platform: "macos", nativePicker: true, failed: true })).toBe("Couldn't open the emoji panel. Press Control+Command+Space instead.");
    expect(pickerHint({ platform: "linux", nativePicker: true, failed: true })).toBe("Couldn't open the emoji panel. Right-click this field and choose Insert Emoji instead.");
  });

  it("asks for the keyboard emoji switch on touch", () => {
    expect(pickerHint({ platform: "macos", coarse: true })).toBe("Switch your keyboard to emoji and choose one.");
  });
});
