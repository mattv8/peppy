import { useEffect, useRef, useState, type FormEvent, type RefObject } from "react";
import { Modal } from "@peppy/desktop-ui";
import { bridge, type ManagedDeviceView } from "../bridge/bridge";
import { errorCode } from "../bridge/bridgeErrors";

const nameError = (name: string) => {
  if (!name) return "Enter a name.";
  if (new TextEncoder().encode(name).length > 128) return "Use 128 bytes or fewer. Emoji and non-Latin characters count for more than one.";
  if (/\p{Cc}/u.test(name)) return "Names can't contain line breaks or control characters.";
  return "";
};

type Props = {
  device: ManagedDeviceView;
  displayName: string;
  offline: boolean;
  opener: RefObject<HTMLButtonElement | null>;
  focusFallback: RefObject<HTMLHeadingElement | null>;
  onClose(): void;
  onOffline(): void;
  onFinished(message?: string): Promise<void>;
};

export function DeviceRenameDialog({ device, displayName, offline, opener, focusFallback, onClose, onOffline, onFinished }: Props) {
  const [draft, setDraft] = useState(device.name);
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const pendingRef = useRef(false);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => { input.current?.focus(); input.current?.select(); }, []);
  useEffect(() => { if (error && !pending) input.current?.focus(); }, [error, pending]);

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (pendingRef.current || offline) return;
    const name = draft.trim();
    const validation = nameError(name);
    if (validation) { setError(validation); input.current?.focus(); return; }
    if (name === device.name.trim()) return;
    pendingRef.current = true;
    setPending(true);
    setError("");
    try {
      await bridge.rename_device(device.id, name);
      onClose();
      await onFinished();
    } catch (reason) {
      const code = errorCode(reason);
      if (code === "device_already_revoked" || code === "device_not_found") {
        onClose();
        await onFinished(code === "device_already_revoked" ? "This device was removed, so it can't be renamed." : "This device is no longer in the roster.");
      } else {
        if (code === "offline") onOffline();
        setError(code === "offline" ? "Server offline. Renaming is unavailable."
          : code === "invalid_device_name" ? "Enter a valid name."
          : code === "invalid_bearer" ? "This device's sign-in expired. Reconnect it, then try again."
          : code === "app-locked" || code === "app_lock" ? "Unlock Peppy to rename a device."
          : code === "operation_not_permitted" ? "Renaming isn't available for this account right now."
          : "Couldn't rename the device. Try again.");
        input.current?.focus();
      }
    } finally {
      pendingRef.current = false;
      setPending(false);
    }
  };

  return <Modal id="device-rename-dialog" title={`Rename ${displayName}`} busy={pending} opener={opener} focusFallback={focusFallback} onDismiss={() => { if (!pendingRef.current) onClose(); }} actions={<>
    <button id="device-rename-cancel" type="button" className="secondary-button" disabled={pending} onClick={onClose}>Cancel</button>
    <button id="device-rename-save" type="submit" form="device-rename-form" className="primary-button" disabled={pending || offline || draft.trim() === device.name.trim()} aria-busy={pending || undefined}>Save name</button>
  </>}>
    <form id="device-rename-form" noValidate onSubmit={event => void submit(event)}>
      <label id="device-rename-label" htmlFor="device-rename-input">Device name</label>
      <input id="device-rename-input" ref={input} autoComplete="off" value={draft} disabled={pending} aria-invalid={error ? true : undefined} aria-describedby={error ? "device-rename-help device-rename-error" : "device-rename-help"} onChange={event => { setDraft(event.target.value); setError(""); }} />
      <p id="device-rename-help" className="settings-note">This name appears wherever Peppy shows this device, such as notifications, contacts and the send-from picker.</p>
      {error && <p id="device-rename-error" className="settings-error" role="alert">{error}</p>}
    </form>
  </Modal>;
}
