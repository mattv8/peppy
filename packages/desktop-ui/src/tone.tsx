import { createContext, useContext, type CSSProperties, type ReactNode } from "react";
import { AVATAR_PALETTE_SIZE, paletteIndex, seedKey } from "./avatar";

export type ToneOverrides = Readonly<Record<string, number>>;

const ToneOverridesContext = createContext<ToneOverrides>({});

function isPaletteSlot(value: unknown): value is number {
  return Number.isInteger(value) && (value as number) >= 0 && (value as number) < AVATAR_PALETTE_SIZE;
}

export function toneIndex(seed: string, overrides: ToneOverrides = {}): number {
  if (Object.hasOwn(overrides, seed)) {
    const override = overrides[seed];
    if (isPaletteSlot(override)) {
      return override;
    }
  }
  return paletteIndex(seed);
}

export function incomingToneSeed(identity: string, senderAddress?: string): string {
  return senderAddress ? seedKey(senderAddress) || identity : identity;
}

export function toneStyle(seed: string, overrides: ToneOverrides = {}): CSSProperties {
  const index = toneIndex(seed, overrides);
  return {
    "--peppy-tone": `var(--peppy-avatar-${index}, var(--peppy-accent))`,
    "--peppy-tone-text": "var(--peppy-avatar-glyph, var(--peppy-accent-text))",
  } as CSSProperties;
}

export function ToneOverridesProvider({ overrides = {}, children }: { overrides?: ToneOverrides; children: ReactNode }) {
  return <ToneOverridesContext.Provider value={overrides}>{children}</ToneOverridesContext.Provider>;
}

export function useToneStyle(seed: string): CSSProperties {
  return toneStyle(seed, useContext(ToneOverridesContext));
}

export function useToneOverrides(): ToneOverrides {
  return useContext(ToneOverridesContext);
}
