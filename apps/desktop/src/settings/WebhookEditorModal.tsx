import { useEffect, useRef, useState, type RefObject } from "react";
import { Modal, RecipientChipInput, Send } from "@peppy/desktop-ui";
import type { WebhookView } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "../bridge/bridge";
import { errorCode } from "../bridge/bridgeErrors";
import { searchRecipientSuggestions, type RecipientSource } from "../contacts/recipientSearch";
import { WebhookFilterModeControl } from "./WebhookFilterModeControl";
import { WebhookSecretField } from "./WebhookSecretField";
import { useWebhookSecret } from "./useWebhookSecret";
import {
  webhookErrorMessage,
  webhookFieldForCode,
  webhookNamePlaceholder,
  webhookTestResultView,
  type StatusView,
  type WebhookField,
} from "./webhookDisplay";
import {
  draftFromWebhook,
  draftSignature,
  filterEntriesFromChipIds,
  filterRecipientChips,
  toWebhookInput,
  webhookDraftErrors,
  type WebhookDraft,
  type WebhookDraftErrors,
} from "./webhookForm";

export type WebhookEditorModalProps = {
  /** The saved webhook being edited; undefined when adding a new one. */
  webhook?: WebhookView;
  serverOffline: boolean;
  opener: RefObject<HTMLElement | null>;
  onChanged(webhook: WebhookView): void;
  onClose(): void;
};

const FORM_ID = "webhook-editor-form";
const FIELD_ORDER: WebhookField[] = ["name", "url", "filter"];
const HINT = "Pick contacts or type a phone number. A contact chip matches all of that contact's numbers.";
const DISCARD_NOTE = "Unsaved changes. Save, or select Close again to discard them.";
const CREATED_SECRET_MESSAGE = "Webhook created. Copy the signing secret into your app.";
const EMPTY_DRAFT: WebhookDraft = { name: "", url: "", filterMode: "all", filter: [], paused: false };

