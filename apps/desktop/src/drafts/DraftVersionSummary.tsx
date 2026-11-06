import type { ReactNode } from "react";
import { formatPhoneNumber, type Attachment } from "@peppy/desktop-ui";

export type DraftVersionSummaryProps = {
  id: string;
  heading: ReactNode;
  text: string;
  recipientIds: string[];
  attachmentIds: string[];
  gatewayId?: string;
  simId?: string;
  attachments: Record<string, Attachment>;
};

export function DraftVersionSummary({
  id,
  heading,
  text,
  recipientIds,
  attachmentIds,
  gatewayId,
  simId,
  attachments,
}: DraftVersionSummaryProps) {
  const hasRoute = gatewayId !== undefined || simId !== undefined;

  return <article id={id} className="draft-review-version" data-draft-review-version>
    <div className="draft-review-version-heading">{heading}</div>
    <dl className="draft-review-summary">
      <div>
        <dt>Caption</dt>
        <dd className="draft-review-caption">{text || "No caption"}</dd>
      </div>
      <div>
        <dt>Images</dt>
        <dd>{attachmentIds.length > 0 ? <ul>{attachmentIds.map((attachmentId) => <li key={attachmentId}>{attachments[attachmentId]?.name ?? "Image details loading"}</li>)}</ul> : "No images"}</dd>
      </div>
      <div>
        <dt>Recipients</dt>
        <dd>{recipientIds.length > 0 ? <ul>{recipientIds.map((recipientId) => <li key={recipientId}>{formatPhoneNumber(recipientId)}</li>)}</ul> : "No recipients"}</dd>
      </div>
      <div>
        <dt>Sending phone and SIM</dt>
        <dd>{hasRoute ? <>
          <span>Selected phone and SIM</span>
          <details id={`${id}-route-details`}>
            <summary>Phone and SIM details</summary>
            <dl>
              <div><dt>Phone ID</dt><dd>{gatewayId ?? "Not specified"}</dd></div>
              <div><dt>SIM ID</dt><dd>{simId ?? "Not specified"}</dd></div>
            </dl>
          </details>
        </> : "Automatic selection"}</dd>
      </div>
    </dl>
  </article>;
}
