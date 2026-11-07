import { useEffect, useRef, useState, type RefObject } from "react";
import { Pencil, Trash2 } from "@peppy/desktop-ui";
import type { WebhookView } from "@peppy/desktop-ui/bridge-contracts";
import { errorCode } from "../bridge/bridgeErrors";
import { deliveryAnnouncement, webhookDeliveryView, webhookErrorMessage, webhookFilterSummary } from "./webhookDisplay";

export type WebhookListProps = {
  webhooks: WebhookView[];
  serverOffline: boolean;
  onEdit(webhook: WebhookView, trigger: HTMLElement): void;
  onSetPaused(webhook: WebhookView, paused: boolean): Promise<void>;
  onDelete(webhook: WebhookView): Promise<void>;
};

const withMember = (members: ReadonlySet<string>, id: string, present: boolean) => {
  const next = new Set(members);
  if (present) next.add(id);
  else next.delete(id);
  return next;
};

const withoutKey = (record: Record<string, string>, key: string) =>
  Object.fromEntries(Object.entries(record).filter(([entry]) => entry !== key));

export function WebhookList({ webhooks, serverOffline, onEdit, onSetPaused, onDelete }: WebhookListProps) {
  const [pausing, setPausing] = useState<ReadonlySet<string>>(() => new Set());
  const [pauseErrors, setPauseErrors] = useState<Record<string, string>>({});
  const [confirmingId, setConfirmingId] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState("");
  const deleteOpeners = useRef(new Map<string, HTMLButtonElement>());
  const cancelButton = useRef<HTMLButtonElement>(null);
  const confirmButton = useRef<HTMLButtonElement>(null);
  const restoreFocusId = useRef<string | null>(null);
  const now = Date.now();

  useEffect(() => {
    if (confirmingId) {
      cancelButton.current?.focus();
      return;
    }
    if (restoreFocusId.current) {
      deleteOpeners.current.get(restoreFocusId.current)?.focus();
      restoreFocusId.current = null;
    }
  }, [confirmingId]);

  useEffect(() => {
    if (deleteError && confirmingId) confirmButton.current?.focus();
  }, [deleteError, confirmingId]);

  const registerDeleteOpener = (id: string, element: HTMLButtonElement | null) => {
    if (element) deleteOpeners.current.set(id, element);
    else deleteOpeners.current.delete(id);
  };

  const togglePause = async (webhook: WebhookView) => {
    const verb = webhook.paused ? "resume" : "pause";
    setPauseErrors((current) => withoutKey(current, webhook.id));
    setPausing((current) => withMember(current, webhook.id, true));
    try {
      await onSetPaused(webhook, !webhook.paused);
    } catch {
      setPauseErrors((current) => ({ ...current, [webhook.id]: `Couldn't ${verb} ${webhook.name}. Try again.` }));
    } finally {
      setPausing((current) => withMember(current, webhook.id, false));
    }
  };

  const requestDelete = (id: string) => {
    setDeleteError("");
    setConfirmingId(id);
  };

  const cancelDelete = () => {
    restoreFocusId.current = confirmingId;
    setDeleteError("");
    setConfirmingId(null);
  };

  const confirmDelete = async (webhook: WebhookView) => {
    setDeleting(true);
    setDeleteError("");
    try {
      await onDelete(webhook);
      setConfirmingId(null);
    } catch (reason) {
      setDeleteError(webhookErrorMessage(errorCode(reason)));
    } finally {
      setDeleting(false);
    }
  };

  return (
    <ul id="webhook-list" className="webhook-list" aria-label="Webhooks">
      {webhooks.map((webhook) => (webhook.id === confirmingId ? (
        <WebhookDeleteConfirm
          key={webhook.id}
          webhook={webhook}
          deleting={deleting}
          error={deleteError}
          cancelRef={cancelButton}
          confirmRef={confirmButton}
          onCancel={cancelDelete}
          onConfirm={() => void confirmDelete(webhook)}
        />
      ) : (
        <WebhookRow
          key={webhook.id}
          webhook={webhook}
          now={now}
          serverOffline={serverOffline}
          pending={pausing.has(webhook.id)}
          pauseError={pauseErrors[webhook.id]}
          onEdit={onEdit}
          onTogglePause={() => void togglePause(webhook)}
          onRequestDelete={() => requestDelete(webhook.id)}
          onDeleteOpener={(element) => registerDeleteOpener(webhook.id, element)}
        />
      )))}
    </ul>
  );
}

