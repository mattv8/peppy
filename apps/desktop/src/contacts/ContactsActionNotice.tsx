import type { JSX } from "react";
import { X } from "@peppy/desktop-ui";
import { useNoticeDismissal } from "../notices/useNoticeDismissal";
import type { ContactActionNotice } from "./contact-action-notice";

type Props = {
  notice?: ContactActionNotice;
  onDismiss(): void;
};

/**
 * Timed result toast for Contacts actions. Success fades after 4 s unless hovered or focused; errors stay until
 * retried or dismissed. The element is always mounted, so an empty toast stays hidden through `:empty`.
 */
export function ContactsActionNotice({ notice, onDismiss }: Props): JSX.Element {
  const { toastRef, setHovered, setFocused, trackBlur } = useNoticeDismissal(notice, onDismiss);
  const isError = notice?.kind === "error";
  // Clear first so a retry that reports its own outcome synchronously is not wiped out.
  const retry = () => {
    const retryNotice = notice?.retry;
    onDismiss();
    retryNotice?.();
  };
  return <div
    ref={toastRef}
    id="contacts-action-notice"
    className="mark-all-read-toast"
    data-placement="list"
    role={isError ? "alert" : "status"}
    aria-live={isError ? undefined : "polite"}
    onMouseEnter={() => setHovered(true)}
    onMouseLeave={() => setHovered(false)}
    onFocus={() => setFocused(true)}
    onBlur={trackBlur}
  >
    {notice && <>
      {notice.message}
      {notice.retry && <button className="secondary-button" type="button" data-action="retry-book-use" onClick={retry}>Try again</button>}
      <button className="mark-all-read-toast-dismiss" type="button" aria-label="Dismiss notification" data-action="dismiss-notice" onClick={onDismiss}>
        <X size={14} strokeWidth={1.75} aria-hidden />
      </button>
    </>}
  </div>;
}