export function WebhookEditorModal({ webhook, serverOffline, opener, onChanged, onClose }: WebhookEditorModalProps) {
  const [saved, setSaved] = useState<WebhookView | undefined>(webhook);
  const [draft, setDraft] = useState<WebhookDraft>(() => (webhook ? draftFromWebhook(webhook) : EMPTY_DRAFT));
  const [submitted, setSubmitted] = useState(false);
  const [serverErrors, setServerErrors] = useState<WebhookDraftErrors>({});
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState("");
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<StatusView | null>(null);
  /** True from a successful create until the next edit or save. */
  const [created, setCreated] = useState(false);
  const [discardArmed, setDiscardArmed] = useState(false);
  const secret = useWebhookSecret(saved?.id ?? null);
  const sources = useRef(new Map<string, RecipientSource>());
  const nameInput = useRef<HTMLInputElement>(null);
  const urlInput = useRef<HTMLInputElement>(null);
  const filterInput = useRef<HTMLInputElement>(null);
  const fieldInputs: Record<WebhookField, RefObject<HTMLInputElement | null>> = {
    name: nameInput,
    url: urlInput,
    filter: filterInput,
  };

  const signature = draftSignature(draft);
  const dirty = !saved || draftSignature(draftFromWebhook(saved)) !== signature;
  /** A webhook created in this modal; closing it with unsaved edits needs a second confirmation. */
  const createdHere = webhook === undefined && saved !== undefined;
  const problems = webhookDraftErrors(draft);
  const errors: WebhookDraftErrors = { ...(submitted ? problems : {}), ...serverErrors };
  const canTest = saved !== undefined && !dirty && !testing && !saving && !serverOffline;
  const testNote = !saved ? "Save to test" : dirty ? "Save changes to test" : null;

  useEffect(() => {
    setTestResult(null);
    setCreated(false);
    setDiscardArmed(false);
    setServerErrors({});
  }, [signature]);

  const patchDraft = (patch: Partial<WebhookDraft>) => setDraft((current) => ({ ...current, ...patch }));

  const save = async () => {
    if (saving || serverOffline) return;
    setSubmitted(true);
    setSaveError("");
    setServerErrors({});
    const firstProblem = FIELD_ORDER.find((field) => problems[field]);
    if (firstProblem) {
      fieldInputs[firstProblem].current?.focus();
      return;
    }
    setSaving(true);
    setCreated(false);
    setDiscardArmed(false);
    try {
      const input = toWebhookInput(draft);
      if (saved) {
        onChanged(await bridge.webhook_update(saved.id, input));
        onClose();
        return;
      }
      const result = await bridge.webhook_create(input);
      setSaved(result.webhook);
      setCreated(true);
      secret.adopt(result.secret, CREATED_SECRET_MESSAGE);
      onChanged(result.webhook);
    } catch (reason) {
      const code = errorCode(reason);
      const field = webhookFieldForCode(code);
      if (field) {
        setServerErrors({ [field]: webhookErrorMessage(code) });
        fieldInputs[field].current?.focus();
      } else {
        setSaveError(webhookErrorMessage(code));
      }
    } finally {
      setSaving(false);
    }
  };

  const runTest = async () => {
    if (!saved || !canTest) return;
    setTesting(true);
    setTestResult(null);
    try {
      setTestResult(webhookTestResultView(await bridge.webhook_test(saved.id)));
    } catch (reason) {
      setTestResult({ tone: "error", text: webhookErrorMessage(errorCode(reason)) });
    } finally {
      setTesting(false);
    }
  };

  const requestClose = () => {
    if (createdHere && dirty && !discardArmed) {
      setDiscardArmed(true);
      return;
    }
    onClose();
  };

  const testStatus = testing
    ? "Sending test…"
    : testResult && (
      <>
        <span className="webhook-dot" data-tone={testResult.tone} aria-hidden="true" />
        {testResult.text}
      </>
    );

  return (
    <Modal
      id="webhook-editor"
      title={created ? "Webhook created" : saved ? "Edit webhook" : "Add webhook"}
      opener={opener}
      busy={saving}
      onDismiss={requestClose}
      actions={(
        <div id="webhook-editor-footer" className="webhook-footer">
          <button
            id="webhook-test"
            type="button"
            className="secondary-button webhook-compact-button"
            disabled={!canTest}
            title={saved ? undefined : "Save first to send a test"}
            aria-describedby={testNote ? "webhook-test-note" : undefined}
            onClick={() => void runTest()}
          >
            <Send size={13} aria-hidden="true" />
            Send test
          </button>
          {testNote && <span id="webhook-test-note" className="webhook-test-note">{testNote}</span>}
          <span id="webhook-test-result" className="webhook-test-result" data-tone={testResult?.tone} role="status">
            {testStatus}
          </span>
          <span className="webhook-footer-spacer" />
          {discardArmed && <span id="webhook-discard-note" className="webhook-test-note" role="status">{DISCARD_NOTE}</span>}
          {createdHere ? (
            <button id="webhook-editor-close" type="button" className="secondary-button" onClick={requestClose}>
              Close
            </button>
          ) : (
            <button id="webhook-editor-cancel" type="button" className="secondary-button" disabled={saving} onClick={onClose}>
              Cancel
            </button>
          )}
          <button
            id="webhook-editor-save"
            type="submit"
            form={FORM_ID}
            className="primary-button"
            aria-busy={saving || undefined}
            disabled={saving || serverOffline}
          >
            {saved ? "Save" : "Create"}
          </button>
        </div>
      )}
    >
      <form id={FORM_ID} className="webhook-editor-body" noValidate onSubmit={(event) => { event.preventDefault(); void save(); }}>
        {serverOffline && <p id="webhook-editor-offline" className="settings-note" role="status">Server is offline. Changes can't be saved right now.</p>}

        <div className="webhook-field">
          <label id="webhook-name-label" htmlFor="webhook-name" className="webhook-field-label">Name (optional)</label>
          <input
            id="webhook-name"
            ref={nameInput}
            className="webhook-input"
            aria-labelledby="webhook-name-label"
            value={draft.name}
            placeholder={webhookNamePlaceholder(draft.url)}
            aria-invalid={errors.name ? true : undefined}
            aria-describedby={errors.name ? "webhook-name-error" : undefined}
            onChange={(event) => patchDraft({ name: event.target.value })}
          />
          {errors.name && <p id="webhook-name-error" className="settings-error" role="alert">{errors.name}</p>}
        </div>

        <div className="webhook-field">
          <label id="webhook-url-label" htmlFor="webhook-url" className="webhook-field-label">Endpoint URL</label>
          <input
            id="webhook-url"
            ref={urlInput}
            className="webhook-input webhook-input-mono"
            aria-labelledby="webhook-url-label"
            inputMode="url"
            value={draft.url}
            placeholder="https://example.com/webhooks"
            aria-invalid={errors.url ? true : undefined}
            aria-describedby={errors.url ? "webhook-url-error" : undefined}
            onChange={(event) => patchDraft({ url: event.target.value })}
          />
          {errors.url && <p id="webhook-url-error" className="settings-error" role="alert">{errors.url}</p>}
        </div>

        <div className="webhook-field">
          <span id="webhook-filter-mode-label" className="webhook-field-label">Send when a message is received from</span>
          <WebhookFilterModeControl
            labelledBy="webhook-filter-mode-label"
            value={draft.filterMode}
            onChange={(filterMode) => patchDraft({ filterMode })}
          />
        </div>

        {draft.filterMode !== "all" && (
          <div className="webhook-field">
            <RecipientChipInput
              id="webhook-filter-input"
              inputRef={filterInput}
              chipsId="webhook-filter-chips"
              hintId="webhook-filter-hint"
              errorId="webhook-filter-error"
              listLabel="Filtered contacts"
              recipients={filterRecipientChips(draft.filter)}
              onCommit={(ids) => patchDraft({ filter: filterEntriesFromChipIds(ids, draft.filter, sources.current) })}
              searchContacts={(query) => searchRecipientSuggestions(query, undefined, sources.current)}
              inputLabel="Contacts or numbers"
              placeholder="Add contact or number"
              emptyPlaceholder="Add contact or phone number"
              hint={HINT}
              hintVisible
              variant="standalone"
              invalid={Boolean(errors.filter)}
              describedBy={errors.filter ? "webhook-filter-required" : undefined}
            />
            {errors.filter && <p id="webhook-filter-required" className="settings-error" role="alert">{errors.filter}</p>}
          </div>
        )}

        <WebhookSecretField
          hasWebhook={saved !== undefined}
          secret={secret.value}
          revealed={secret.revealed}
          busy={secret.busy}
          status={secret.status}
          error={secret.error}
          rotateOpen={secret.rotateOpen}
          focusKey={secret.adoptedRevision}
          onToggleReveal={secret.toggleReveal}
          onCopy={secret.copy}
          onStartRotate={secret.startRotate}
          onCancelRotate={secret.cancelRotate}
          onConfirmRotate={secret.confirmRotate}
        />

        {saveError && <p id="webhook-save-error" className="settings-error" role="alert">{saveError}</p>}
      </form>
    </Modal>
  );
}