type WebhookRowProps = {
  webhook: WebhookView;
  now: number;
  serverOffline: boolean;
  pending: boolean;
  pauseError?: string;
  onEdit(webhook: WebhookView, trigger: HTMLElement): void;
  onTogglePause(): void;
  onRequestDelete(): void;
  onDeleteOpener(element: HTMLButtonElement | null): void;
};

function WebhookRow({ webhook, now, serverOffline, pending, pauseError, onEdit, onTogglePause, onRequestDelete, onDeleteOpener }: WebhookRowProps) {
  const filter = webhookFilterSummary(webhook);
  const delivery = webhookDeliveryView(webhook, now);

  return (
    <li
      id={`webhook-${webhook.id}`}
      className="webhook-row"
      data-webhook={webhook.id}
      data-paused={webhook.paused}
      aria-busy={pending || undefined}
    >
      <label className="settings-switch">
        <input
          type="checkbox"
          role="switch"
          aria-label={`${webhook.name} active`}
          checked={!webhook.paused}
          disabled={serverOffline}
          aria-disabled={pending || undefined}
          onChange={() => {
            if (!pending) onTogglePause();
          }}
        />
        <span />
      </label>
      <div className="webhook-main">
        <strong className="webhook-name">
          <span className="webhook-name-text">{webhook.name}</span>
          {webhook.paused && <span className="webhook-badge">Paused</span>}
          <span
            className="webhook-name-status"
            role="img"
            aria-label={deliveryAnnouncement(webhook)}
            data-tone={delivery.tone}
          />
        </strong>
        <span className="webhook-url">{webhook.url}</span>
      </div>
      <span className="webhook-filter">
        {filter.lead}
        {filter.names && <>{" "}<b>{filter.names}</b></>}
      </span>
      <span className="webhook-last" data-tone={delivery.tone}>
        <span className="webhook-dot" aria-hidden="true" />
        <span className="webhook-last-text">{delivery.text}</span>
      </span>
      <div className="webhook-actions" data-webhook-actions={webhook.id}>
        <button
          type="button"
          className="webhook-icon-button"
          aria-label={`Edit ${webhook.name}`}
          disabled={pending}
          onClick={(event) => onEdit(webhook, event.currentTarget)}
        >
          <Pencil size={14} aria-hidden="true" />
        </button>
        <button
          ref={onDeleteOpener}
          type="button"
          className="webhook-icon-button"
          aria-label={`Delete ${webhook.name}`}
          disabled={serverOffline || pending}
          onClick={onRequestDelete}
        >
          <Trash2 size={14} aria-hidden="true" />
        </button>
      </div>
      {pauseError && <p className="settings-error webhook-row-error" role="alert">{pauseError}</p>}
    </li>
  );
}

type WebhookDeleteConfirmProps = {
  webhook: WebhookView;
  deleting: boolean;
  error: string;
  cancelRef: RefObject<HTMLButtonElement | null>;
  confirmRef: RefObject<HTMLButtonElement | null>;
  onCancel(): void;
  onConfirm(): void;
};

function WebhookDeleteConfirm({ webhook, deleting, error, cancelRef, confirmRef, onCancel, onConfirm }: WebhookDeleteConfirmProps) {
  const textId = `webhook-delete-text-${webhook.id}`;

  return (
    <li
      id={`webhook-${webhook.id}`}
      className="webhook-row webhook-row-confirm"
      data-webhook={webhook.id}
      data-webhook-confirm
      aria-busy={deleting || undefined}
    >
      <p id={textId} className="webhook-confirm-text">
        Delete <b>{webhook.name}</b>? It stops immediately; queued deliveries are dropped.
      </p>
      <button ref={cancelRef} type="button" className="secondary-button webhook-compact-button" disabled={deleting} onClick={onCancel}>
        Cancel
      </button>
      <button
        ref={confirmRef}
        type="button"
        className="destructive-button webhook-compact-button"
        aria-describedby={textId}
        disabled={deleting}
        onClick={onConfirm}
      >
        Delete
      </button>
      {error && <p className="settings-error webhook-row-error" role="alert">{error}</p>}
    </li>
  );
}
