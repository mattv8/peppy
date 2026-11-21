import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { Clock, ContactAvatar, Pencil, TriangleAlert, X, contactSeed, formatPhoneNumber } from "@peppy/desktop-ui";
import { bridge, type ContactBookView, type ContactEditOutcome, type ContactView } from "../bridge/bridge";
import { contactErrorText, editStateLabel } from "./contact-labels";
import { formatContactName, type ContactNameFormat } from "./contact-name-format";
import { useContactPhotoChange } from "./useContactPhotoChange";
import { COMPACT_DETAIL_QUERY, useMediaMatch } from "./useMediaMatch";
import "./contact-detail.css";

type PhotoChange = { kind: "set"; croppedDataUrl: string } | { kind: "remove" };
type PendingEditState = ContactView["pendingEditState"];

const PHOTO_PENDING_REASON = "Waiting for the phone to apply your last change.";
const PHOTO_LOCKED_REASON = "Unlock sync to change photos.";

/** Edit the phone has not finished: the photo button is disabled while one is open. */
const isPendingEdit = (state: PendingEditState) => state === "pending" || state === "awaiting-approval" || state === "outcome-unknown";
/** Edit the phone refused or dropped: the stored photo is shown with a retry. */
const isFailedEdit = (state: PendingEditState) => state === "conflict" || state === "rejected" || state === "failed" || state === "expired";

function photoOutcomeReason(outcome: ContactEditOutcome): string | undefined {
  if (outcome.state === "rejected") return outcome.reason;
  if (outcome.state === "conflict") return outcome.conflictSummary;
  if (outcome.state === "expired") return "The request expired before it was sent.";
  return undefined;
}

export type ContactDetailProps = {
  contact: ContactView;
  /** The contact's own book; the photo button needs it to know whether photos can be changed. */
  book?: ContactBookView;
  canWrite: boolean;
  confirm: boolean;
  nameFormat: ContactNameFormat;
  /** Sync is locked (encryption or app lock, or sync needs unlock): the photo button shows why it is disabled. */
  photoLocked?: boolean;
  onEdit(): void;
  onDelete(): void;
  onCancelConfirm(): void;
  onClose(): void;
  /** Called after the photo edit was accepted by the phone's queue; the caller reloads the list. */
  onPhotoSubmitted?(): void;
  /** A save or delete is in flight: Close waits like Back does. */
  busy?: boolean;
};

