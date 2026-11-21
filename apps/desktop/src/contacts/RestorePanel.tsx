import { useEffect, useRef, type ReactNode } from "react";
import { ContactAvatar, UPDATE_PAUSED_DESCRIPTION, contactSeed } from "@peppy/desktop-ui";
import type { RestorableContact } from "../bridge/bridge";

export function RestorePanel({
  list,
  canRestore,
  writesPaused = false,
  notice,
  error,
  onClose,
  onRestore,
}: {
  list: RestorableContact[];
  onClose(): void;
  canRestore: boolean;
  writesPaused?: boolean;
  notice: ReactNode;
  error: ReactNode;
  onRestore(item: RestorableContact): void;
}) {
  const closeRef = useRef<HTMLButtonElement>(null);
  useEffect(() => { closeRef.current?.focus(); }, []);

  return (
    <section
      id="contacts-restore-panel"
      role="dialog"
      aria-modal="true"
      aria-label="Recently deleted contacts"
    >
      <header>
        <button
          ref={closeRef}
          data-action="close-restore"
          aria-label="Close recently deleted"
          onClick={onClose}
        >
          ←
        </button>
        <h2>Recently deleted</h2>
      </header>
      {notice}
      {error}
      {list.length ? (
        <ul id="contacts-restore-list">
          {list.map(item => (
            <li key={item.id} data-contact-id={item.id}>
              <ContactAvatar
                seed={contactSeed(undefined, item.id)}
                name={item.displayName}
                photoDataUrl={item.photoDataUrl}
              />
              <span>
                {item.displayName}
                <small>Deleted {item.deletedAt}</small>
              </span>
              {canRestore && (
                <button
                  data-action="restore-contact"
                  aria-label={`Restore ${item.displayName}`}
                  disabled={writesPaused}
                  title={writesPaused ? UPDATE_PAUSED_DESCRIPTION : undefined}
                  aria-describedby={writesPaused ? "update-paused-description" : undefined}
                  onClick={() => onRestore(item)}
                >
                  Restore
                </button>
              )}
            </li>
          ))}
        </ul>
      ) : (
        <p id="contacts-restore-empty">No contacts deleted in the past 90 days.</p>
      )}
      {list.length > 0 && !canRestore && (
        <p id="contacts-restore-owner-only">This book doesn't accept changes from this computer, so deleted contacts can't be restored from here.</p>
      )}
    </section>
  );
}
