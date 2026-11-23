import type { ReactNode } from "react";

/** A transient Contacts result shown in the list's timed toast. Errors stay until dismissed or retried. */
export type ContactActionNotice = {
  kind: "success" | "error";
  message: ReactNode;
  /** Overrides the default success fade delay; errors do not fade. */
  holdMs?: number;
  retry?: () => void;
};

export type PostContactActionNotice = (notice: ContactActionNotice | undefined) => void;
