import { useEffect, useRef, useState, type RefObject } from "react";
import { createPortal } from "react-dom";
import { Plus } from "@peppy/desktop-ui";
import type { McpDeviceStatus, McpDeviceView, WebhookView } from "@peppy/desktop-ui/bridge-contracts";
import { isRemovedDevice } from "./mcpDisplay";
import { WebhookEditorModal } from "./WebhookEditorModal";
import { WebhookList } from "./WebhookList";
import {
  MAX_WEBHOOKS,
  OWNER_REQUIRED_CODE,
  WEBHOOK_LIMIT_MESSAGE,
  serverClientStatus,
  type StatusView,
} from "./webhookDisplay";
import { useServerClientWatch } from "./useServerClientWatch";
import { useTransientMessage } from "./useTransientMessage";
import { useWebhookSettings, type WebhookLoadError, type WebhookSettingsData } from "./useWebhookSettings";
import "./webhooks.css";

export type WebhookSettingsProps = {
  sessionKey: string;
  unlocked: boolean;
  serverOffline: boolean;
  onOpenDevices(): void;
};

const INTRO = "Send incoming messages to your own apps. Runs on your server, even when your devices are offline.";
const OFFLINE_ADD_MESSAGE = "Reconnect to the server to add webhooks.";
const SETUP_ADD_MESSAGE = "Set up the Peppy server client in Devices first.";
const OFFLINE_NOTE = "The server is offline. Changes resume when it reconnects.";
const DELETED_DURATION_MS = 4_000;

type Stage =
  | { kind: "locked" }
  | { kind: "loading" }
  | { kind: "error"; error: WebhookLoadError }
  | { kind: "owner" }
  | { kind: "setup" }
  | { kind: "unavailable" }
  | { kind: "ready"; device: McpDeviceView; webhooks: WebhookView[] };

/** A server client counts as set up when it is present and not removed. */
const hasServerClient = (status: McpDeviceStatus) => Boolean(status.device) && !isRemovedDevice(status.device);

function settingsStage(unlocked: boolean, data: WebhookSettingsData | null, loadError: WebhookLoadError | null): Stage {
  if (!unlocked) return { kind: "locked" };
  if (loadError?.code === OWNER_REQUIRED_CODE) return { kind: "owner" };
  if (loadError) return { kind: "error", error: loadError };
  if (!data) return { kind: "loading" };
  const device = data.status.device;
  if (!device || isRemovedDevice(device)) return { kind: "setup" };
  if (!data.list.available) return { kind: "unavailable" };
  return { kind: "ready", device, webhooks: data.list.webhooks };
}

export function WebhookSettings({ sessionKey, unlocked, serverOffline, onOpenDevices }: WebhookSettingsProps) {
  const settings = useWebhookSettings(sessionKey, unlocked);
  const stage = settingsStage(unlocked, settings.data, settings.loadError);
  const [editing, setEditing] = useState<WebhookView | "new" | null>(null);
  const notice = useTransientMessage();
  const opener = useRef<HTMLElement | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    setEditing(null);
  }, [sessionKey, unlocked]);

  /** While no server client is set up, re-check on an interval and load the list once one appears. */
  useServerClientWatch(
    stage.kind === "setup",
    async () => {
      const status = await settings.refreshStatus();
      return status !== null && hasServerClient(status);
    },
    () => void settings.reload(),
  );

  const openEditor = (target: WebhookView | "new", trigger: HTMLElement) => {
    opener.current = trigger;
    notice.clear();
    setEditing(target);
  };

  const deleteWebhook = async (webhook: WebhookView) => {
    await settings.deleteWebhook(webhook.id);
    notice.show(`Deleted ${webhook.name}.`, DELETED_DURATION_MS);
    headingRef.current?.focus();
  };

  const renderStage = () => {
    switch (stage.kind) {
      case "locked":
        return <p id="webhooks-locked" className="settings-note">Unlock this vault to manage webhooks.</p>;
      case "loading":
        return <p id="webhooks-loading" className="settings-note" role="status">Loading webhooks…</p>;
      case "error":
        return (
          <div id="webhooks-load-error-block" className="webhooks-error">
            <p id="webhooks-load-error" className="settings-error" role="alert">{stage.error.message}</p>
            <button id="webhooks-retry" type="button" className="secondary-button" onClick={() => void settings.reload()}>
              Retry
            </button>
          </div>
        );
      case "owner":
        return <p id="webhooks-owner-note" className="settings-note">Only vault owners can manage webhooks.</p>;
      case "setup":
        return (
          <>
            <WebhookHeader headingRef={headingRef} addDisabled addTitle={SETUP_ADD_MESSAGE} />
            <div id="webhooks-needs-server-client" className="webhooks-callout">
              <p>
                Webhooks run on the <b>Peppy server client</b>, the same always-on client that powers MCP. Turning it on lets
                your server operator hold this vault's keys. AI clients still need their own approval.
              </p>
              <button id="webhooks-setup-devices" type="button" className="primary-button webhook-compact-button" onClick={onOpenDevices}>
                Set up in Devices
              </button>
            </div>
          </>
        );
      case "unavailable":
        return <p id="webhooks-unavailable" className="settings-note">Webhooks are unavailable because this server isn't running the Peppy server client.</p>;
      case "ready":
        return (
          <WebhookReadyView
            device={stage.device}
            webhooks={stage.webhooks}
            serverOffline={serverOffline}
            headingRef={headingRef}
            onAdd={(trigger) => openEditor("new", trigger)}
            onEdit={openEditor}
            onSetPaused={settings.setPaused}
            onDelete={deleteWebhook}
          />
        );
    }
  };

  return (
    <div id="webhooks-settings" data-webhooks-stage={stage.kind}>
      {renderStage()}
      <p id="webhooks-notice" className="settings-note" role="status">{notice.text}</p>
      {editing !== null && createPortal(
        <WebhookEditorModal
          webhook={editing === "new" ? undefined : editing}
          serverOffline={serverOffline}
          opener={opener}
          onChanged={settings.upsertWebhook}
          onClose={() => setEditing(null)}
        />,
        document.body,
      )}
    </div>
  );
}

