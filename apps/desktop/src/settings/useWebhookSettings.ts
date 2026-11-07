import { useCallback, useEffect, useRef, useState } from "react";
import type { McpDeviceStatus, WebhookList, WebhookView } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "../bridge/bridge";
import { errorCode } from "../bridge/bridgeErrors";
import { webhookErrorMessage } from "./webhookDisplay";
import { webhookInputFor } from "./webhookForm";

export type WebhookSettingsData = { status: McpDeviceStatus; list: WebhookList };
export type WebhookLoadError = { code?: string; message: string };

const loadErrorFor = (reason: unknown): WebhookLoadError => {
  const code = errorCode(reason);
  return { code, message: webhookErrorMessage(code) };
};

function withWebhook(webhooks: WebhookView[], view: WebhookView): WebhookView[] {
  if (!webhooks.some((webhook) => webhook.id === view.id)) return [...webhooks, view];
  return webhooks.map((webhook) => (webhook.id === view.id ? view : webhook));
}

const replaceWebhook = (webhooks: WebhookView[], view: WebhookView) =>
  webhooks.map((webhook) => (webhook.id === view.id ? view : webhook));

/**
 * Loads the device status and webhook list for an unlocked session. Results from a superseded
 * load or a previous session are dropped, and in-flight mutations never touch a newer session.
 */
export function useWebhookSettings(sessionKey: string, unlocked: boolean) {
  const [data, setData] = useState<WebhookSettingsData | null>(null);
  const [loadError, setLoadError] = useState<WebhookLoadError | null>(null);
  const request = useRef(0);
  const session = useRef(0);

  const load = useCallback(async () => {
    const current = ++request.current;
    setLoadError(null);
    try {
      const [status, list] = await Promise.all([bridge.mcp_status(), bridge.webhooks_list()]);
      if (current === request.current) setData({ status, list });
    } catch (reason) {
      if (current === request.current) setLoadError(loadErrorFor(reason));
    }
  }, []);

  useEffect(() => {
    session.current += 1;
    request.current += 1;
    setData(null);
    setLoadError(null);
    if (unlocked) void load();
  }, [load, sessionKey, unlocked]);

  const updateList = useCallback((generation: number, update: (webhooks: WebhookView[]) => WebhookView[]) => {
    if (generation !== session.current) return;
    setData((current) => (current ? { ...current, list: { ...current.list, webhooks: update(current.list.webhooks) } } : current));
  }, []);

  /** Re-reads only the device status, keeping the loaded list. Returns null when the session changed meanwhile. */
  const refreshStatus = useCallback(async (): Promise<McpDeviceStatus | null> => {
    const generation = session.current;
    const status = await bridge.mcp_status();
    if (generation !== session.current) return null;
    setData((current) => (current ? { ...current, status } : current));
    return status;
  }, []);

  const beginEdit = useCallback(() => {
    const generation = session.current;
    return (view: WebhookView) => updateList(generation, (webhooks) => withWebhook(webhooks, view));
  }, [updateList]);

  /** Flips the switch at once; the server's saved view replaces the optimistic one, and a failure restores the original. */
  const setPaused = useCallback(async (webhook: WebhookView, paused: boolean) => {
    const generation = session.current;
    updateList(generation, (webhooks) => replaceWebhook(webhooks, { ...webhook, paused }));
    try {
      const saved = await bridge.webhook_update(webhook.id, webhookInputFor(webhook, paused));
      updateList(generation, (webhooks) => replaceWebhook(webhooks, saved));
    } catch (reason) {
      updateList(generation, (webhooks) => webhooks.map((current) =>
        current.id === webhook.id ? { ...current, paused: webhook.paused } : current,
      ));
      throw reason;
    }
  }, [updateList]);

  const deleteWebhook = useCallback(async (id: string) => {
    const generation = session.current;
    await bridge.webhook_delete(id);
    updateList(generation, (webhooks) => webhooks.filter((webhook) => webhook.id !== id));
  }, [updateList]);

  return {
    data,
    loadError,
    reload: load,
    refreshStatus,
    beginEdit,
    setPaused,
    deleteWebhook,
  };
}
