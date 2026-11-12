import { formatComposerReason, type InlineStatusItem } from "@peppy/desktop-ui";
import { peppyCopy } from "../generated/peppyCopy";
import { GROUP_STATUS_PRIORITY } from "../group/groupComposerStatus";

type RcsBlock = { kind: "none" | "unproven" } | { kind: "link"; help: "relink" | "phone" };

export type ComposerStatusInput = {
  pendingRecipientReason?: string;
  groupLeft: boolean;
  draftSendBlock?: string;
  draftBlockKind?: "attachment" | "offline" | "conflict" | "failed-attachment";
  routeProblem?: string;
  rcsBlock: RcsBlock;
  mainWindowRecovery?: boolean;
  groupItems: InlineStatusItem[];
  composerFeedback?: { key: string; message: string };
};

const SEND_BLOCKER_KEYS = new Set([
  "pending-recipient", "group-left", "group-withdrawn", "attachment-block", "draft-offline", "draft-conflict", "failed-attachment", "route-problem", "rcs-unproven", "rcs-relink", "rcs-phone",
]);

const GROUP_PRIORITY_ORDER = { 1: 10, 2: 21, 3: 60, 4: 80 } as const;

function rcsStatus(block: RcsBlock): InlineStatusItem | undefined {
  if (block.kind === "none") return;
  if (block.kind === "unproven") return { key: "rcs-unproven", tone: "warning", message: peppyCopy.rcs_composer_status_unproven, detail: peppyCopy.rcs_composer_detail_unproven, hooks: { "data-route-block": "unproven" } };
  if (block.kind !== "link") return;
  if (block.help === "relink") return { key: "rcs-relink", tone: "warning", message: peppyCopy.rcs_composer_status_relink, detail: peppyCopy.rcs_composer_detail_relink, hooks: { "data-route-block": "link-relink" } };
  return { key: "rcs-phone", tone: "warning", message: peppyCopy.rcs_composer_status_phone, detail: peppyCopy.rcs_composer_detail_phone, hooks: { "data-route-block": "link-phone" } };
}

function feedbackReason(feedback: ComposerStatusInput["composerFeedback"]): string | undefined {
  const match = feedback?.message.match(/^Not sent: (.*?)\.?$/i);
  return match?.[1].trim().toLocaleLowerCase().replace(/ in the main window$/, "");
}

function dedupe(item: InlineStatusItem, rawReason: string | undefined, feedback: ComposerStatusInput["composerFeedback"]): InlineStatusItem {
  const reason = feedbackReason(feedback);
  return reason && rawReason?.trim().toLocaleLowerCase().replace(/ in the main window$/, "") === reason
    ? { ...item, srOnly: true }
    : item;
}

export function composerStatusItems(input: ComposerStatusInput): InlineStatusItem[] {
  const items: Array<{ priority: number; item: InlineStatusItem; rawReason?: string }> = input.groupItems.map((item) => ({
    priority: GROUP_PRIORITY_ORDER[GROUP_STATUS_PRIORITY[item.key]],
    item,
  }));
  if (input.pendingRecipientReason) items.push({ priority: 20, rawReason: input.pendingRecipientReason, item: { key: "pending-recipient", tone: "warning", message: formatComposerReason(input.mainWindowRecovery ? `${input.pendingRecipientReason} in the main window` : input.pendingRecipientReason) } });
  if (input.draftSendBlock) {
    const key = input.draftBlockKind === "attachment" ? "attachment-block" : input.draftBlockKind === "offline" ? "draft-offline" : input.draftBlockKind === "conflict" ? "draft-conflict" : "failed-attachment";
    items.push({ priority: 30, rawReason: input.draftSendBlock, item: {
      key, tone: "warning", message: formatComposerReason(input.draftSendBlock),
      ...(key === "attachment-block" ? { detail: peppyCopy.composer_status_attachment_blocked_detail } : {}),
      ...(key === "draft-conflict" || key === "failed-attachment" ? { srOnly: true } : {}),
    } });
  }
  if (input.routeProblem) items.push({ priority: 40, rawReason: input.routeProblem, item: { key: "route-problem", tone: "warning", message: formatComposerReason(input.mainWindowRecovery ? `${input.routeProblem} in the main window` : input.routeProblem) } });
  const rcs = rcsStatus(input.rcsBlock);
  if (rcs) items.push({ priority: 50, item: rcs });
  return items
    .sort((left, right) => left.priority - right.priority)
    .map(({ item, rawReason }) => dedupe(item, rawReason, input.composerFeedback))
    .map((item, index, ordered) => {
      if (!SEND_BLOCKER_KEYS.has(item.key)) return item;
      const primaryBlocker = !ordered.slice(0, index).some((preceding) => SEND_BLOCKER_KEYS.has(preceding.key));
      return primaryBlocker ? { ...item, id: "unavailable-hint" } : item.id || !item.key.startsWith("rcs-") ? item : { ...item, id: "composer-route-notice" };
    });
}
