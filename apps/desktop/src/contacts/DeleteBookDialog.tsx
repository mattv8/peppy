import { useRef, useState, type RefObject } from "react";
import { Modal } from "@peppy/desktop-ui";
import { bridge, type ContactBookView } from "../bridge/bridge";
import { errorCode } from "../bridge/bridgeErrors";
import { IsolatedText } from "../notices/IsolatedText";

function deleteFailureText(code: string | undefined): string {
  switch (code) {
    case "contact-book-deleted": return "This book was already deleted for all devices.";
    case "contact-book-not-deletable": return "This contact book can't be deleted for all devices.";
    case "locked": return "Unlock sync to delete this book for all devices.";
    default: return "Couldn't delete this book for all devices. Try again.";
  }
}

/** Confirms deleting a removed phone's synced contacts for every vault device. ContactsView owns it, like the removal dialog. */
export function DeleteBookDialog({ book, opener, onDismiss, onDeleted, onBooksChanged }: {
  book: ContactBookView;
  opener: RefObject<HTMLElement | null>;
  onDismiss(): void;
  onDeleted(book: ContactBookView): void;
  onBooksChanged(): void;
}) {
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const inFlight = useRef(false);
  const blocked = !acknowledged || busy;
  const remove = async () => {
    if (blocked || inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setFailure("");
    try {
      await bridge.delete_contact_book(book.id);
      onDeleted(book);
    } catch (error) {
      const code = errorCode(error);
      setFailure(deleteFailureText(code));
      if (code === "contact-book-deleted") onBooksChanged();
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return <Modal id="contacts-book-delete-dialog" title={<>Delete <IsolatedText text={book.deviceName} />'s contacts for all devices?</>} opener={opener} busy={busy} onDismiss={onDismiss} actions={<>
    <button type="button" className="secondary-button" disabled={busy} onClick={onDismiss}>Cancel</button>
    <button
      type="button"
      className="destructive-button"
      aria-disabled={blocked || undefined}
      aria-busy={busy || undefined}
      onClick={() => void remove()}
    >
      {busy ? "Deleting…" : "Delete for all devices"}
    </button>
  </>}>
    <ul>
      <li>Deletes the saved copy of {book.contactCount} contacts from every device in your vault.</li>
      <li>Devices that are offline remove it when they next connect.</li>
      <li>Names from this book stop showing in conversations on all your devices.</li>
      <li>This can't be undone. Contacts on the phone itself aren't touched.</li>
    </ul>
    <label className="contacts-book-delete-ack">
      <input id="contacts-book-delete-ack" type="checkbox" checked={acknowledged} disabled={busy} onChange={event => setAcknowledged(event.target.checked)} />
      I understand this can't be undone.
    </label>
    {failure && <p id="contacts-book-delete-error" role="alert">{failure}</p>}
  </Modal>;
}
