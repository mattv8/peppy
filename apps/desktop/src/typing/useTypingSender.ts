import { useEffect, useRef, useState } from "react";
import type { DesktopBridge } from "@peppy/desktop-ui/bridge-contracts";
import { readShareTyping } from "../shell/layout-persistence";

const REFRESH_MS = 3_200;
const IDLE_STOP_MS = 6_000;

type TypingSenderInput = {
  bridge: DesktopBridge;
  conversationId?: string;
  current: boolean;
  existing: boolean;
  sendSupported: boolean;
  readOnly: boolean;
  visible: boolean;
  sharingEnabled: boolean;
};

export function useTypingSender(input: TypingSenderInput) {
  const [state, setState] = useState<"idle" | "active" | "suppressed">("idle");
  const activeConversationId = useRef<string | undefined>(undefined);
  const refreshTimer = useRef<number | undefined>(undefined);
  const idleTimer = useRef<number | undefined>(undefined);
  const lastInputAt = useRef(0);
  const lastStartAt = useRef(0);
  const inputRef = useRef(input);
  inputRef.current = input;

  const clearTimers = () => {
    if (refreshTimer.current !== undefined) window.clearInterval(refreshTimer.current);
    if (idleTimer.current !== undefined) window.clearTimeout(idleTimer.current);
    refreshTimer.current = undefined;
    idleTimer.current = undefined;
  };

  const stop = () => {
    const conversationId = activeConversationId.current;
    if (!conversationId) { setState("idle"); return; }
    activeConversationId.current = undefined;
    clearTimers();
    setState("idle");
    void inputRef.current.bridge.send_typing?.({ conversationId, state: "stopped" }).catch(() => undefined);
  };

  const canSend = () => {
    const { bridge, conversationId, current, existing, sendSupported, readOnly, visible, sharingEnabled } = inputRef.current;
    return Boolean(bridge.send_typing && conversationId && current && existing && sendSupported && !readOnly && visible && sharingEnabled && readShareTyping() && document.visibilityState === "visible");
  };

  const start = (conversationId: string) => {
    lastStartAt.current = Date.now();
    void inputRef.current.bridge.send_typing?.({ conversationId, state: "started" }).catch(() => undefined);
  };

  const refresh = () => {
    const conversationId = activeConversationId.current;
    if (!conversationId || !canSend()) return stop();
    if (Date.now() - lastInputAt.current >= REFRESH_MS) {
      if (refreshTimer.current !== undefined) window.clearInterval(refreshTimer.current);
      refreshTimer.current = undefined;
      return;
    }
    start(conversationId);
  };

  const noteInput = (text: string) => {
    if (!text.trim()) return stop();
    if (!canSend()) { setState("suppressed"); return; }
    const conversationId = inputRef.current.conversationId;
    if (!conversationId) return;
    lastInputAt.current = Date.now();
    if (!activeConversationId.current) {
      activeConversationId.current = conversationId;
      setState("active");
      start(conversationId);
      refreshTimer.current = window.setInterval(refresh, REFRESH_MS);
    } else if (refreshTimer.current === undefined) {
      if (Date.now() - lastStartAt.current >= REFRESH_MS) start(conversationId);
      refreshTimer.current = window.setInterval(refresh, REFRESH_MS);
    }
    if (idleTimer.current !== undefined) window.clearTimeout(idleTimer.current);
    idleTimer.current = window.setTimeout(stop, IDLE_STOP_MS);
  };

  useEffect(() => {
    const stopWhenHidden = () => {
      if (document.visibilityState === "hidden") stop();
    };
    const stopWhenSharingDisabled = (event: StorageEvent) => {
      if (event.key === "peppy.layout.v1" && !readShareTyping()) stop();
    };
    const stopOnComposerBlur = (event: FocusEvent) => {
      if ((event.target as HTMLElement | null)?.id === "composer-textarea") stop();
    };
    window.addEventListener("blur", stop);
    window.addEventListener("pagehide", stop);
    window.addEventListener("storage", stopWhenSharingDisabled);
    document.addEventListener("visibilitychange", stopWhenHidden);
    document.addEventListener("focusout", stopOnComposerBlur);
    return () => {
      window.removeEventListener("blur", stop);
      window.removeEventListener("pagehide", stop);
      window.removeEventListener("storage", stopWhenSharingDisabled);
      document.removeEventListener("visibilitychange", stopWhenHidden);
      document.removeEventListener("focusout", stopOnComposerBlur);
      stop();
    };
  }, [input.conversationId, input.current, input.existing, input.sendSupported, input.readOnly, input.visible, input.sharingEnabled, input.bridge]);

  return { noteInput, noteSend: stop, state };
}
