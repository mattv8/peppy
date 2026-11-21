import { useRef, useState } from "react";
import { Info, Modal } from "@peppy/desktop-ui";
import { formatCopy, peppyCopy } from "../generated/peppyCopy";
import { diagnosticsUsagePercent, formatDiagnosticsSize } from "./diagnosticsDisplay";
import { useDiagnostics, type DiagnosticsBridge, type DiagnosticsErrorKind } from "./useDiagnostics";

export { hasDiagnosticsBridge, type DiagnosticsBridge } from "./useDiagnostics";

const STORAGE_ERROR_TEXT = "Diagnostics storage is unavailable. Try again later.";
const ERROR_TEXT: Record<DiagnosticsErrorKind, string> = {
  load: STORAGE_ERROR_TEXT,
  storage: STORAGE_ERROR_TEXT,
  setting: peppyCopy.setting_update_error,
  export: "Could not export the diagnostic log. Try again.",
  clear: "Could not clear diagnostic logs. Try again.",
};

export function DiagnosticsSettings({ bridge }: { bridge: DiagnosticsBridge }) {
  const { state, busy, errorKind, statusText, recordingLabel, setRecording, exportLog, clearLogs, retryLoad } = useDiagnostics(bridge);
  const [clearOpen, setClearOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const titleRef = useRef<HTMLHeadingElement>(null);
  const helpButtonRef = useRef<HTMLButtonElement>(null);
  const clearButtonRef = useRef<HTMLButtonElement>(null);

  const loaded = state !== null;
  const actionsBusy = busy !== null;
  const hasRetained = (state?.retainedBytes ?? 0) > 0;

  const confirmClear = async () => {
    setClearOpen(false);
    const result = await clearLogs();
    if (result === "done") titleRef.current?.focus();
    if (result === "failed") clearButtonRef.current?.focus();
  };

  // One alert element is mounted at a time, inside the switch group for a setting error so aria-describedby resolves.
  const errorNode = errorKind ? <p id="diagnostics-error" className="settings-error" role="alert" data-diagnostics-error={errorKind}>{ERROR_TEXT[errorKind]}</p> : null;

  return <section id="settings-diagnostics" className="settings-card" data-settings-section="diagnostics" aria-labelledby="settings-diagnostics-name">
    <h2 id="settings-diagnostics-title" ref={titleRef} tabIndex={-1} aria-labelledby="settings-diagnostics-name">
      <span id="settings-diagnostics-name">Diagnostics</span>
      <button id="settings-diagnostics-help" ref={helpButtonRef} type="button" className="info-button" aria-label="About diagnostics" title="About diagnostics" aria-haspopup="dialog" onClick={() => setHelpOpen(true)}><Info size={16} aria-hidden /></button>
    </h2>
    {helpOpen && <Modal id="settings-diagnostics-help-dialog" title="About diagnostics" opener={helpButtonRef} onDismiss={() => setHelpOpen(false)} actions={<button type="button" className="secondary-button" onClick={() => setHelpOpen(false)}>Close</button>}>
      <p>Verbose logging adds a little overhead. Turn it off after testing.</p>
      <p>When the log is full, the oldest records are removed.</p>
      <p>Dropped events counts records Peppy could not save because the log was busy or unavailable.</p>
    </Modal>}
    {!loaded && errorKind === null && <p id="diagnostics-loading" className="settings-note" role="status">Loading diagnostics…</p>}
    <div id="diagnostics-enabled-group" data-setting="diagnostics-enabled">
      <div id="diagnostics-enabled-row" className="settings-control-row">
        <label htmlFor="diagnostics-enabled">Verbose logging</label>
        <span className="settings-switch">
          <input
            id="diagnostics-enabled"
            type="checkbox"
            role="switch"
            aria-checked={state?.enabled === true}
            aria-describedby={errorKind === "setting" ? "diagnostics-enabled-hint diagnostics-error" : "diagnostics-enabled-hint"}
            aria-busy={busy === "toggle" || undefined}
            aria-disabled={actionsBusy || undefined}
            disabled={!loaded}
            checked={state?.enabled === true}
            onChange={(event) => void setRecording(event.target.checked)}
          />
          <span aria-hidden />
        </span>
      </div>
      <p id="diagnostics-enabled-hint" className="settings-field-hint">Records typing and connection events on this computer for troubleshooting.</p>
      {errorKind === "setting" && errorNode}
    </div>
    {state && <dl id="diagnostics-status" data-diagnostics-status>
      <div id="diagnostics-recording-row">
        <dt>Recording</dt>
        <dd id="diagnostics-recording-value">{recordingLabel}</dd>
      </div>
      <div id="diagnostics-size-row">
        <dt>Retained log size</dt>
        <dd id="diagnostics-size-value">
          {formatCopy(peppyCopy.attachment_storage_used_of_limit, formatDiagnosticsSize(state.retainedBytes), formatDiagnosticsSize(state.limitBytes))}
          {state.limitBytes > 0 && <div id="diagnostics-size-bar" aria-hidden="true"><span style={{ width: `${diagnosticsUsagePercent(state.retainedBytes, state.limitBytes)}%` }} /></div>}
        </dd>
      </div>
      <div id="diagnostics-dropped-row">
        <dt>Dropped events</dt>
        <dd id="diagnostics-dropped-value">{state.droppedEvents}</dd>
      </div>
    </dl>}
    <p id="diagnostics-privacy" className="settings-field-hint">Logs stay on this computer until you choose Export log. They hold typing and connection counters, timing, and build information only, not message contents, contacts, identifiers, or credentials.</p>
    {errorKind !== "setting" && errorNode}
    <div id="diagnostics-actions">
      <button id="diagnostics-export" type="button" className="secondary-button" disabled={!loaded || !hasRetained} aria-disabled={actionsBusy || undefined} aria-busy={busy === "export" || undefined} onClick={() => void exportLog()}>Export log</button>
      <button id="diagnostics-clear" ref={clearButtonRef} type="button" className="secondary-button" aria-haspopup="dialog" disabled={!loaded || !hasRetained} aria-disabled={actionsBusy || undefined} aria-busy={busy === "clear" || undefined} onClick={() => { if (!actionsBusy) setClearOpen(true); }}>Clear logs</button>
      {errorKind === "load" && <button id="diagnostics-retry" type="button" className="secondary-button" onClick={retryLoad}>Try again</button>}
      <p id="diagnostics-status-message" role="status">{statusText}</p>
    </div>
    {clearOpen && <Modal id="diagnostics-clear-dialog" title="Clear diagnostic logs?" opener={clearButtonRef} onDismiss={() => setClearOpen(false)} actions={<>
      <button id="diagnostics-clear-cancel" type="button" className="secondary-button" onClick={() => setClearOpen(false)}>Cancel</button>
      <button id="diagnostics-clear-confirm" type="button" className="primary-button" onClick={() => void confirmClear()}>Clear logs</button>
    </>}>
      <p>This removes the logs kept on this computer. It cannot erase copies you already exported.</p>
    </Modal>}
  </section>;
}
