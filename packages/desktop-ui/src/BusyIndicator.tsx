import { LoaderCircle } from "lucide-react";
import type { ReactElement } from "react";

/** Visible pending status: the label is the live-region text; the spinner is decorative. */
export function BusyIndicator({ id, label }: { id: string; label: string }): ReactElement {
  return <span id={id} className="busy-indicator" role="status" aria-live="polite" aria-atomic="true">
    <LoaderCircle className="busy-indicator-icon" size={14} strokeWidth={1.75} aria-hidden />
    <span className="busy-indicator-label">{label}</span>
  </span>;
}
