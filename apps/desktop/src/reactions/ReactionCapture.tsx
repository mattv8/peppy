import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { ChevronLeft } from "@peppy/desktop-ui";
import { emojiGraphemes, pickerHint, type ReactionPlatform } from "./emoji";

export const INVALID_CAPTURE_COPY = "Pick an emoji, not text.";

export type ReactionCaptureProps = {
  route: "native" | "text";
  platform: ReactionPlatform;
  coarse: boolean;
  /** Present only in the desktop shell; the browser variant shows the shortcut instead. */
  openPicker?: () => Promise<void>;
  /** `multiple` is set when the input held more than one emoji; the first was used. */
  onCommit(emoji: string, multiple: boolean): void;
  onBack(): void;
  announce(text: string, alert?: boolean): void;
};

/**
 * The capture field for the OS emoji panel. Typed or pasted text is reduced to its first emoji grapheme.
 * Blur never dismisses: the OS panel takes window focus, and the field takes it back when the window refocuses.
 */
export function ReactionCapture({ route, platform, coarse, openPicker, onCommit, onBack, announce }: ReactionCaptureProps) {
  const input = useRef<HTMLInputElement>(null);
  const [invalid, setInvalid] = useState(false);
  const [failed, setFailed] = useState(false);
  const nativePicker = Boolean(openPicker);
  const hint = pickerHint({ platform, coarse, nativePicker, failed });
  const sentence = route === "text" ? " Sends as a text message." : "";

  useEffect(() => {
    const field = input.current;
    field?.focus();
    const returnFocus = () => { if (document.activeElement !== input.current) input.current?.focus(); };
    window.addEventListener("focus", returnFocus);
    // The OS panel only inserts into a focused field, so it opens after focus is confirmed for one frame.
    const frame = requestAnimationFrame(() => {
      if (!openPicker || document.activeElement !== input.current) return;
      openPicker().catch(() => setFailed(true));
    });
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("focus", returnFocus);
    };
  }, [openPicker]);

  const accept = (value: string) => {
    if (value === "") return;
    const emojis = emojiGraphemes(value);
    if (!emojis.length) {
      setInvalid(true);
      announce(INVALID_CAPTURE_COPY);
      if (input.current) input.current.value = "";
      return;
    }
    setInvalid(false);
    onCommit(emojis[0], emojis.length > 1);
  };

  const onInput = (event: FormEvent<HTMLInputElement>) => {
    const native = event.nativeEvent as InputEvent;
    if (native.isComposing) return;
    accept(event.currentTarget.value);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Escape") {
      // The first Escape backs out of capture; the bar handles the second.
      event.preventDefault();
      event.stopPropagation();
      onBack();
    }
  };

  return <div data-reaction-capture>
    <div data-reaction-capture-row>
      <button type="button" data-reaction-back aria-label="Back to quick reactions" onClick={onBack}><ChevronLeft size={16} aria-hidden /></button>
      <input
        id="message-reaction-capture"
        ref={input}
        type="text"
        placeholder="Emoji"
        aria-label="Emoji reaction"
        aria-describedby="message-reaction-capture-hint"
        aria-invalid={invalid || undefined}
        autoComplete="off"
        autoCorrect="off"
        spellCheck={false}
        enterKeyHint="done"
        onInput={onInput}
        onCompositionEnd={(event) => accept(event.currentTarget.value)}
        onKeyDown={onKeyDown}
      />
    </div>
    <p id="message-reaction-capture-hint" data-reaction-capture-hint={failed ? "failed" : undefined}>{invalid ? INVALID_CAPTURE_COPY : `${hint}${sentence}`}</p>
  </div>;
}
