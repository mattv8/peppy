import { useCallback, useEffect, useState } from "react";

const DEFAULT_DURATION_MS = 4_000;

type Message = { text: string; durationMs: number; id: number };

/** Status text that clears itself after a short time. A newer message replaces the timer. */
export function useTransientMessage() {
  const [message, setMessage] = useState<Message>({ text: "", durationMs: DEFAULT_DURATION_MS, id: 0 });

  useEffect(() => {
    if (!message.text) return;
    const timer = setTimeout(() => {
      setMessage((current) => (current.id === message.id ? { ...current, text: "" } : current));
    }, message.durationMs);
    return () => clearTimeout(timer);
  }, [message]);

  const show = useCallback((text: string, durationMs = DEFAULT_DURATION_MS) => {
    setMessage((current) => ({ text, durationMs, id: current.id + 1 }));
  }, []);

  const clear = useCallback(() => {
    setMessage((current) => ({ ...current, text: "" }));
  }, []);

  return { text: message.text, show, clear };
}
