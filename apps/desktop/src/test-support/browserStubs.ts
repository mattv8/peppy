import { onTestFinished, vi } from "vitest";

/** Replaces a window property for the current test and restores the original when the test finishes. */
export function replaceWindowProperty(name: string, value: unknown, writable = true): void {
  const original = Object.getOwnPropertyDescriptor(window, name);
  Object.defineProperty(window, name, { configurable: true, writable, value });
  onTestFinished(() => {
    if (original) Object.defineProperty(window, name, original);
    else Reflect.deleteProperty(window, name);
  });
}

export function stubMatchMedia(matching: readonly string[]): void {
  replaceWindowProperty("matchMedia", vi.fn((query: string) => ({
    matches: matching.includes(query), media: query,
    addEventListener: vi.fn(), removeEventListener: vi.fn(),
  })));
}

export function stubIntersectionObserver(observer: unknown): void {
  replaceWindowProperty("IntersectionObserver", observer);
}
