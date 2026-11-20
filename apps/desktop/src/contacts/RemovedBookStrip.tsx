import { useRef, useState, type RefObject } from "react";
import { Modal } from "@peppy/desktop-ui";
import { bridge, type ContactBookView } from "../bridge/bridge";
import { IsolatedText } from "../notices/IsolatedText";
import type { PostContactActionNotice } from "./contact-action-notice";
import "./book-rail.css";

/** The removed-book strip. The removal dialog itself is owned by ContactsView, so it survives the strip leaving the status slot. */
export function RemovedBookStrip({ book, onRemoveRequest }: {
  book: ContactBookView;
  onRemoveRequest(book: ContactBookView, opener: RefObject<HTMLElement | null>): void;
}) {
  const opener = useRef<HTMLButtonElement>(null);
  return (
    <section id="contacts-book-removed" role="status">
      <div className="contacts-strip-text">
        <strong>This phone was removed.</strong>
        <p>Peppy keeps a read-only copy of its contacts. You can browse them, but you can't change them or ask the phone to sync.</p>
      </div>
      <div className="contacts-strip-actions">
        <button ref={opener} type="button" className="secondary-button" data-action="remove-book-local" onClick={() => onRemoveRequest(book, opener)}>Remove from this computer</button>
      </div>
    </section>
  );
}

export function RemoveBookDialog({ book, opener, onDismiss, onRemoved, onNotice }: {
  book: ContactBookView;
  opener: React.RefObject<HTMLElement | null>;
  onDismiss(): void;
  onRemoved(bookId: string): void;
  onNotice: PostContactActionNotice;
}) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const inFlight = useRef(false);
  const remove = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setFailed(false);
    try {
      await bridge.forget_contact_book(book.id);
      onDismiss();
      onRemoved(book.id);
      onNotice({ kind: "success", message: <>Removed <IsolatedText text={book.deviceName} /> from this computer.</> });
    } catch {
      setFailed(true);
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return <Modal id="contacts-book-remove-dialog" title={<>Remove <IsolatedText text={book.deviceName} /> from this computer?</>} opener={opener} busy={busy} onDismiss={onDismiss} actions={<>
    <button type="button" className="secondary-button" disabled={busy} onClick={onDismiss}>Cancel</button>
    <button type="button" className="destructive-button" disabled={busy} onClick={() => void remove()}>{busy ? "Removing…" : "Remove from this computer"}</button>
  </>}>
    <p><IsolatedText text={book.deviceName} /> and its {book.contactCount} contacts stop showing on this computer, and names from this book stop being used here. Other devices in your vault keep theirs. Contacts on the phone aren't touched.</p>
    <p>You can't bring the book back on this computer later.</p>
    {failed && <p role="alert">Couldn't remove this book. Try again.</p>}
  </Modal>;
}
