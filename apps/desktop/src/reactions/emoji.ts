export const QUICK_REACTIONS = [
  { emoji: "👍", name: "Thumbs up", hook: "thumbs-up" },
  { emoji: "❤️", name: "Heart", hook: "heart" },
  { emoji: "😂", name: "Laughing", hook: "laughing" },
  { emoji: "😮", name: "Surprised", hook: "surprised" },
  { emoji: "😢", name: "Sad", hook: "sad" },
  { emoji: "🙏", name: "Folded hands", hook: "folded-hands" },
] as const;

export type ReactionPlatform = "macos" | "windows" | "linux";

/**
 * The emoji contract, shared with packages/contracts/fixtures/reaction-emoji.json and enforced identically by core
 * (reaction_validation.rs) and the Go link. One grapheme of at most 32 UTF-8 bytes that either contains an
 * Extended_Pictographic character or is a pair of regional indicators, with no U+FE0E (text presentation), no
 * leading or trailing U+200D, no keycap (U+20E3) and no tag characters (subdivision flags).
 */
export const EMOJI_BYTE_LIMIT = 32;
const PICTOGRAPHIC_START = /^\p{Extended_Pictographic}/u;
const FLAG = /^\p{Regional_Indicator}{2}$/u;
const TEXT_PRESENTATION = /\uFE0E/u;
const JOINER_AT_EDGE = /^\u200D|\u200D$/u;
const KEYCAP = /\u20E3/u;
const TAG_CHARACTERS = /[\u{E0020}-\u{E007F}]/u;

export function isEmojiGrapheme(segment: string): boolean {
  if (!PICTOGRAPHIC_START.test(segment) && !FLAG.test(segment)) return false;
  if (new TextEncoder().encode(segment).length > EMOJI_BYTE_LIMIT) return false;
  return !TEXT_PRESENTATION.test(segment) && !JOINER_AT_EDGE.test(segment) && !KEYCAP.test(segment) && !TAG_CHARACTERS.test(segment);
}

/** True only when the whole value is exactly one emoji grapheme (what a reaction sends). */
export function isSingleEmojiValue(value: string): boolean {
  const parts = graphemes(value);
  return parts.length === 1 && isEmojiGrapheme(parts[0]);
}

/** The value split into extended grapheme clusters (`Intl.Segmenter` where available). */
export function graphemes(value: string): string[] {
  if (typeof Intl.Segmenter !== "function") return Array.from(value);
  return Array.from(new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(value), (part) => part.segment);
}

/** Every emoji grapheme in order. Skin tones and ZWJ sequences stay whole because they are one grapheme. */
export function emojiGraphemes(value: string): string[] {
  return graphemes(value).filter(isEmojiGrapheme);
}

export function firstEmoji(value: string): string | undefined {
  return emojiGraphemes(value)[0];
}

const SHORTCUT: Record<Exclude<ReactionPlatform, "linux">, string> = {
  macos: "Control+Command+Space",
  windows: "Windows key + period",
};

export type PickerHintOptions = {
  platform: ReactionPlatform;
  coarse?: boolean;
  /** The OS panel was asked to open (desktop shell). */
  nativePicker?: boolean;
  /** The OS panel could not be opened. */
  failed?: boolean;
};

export function pickerHint({ platform, coarse = false, nativePicker = false, failed = false }: PickerHintOptions): string {
  if (coarse) return "Switch your keyboard to emoji and choose one.";
  if (platform === "linux") {
    return failed
      ? "Couldn't open the emoji panel. Right-click this field and choose Insert Emoji instead."
      : "Right-click this field and choose Insert Emoji, or paste an emoji.";
  }
  const shortcut = SHORTCUT[platform];
  if (failed) return `Couldn't open the emoji panel. Press ${shortcut} instead.`;
  return nativePicker
    ? `Choose an emoji in the panel that opened. If nothing opened, press ${shortcut}.`
    : `Press ${shortcut} to open your emoji panel, then choose one.`;
}
