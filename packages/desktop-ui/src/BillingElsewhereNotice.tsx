import { createElement, useState, type ReactElement } from "react";
import type { BillingProvider } from "./bridge-contracts";

export type BillingElsewhereCopy = {
  billing_elsewhere_title: string;
  billing_elsewhere_website_body: string;
  billing_elsewhere_website_cta: string;
  billing_elsewhere_other_body: string;
  billing_elsewhere_apple_body: string;
  billing_elsewhere_google_play_body: string;
  attachment_storage_billing_handoff: string;
  attachment_storage_billing_open_failed: string;
  store_manage_cta: string;
  store_manage_failed: string;
  google_play_manage_cta: string;
  google_play_manage_failed: string;
};

export type BillingElsewhereVariant = "website" | "other" | "apple" | "google_play";

export function conflictVariant(billingProvider: BillingProvider | undefined, website: boolean): BillingElsewhereVariant {
  if (billingProvider === "google_play") return "google_play";
  if (billingProvider === "apple") return "apple";
  return billingProvider === "stripe" && website ? "website" : "other";
}

export type BillingElsewhereNoticeProps = {
  variant: BillingElsewhereVariant;
  provider?: "stripe" | "apple" | "google_play";
  copy: BillingElsewhereCopy;
  onManageWebsite?(): Promise<void> | void;
  onManageAppStore?(): Promise<void> | void;
  onManageGooglePlay?(): Promise<void> | void;
  headingLevel?: 2 | 3 | 4 | 5 | 6;
};

function bodyForVariant(variant: BillingElsewhereVariant, copy: BillingElsewhereCopy): string {
  if (variant === "website") return copy.billing_elsewhere_website_body;
  if (variant === "apple") return copy.billing_elsewhere_apple_body;
  if (variant === "google_play") return copy.billing_elsewhere_google_play_body;
  return copy.billing_elsewhere_other_body;
}

export function BillingElsewhereNotice({ variant, provider, copy, onManageWebsite, onManageAppStore, onManageGooglePlay, headingLevel = 3 }: BillingElsewhereNoticeProps): ReactElement {
  const [manageError, setManageError] = useState(false);
  const manage = variant === "website" ? onManageWebsite : variant === "apple" ? onManageAppStore : variant === "google_play" ? onManageGooglePlay : undefined;
  const manageCopy = variant === "website" ? copy.billing_elsewhere_website_cta : variant === "google_play" ? copy.google_play_manage_cta : copy.store_manage_cta;
  const errorCopy = variant === "website" ? copy.attachment_storage_billing_open_failed : variant === "google_play" ? copy.google_play_manage_failed : copy.store_manage_failed;

  const openManage = async () => {
    try {
      setManageError(false);
      await manage?.();
    } catch {
      setManageError(true);
    }
  };

  return <div id="billing-elsewhere-notice" role="group" aria-labelledby="billing-elsewhere-title" data-variant={variant} data-billing-provider={provider}>
    {createElement(`h${headingLevel}`, { id: "billing-elsewhere-title" }, copy.billing_elsewhere_title)}
    <p id="billing-elsewhere-body">{bodyForVariant(variant, copy)}</p>
    {manage && <button id="billing-elsewhere-manage" className="secondary-button" type="button" onClick={() => { void openManage(); }}>{manageCopy}</button>}
    {variant === "website" && <p id="billing-elsewhere-handoff">{copy.attachment_storage_billing_handoff}</p>}
    <p id="billing-elsewhere-error" className="settings-error" role="alert">{manageError ? errorCopy : ""}</p>
  </div>;
}
