import type { BrowserPort } from "../host.js";

export function port(): BrowserPort & { messages: unknown[]; emit(message: unknown): void; closed: boolean } {
  const result = {
    messages: [] as unknown[], closed: false, onmessage: null as BrowserPort["onmessage"],
    postMessage(message: unknown) { result.messages.push(message); },
    close() { result.closed = true; },
    emit(message: unknown) { result.onmessage?.({ data: message } as MessageEvent); },
  };
  return result;
}