type WebhookHeaderProps = {
  headingRef: RefObject<HTMLHeadingElement | null>;
  addDisabled: boolean;
  addTitle?: string;
  onAdd?(trigger: HTMLElement): void;
};

function WebhookHeader({ headingRef, addDisabled, addTitle, onAdd }: WebhookHeaderProps) {
  return (
    <header className="webhooks-header">
      <div className="webhooks-heading">
        <h3 id="webhooks-heading" ref={headingRef} tabIndex={-1}>Webhooks</h3>
        <p className="settings-note">{INTRO}</p>
      </div>
      <button
        id="webhook-add"
        type="button"
        className="secondary-button webhook-compact-button"
        disabled={addDisabled}
        title={addTitle}
        onClick={(event) => onAdd?.(event.currentTarget)}
      >
        <Plus size={13} aria-hidden="true" />
        Add webhook
      </button>
    </header>
  );
}

type WebhookReadyViewProps = {
  device: McpDeviceView;
  webhooks: WebhookView[];
  serverOffline: boolean;
  headingRef: RefObject<HTMLHeadingElement | null>;
  onAdd(trigger: HTMLElement): void;
  onEdit(webhook: WebhookView, trigger: HTMLElement): void;
  onSetPaused(webhook: WebhookView, paused: boolean): Promise<void>;
  onDelete(webhook: WebhookView): Promise<void>;
};

function WebhookReadyView({ device, webhooks, serverOffline, headingRef, onAdd, onEdit, onSetPaused, onDelete }: WebhookReadyViewProps) {
  const atLimit = webhooks.length >= MAX_WEBHOOKS;
  const addDisabled = serverOffline || atLimit;
  const addTitle = atLimit ? WEBHOOK_LIMIT_MESSAGE : serverOffline ? OFFLINE_ADD_MESSAGE : undefined;
  const status: StatusView = serverClientStatus(device, Date.now());

  return (
    <>
      <WebhookHeader headingRef={headingRef} addDisabled={addDisabled} addTitle={addTitle} onAdd={onAdd} />
      <p id="webhooks-status" className="webhooks-status">
        <span className="webhook-dot" data-tone={status.tone} aria-hidden="true" />
        {status.text}
      </p>
      {serverOffline && <p id="webhooks-offline-note" className="settings-note">{OFFLINE_NOTE}</p>}
      {webhooks.length > 0 ? (
        <WebhookList
          webhooks={webhooks}
          serverOffline={serverOffline}
          onEdit={onEdit}
          onSetPaused={onSetPaused}
          onDelete={onDelete}
        />
      ) : (
        <p id="webhooks-empty" className="settings-note">No webhooks yet. Add one to send incoming messages to your own apps.</p>
      )}
    </>
  );
}
