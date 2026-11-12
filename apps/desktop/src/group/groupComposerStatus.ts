import type { InlineStatusItem } from "@peppy/desktop-ui";
import { peppyCopy } from "../generated/peppyCopy";
import { fillCopy, summarizeGroupNames } from "./groupCopy";

export type GroupComposerStatusInput = {
  creating: boolean;
  notice: null
    | { kind: "members-changed"; memberRevision: string; members?: readonly string[] }
    | { kind: "create-unconfirmed" }
    | { kind: "create-not-rcs" }
    | { kind: "update-required" }
    | { kind: "withdrawn" };
  groupLeft: boolean;
  displayName(address: string): string;
  canResend: boolean;
  checking: boolean;
  onResend(): void;
  onReview(): void;
  onCreateAgain(): void;
  onStartNewMms(): void;
};

/** Lower numbers appear first in the shared composer status slot. */
export const GROUP_STATUS_PRIORITY: Record<string, 1 | 2 | 3 | 4> = {
  "group-members-changed": 1,
  "group-create-unconfirmed": 1,
  "group-left": 2,
  "group-withdrawn": 2,
  "group-update-required": 3,
  "group-creating": 4,
  "group-create-not-rcs": 4,
};

export function groupComposerStatusItems(input: GroupComposerStatusInput): InlineStatusItem[] {
  const items: InlineStatusItem[] = [];
  const { notice } = input;

  if (notice?.kind === "members-changed") {
    const detailItems = notice.members?.length
      ? [fillCopy(peppyCopy.group_send_members_now, { names: summarizeGroupNames(notice.members.map(input.displayName)) })]
      : undefined;
    items.push({
      key: "group-members-changed",
      id: "composer-group-changed",
      tone: "error",
      role: "alert",
      message: peppyCopy.composer_status_group_changed,
      detail: peppyCopy.composer_status_group_changed_detail,
      ...(detailItems ? { detailItems } : {}),
      hooks: {
        "data-group-send-refusal": "members-changed",
        "data-member-revision": notice.memberRevision,
      },
      actions: [
        ...(input.canResend ? [{
          id: "group-send-updated",
          label: input.checking ? peppyCopy.composer_status_group_checking : peppyCopy.composer_status_group_resend,
          ariaLabel: peppyCopy.composer_status_group_resend_label,
          disabled: input.checking,
          onAction: input.onResend,
        }] : []),
        {
          id: "group-send-review",
          label: peppyCopy.composer_status_group_review,
          ariaLabel: peppyCopy.group_send_review,
          onAction: input.onReview,
        },
      ],
    });
  }

  if (notice?.kind === "create-unconfirmed") {
    items.push({
      key: "group-create-unconfirmed",
      id: "composer-group-create-unconfirmed",
      tone: "warning",
      role: "alert",
      message: peppyCopy.composer_status_create_unconfirmed,
      detail: peppyCopy.group_create_unconfirmed,
      hooks: { "data-group-create": "unconfirmed" },
      actions: [{
        id: "group-create-again",
        label: peppyCopy.composer_status_create_again,
        onAction: input.onCreateAgain,
        confirm: {
          title: peppyCopy.composer_status_create_again_title,
          body: peppyCopy.composer_status_create_again_body,
          confirmLabel: peppyCopy.composer_status_create_again,
          cancelLabel: peppyCopy.composer_status_cancel,
        },
      }],
    });
  }

  if (input.groupLeft || notice?.kind === "withdrawn") {
    const groupLeft = input.groupLeft;
    items.push({
      key: groupLeft ? "group-left" : "group-withdrawn",
      id: "composer-group-withdrawn",
      tone: "warning",
      role: "status",
      message: groupLeft ? peppyCopy.composer_status_group_left : peppyCopy.group_withdrawn_notice,
      detail: groupLeft ? peppyCopy.composer_status_group_left_detail : peppyCopy.composer_status_group_withdrawn_detail,
      hooks: { "data-group-send-refusal": "withdrawn" },
      actions: [{
        id: "group-start-new-mms",
        label: peppyCopy.composer_status_start_group_mms,
        ariaLabel: peppyCopy.group_withdrawn_action,
        onAction: input.onStartNewMms,
      }],
    });
  }

  if (notice?.kind === "update-required") {
    items.push({
      key: "group-update-required",
      id: "composer-group-update-required",
      tone: "warning",
      role: "status",
      message: peppyCopy.group_update_required,
      hooks: { "data-group-send-refusal": "update-required" },
    });
  }

  if (input.creating) {
    items.push({
      key: "group-creating",
      id: "composer-group-create-status",
      tone: "info",
      role: "status",
      message: peppyCopy.composer_status_group_creating,
    });
  }

  if (notice?.kind === "create-not-rcs") {
    items.push({
      key: "group-create-not-rcs",
      id: "composer-group-create-not-rcs",
      tone: "info",
      role: "status",
      message: peppyCopy.group_create_not_rcs_info,
      hooks: { "data-group-create": "not-rcs" },
    });
  }

  return items.sort((left, right) => GROUP_STATUS_PRIORITY[left.key] - GROUP_STATUS_PRIORITY[right.key]);
}