export function ContactDetail({
  contact,
  book,
  canWrite,
  confirm,
  nameFormat,
  photoLocked = false,
  onEdit,
  onDelete,
  onCancelConfirm,
  onClose,
  onPhotoSubmitted,
  busy = false,
}: ContactDetailProps) {
  const compactDetail = useMediaMatch(COMPACT_DETAIL_QUERY);
  const photoButtonRef = useRef<HTMLButtonElement>(null);
  const photoWrapRef = useRef<HTMLDivElement>(null);
  const photoMenuRef = useRef<HTMLDivElement>(null);
  const previousPendingState = useRef(contact.pendingEditState);
  const [menuOpen, setMenuOpen] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [photoError, setPhotoError] = useState("");
  // Set when this panel's photo change was accepted for this contact. `preview` is the crop shown while it is pending.
  const [photoSent, setPhotoSent] = useState<{ preview?: string } | null>(null);
  const deviceName = book?.deviceName ?? "the phone";
  const name = formatContactName(contact, nameFormat);

  const sendPhoto = async (photo: PhotoChange) => {
    if (submitting) return;
    setSubmitting(true);
    setPhotoError("");
    try {
      const outcome = await bridge.submit_contact_edit({
        targetBookId: contact.bookId,
        kind: "update",
        contactId: contact.id,
        baseRevision: contact.revision,
        patches: [],
        photo,
      });
      const reason = photoOutcomeReason(outcome);
      if (reason) {
        setPhotoError(`Couldn't send the photo to ${deviceName}. ${reason}`);
        return;
      }
      setPhotoSent({ preview: photo.kind === "set" ? photo.croppedDataUrl : undefined });
      onPhotoSubmitted?.();
    } catch (error) {
      setPhotoError(`Couldn't send the photo to ${deviceName}. ${contactErrorText(error)}`);
    } finally {
      setSubmitting(false);
    }
  };

  const { choosePhoto, picking, error: pickError, cropDialog } = useContactPhotoChange({
    onCropped: croppedDataUrl => void sendPhoto({ kind: "set", croppedDataUrl }),
  });

  const photoEditable = Boolean(book && canWrite && book.capabilities.supportsPhoto && !book.sourceRevoked && book.state !== "retired");
  const photoPending = isPendingEdit(contact.pendingEditState);
  const photoFailed = isFailedEdit(contact.pendingEditState) && photoSent !== null;
  const photoDisabledReason = photoPending ? PHOTO_PENDING_REASON : photoLocked ? PHOTO_LOCKED_REASON : undefined;
  const photoBusy = submitting || picking;
  const photoChangePending = photoPending && photoSent !== null;
  const photoState = photoFailed ? "error" : photoChangePending ? "pending" : "idle";
  const hasPhoto = Boolean(contact.photoDataUrl);
  const shownPhoto = photoChangePending ? photoSent.preview : contact.photoDataUrl;
  const photoAvatar = (
    <ContactAvatar
      seed={contactSeed(contact.phones, contact.id)}
      name={contact.displayName}
      photoDataUrl={shownPhoto}
      size={64}
    />
  );
  const badgeIcon = photoFailed
    ? <TriangleAlert size={12} aria-hidden />
    : photoChangePending
      ? <Clock size={12} aria-hidden />
      : <Pencil size={12} aria-hidden />;

  useEffect(() => {
    const previous = previousPendingState.current;
    previousPendingState.current = contact.pendingEditState;
    // The edit was applied or cleared: this session's photo record no longer applies.
    if (previous && !contact.pendingEditState) setPhotoSent(null);
  }, [contact.pendingEditState]);

  useEffect(() => {
    if (!menuOpen) return;
    photoMenuRef.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus();
    const closeOnOutsideClick = (event: MouseEvent) => {
      if (!photoWrapRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    document.addEventListener("mousedown", closeOnOutsideClick);
    return () => document.removeEventListener("mousedown", closeOnOutsideClick);
  }, [menuOpen]);

  const closeMenu = (restoreFocus = false) => {
    setMenuOpen(false);
    if (restoreFocus) photoButtonRef.current?.focus();
  };

  const startPick = () => {
    if (photoBusy) return;
    setPhotoError("");
    void choosePhoto(photoButtonRef.current);
  };

  const onPhotoButtonClick = () => {
    if (photoDisabledReason || photoBusy) return;
    if (hasPhoto) {
      setMenuOpen(open => !open);
      return;
    }
    startPick();
  };

  const onPhotoMenuKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'));
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    if (event.key === "Escape") {
      event.preventDefault();
      closeMenu(true);
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      items[(current + step + items.length) % items.length]?.focus();
    } else if (event.key === "Tab") {
      closeMenu();
    }
  };

  const stableKeys = <T extends { id?: string }>(items: T[], fallback: (item: T) => string) => {
    const occurrences = new Map<string, number>();
    return items.map(item => {
      if (item.id) return item.id;
      const base = fallback(item);
      const occurrence = occurrences.get(base) ?? 0;
      occurrences.set(base, occurrence + 1);
      return `${base}:duplicate-${occurrence}`;
    });
  };
  const phoneKeys = stableKeys(contact.phones, phone => `${phone.label}-${phone.number}`);
  const emailKeys = stableKeys(contact.emails, email => email.address);
  const addressKeys = stableKeys(contact.addresses, address => `${address.label ?? ""}-${address.street ?? ""}-${address.city ?? ""}`);
  return (
    <>
      <header id="contacts-detail-header">
        {photoEditable ? (
          <div className="contacts-detail-photo" ref={photoWrapRef}>
            <button
              id="contacts-detail-photo"
              ref={photoButtonRef}
              type="button"
              data-photo-state={photoState}
              aria-label={`Change photo for ${name}`}
              title={photoDisabledReason ?? "Change photo"}
              aria-describedby={photoDisabledReason ? "contacts-detail-photo-reason" : undefined}
              aria-disabled={photoDisabledReason || photoBusy ? "true" : undefined}
              aria-busy={photoBusy || undefined}
              aria-haspopup={hasPhoto ? "menu" : undefined}
              aria-expanded={hasPhoto ? menuOpen : undefined}
              onClick={onPhotoButtonClick}
            >
              {photoAvatar}
              <span className="contacts-detail-photo-badge" aria-hidden="true">{badgeIcon}</span>
            </button>
            {photoDisabledReason && <span id="contacts-detail-photo-reason" className="sr-only">{photoDisabledReason}</span>}
            {hasPhoto && menuOpen && (
              <div id="contacts-detail-photo-menu" role="menu" aria-label="Photo options" ref={photoMenuRef} onKeyDown={onPhotoMenuKeyDown}>
                <button type="button" role="menuitem" data-action="change-photo" onClick={() => { closeMenu(); startPick(); }}>
                  Choose photo…
                </button>
                <button type="button" role="menuitem" data-action="remove-photo" onClick={() => { closeMenu(true); void sendPhoto({ kind: "remove" }); }}>
                  Remove photo
                </button>
              </div>
            )}
          </div>
        ) : photoAvatar}
        <div id="contacts-detail-identity">
          <div id="contacts-detail-title">
            <h2 title={name}>{name}</h2>
            <small>{[contact.organization, contact.title].filter(Boolean).join(", ")}</small>
          </div>
          {canWrite && (
            <div id="contacts-detail-actions">
              <button
                data-action="edit-contact"
                aria-label="Edit contact"
                onClick={onEdit}
              >
                Edit
              </button>
              <button
                data-action="delete-contact"
                data-confirm-delete={confirm ? "pending" : undefined}
                aria-label={confirm ? "Confirm delete" : "Delete contact"}
                onKeyDown={e => { if (e.key === "Escape" && confirm) { e.preventDefault(); onCancelConfirm(); } }}
                onBlur={confirm ? onCancelConfirm : undefined}
                onClick={onDelete}
              >
                {confirm ? "Confirm delete" : "Delete"}
              </button>
            </div>
          )}
        </div>
        {!compactDetail && (
          <button
            id="contacts-detail-close"
            type="button"
            data-action="close-detail"
            aria-label="Close contact"
            title="Close"
            disabled={busy}
            onClick={onClose}
          >
            <X size={16} aria-hidden />
          </button>
        )}
      </header>
      {(photoError || pickError) && <p id="contacts-detail-photo-error" role="alert">{photoError || pickError}</p>}
      {contact.pendingEditState && (
        <section data-contact-banner={contact.pendingEditState} role="status">
          {photoFailed
            ? "Photo change wasn't applied on the phone."
            : `${contact.pendingEditSummary ?? (photoSent ? "Photo change" : "Change")} — ${editStateLabel(contact.pendingEditState)}`}
          {photoFailed && (
            <button type="button" data-action="retry-photo" onClick={startPick}>Try again</button>
          )}
        </section>
      )}
      {contact.phones.length > 0 && (
        <section data-contact-section="phones">
          <h3>Phone numbers</h3>
          {contact.phones.map((phone, position) => (
            <p key={phoneKeys[position]}>
              <span className="contacts-detail-label">{phone.displayLabel ?? phone.label}</span>{" "}
              <span>{formatPhoneNumber(phone.number)}</span>
            </p>
          ))}
        </section>
      )}
      {contact.emails.length > 0 && (
        <section data-contact-section="emails">
          <h3>Email</h3>
          {contact.emails.map((email, position) => (
            <p key={emailKeys[position]}>
              <span className="contacts-detail-label">{email.displayLabel ?? email.label}</span>{" "}
              <span>{email.address}</span>
            </p>
          ))}
        </section>
      )}
      {contact.addresses.length > 0 && (
        <section data-contact-section="addresses">
          <h3>Addresses</h3>
          {contact.addresses.map((addr, position) => (
            <p key={addressKeys[position]}>
              <span className="contacts-detail-label">{addr.displayLabel ?? addr.label}</span>{" "}
              <span>{[addr.street, addr.city, addr.state, addr.postalCode, addr.country]
                .filter(Boolean)
                .join(", ")}</span>
            </p>
          ))}
        </section>
      )}
      {contact.organization && (
        <section data-contact-section="organization">
          <h3>Organization</h3>
          <p>{contact.organization}</p>
        </section>
      )}
      {contact.title && (
        <section data-contact-section="title">
          <h3>Title</h3>
          <p>{contact.title}</p>
        </section>
      )}
      {contact.birthday && (
        <section data-contact-section="birthday">
          <h3>Birthday</h3>
          <p>
            {contact.birthday.month}/{contact.birthday.day}
            {contact.birthday.year && `/${contact.birthday.year}`}
          </p>
        </section>
      )}
      {contact.notes && (
        <section data-contact-section="notes">
          <h3>Notes</h3>
          <p>{contact.notes}</p>
        </section>
      )}
      {cropDialog}
    </>
  );
}
