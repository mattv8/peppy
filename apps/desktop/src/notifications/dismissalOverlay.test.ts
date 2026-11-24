import { describe, expect, it } from "vitest";
import { applyDismissResults, classifyDismissalError, newestDismissTargets, reconcileDismissalOverlay } from "./dismissalOverlay";
import { GENERIC_FAILURE_REASON } from "./notificationDismissCopy";
import { notificationKey } from "./notificationSections";

const target = { sourceDeviceId: "phone", notificationKey: "notice", lifetime: "one" };
const other = { ...target, lifetime: "two" };
const row = { target, packageName: "app", appName: "App", title: "Title", text: "", postedAt: 1, dismissible: true, seen: false, dismissalPending: false };
const committing = (overlayTarget: typeof target) => ({ [notificationKey(overlayTarget)]: { target: overlayTarget, phase: "committing" as const } });

describe("dismissal overlay", () => {
  it("keeps recorded entries hidden until the matching lifetime is pending or gone", () => {
    const recorded = applyDismissResults(committing(target), [target], [{ target, outcome: "recorded" }]);
    expect(recorded[notificationKey(target)].phase).toBe("recorded");
    expect(reconcileDismissalOverlay(recorded, [row])).toEqual(recorded);
    expect(reconcileDismissalOverlay(recorded, [{ ...row, dismissalPending: true }])).toEqual({});
    expect(reconcileDismissalOverlay(recorded, [{ ...row, target: other }])).toEqual({});
  });

  it("drops refused, gone, and muted results but records already-recorded results", () => {
    for (const outcome of ["notDismissible", "gone", "muted"] as const) {
      expect(applyDismissResults(committing(target), [target], [{ target, outcome }])).toEqual({});
    }
    expect(applyDismissResults(committing(target), [target], [{ target, outcome: "alreadyRecorded" }])[notificationKey(target)].phase).toBe("recorded");
  });

  it("marks a sent target that got no result as failed with the generic reason and a retry", () => {
    const failed = applyDismissResults(committing(target), [target], []);
    expect(failed[notificationKey(target)]).toEqual({ target, phase: "failed", reason: GENERIC_FAILURE_REASON, canRetry: true });
  });

  it("ignores results for targets that are not in flight", () => {
    expect(applyDismissResults({}, [target], [{ target, outcome: "recorded" }])).toEqual({});
  });

  it("drops a failed entry once its row is gone or pending, and keeps it while the row can be retried", () => {
    const failed = { [notificationKey(target)]: { target, phase: "failed" as const, reason: GENERIC_FAILURE_REASON, canRetry: true } };
    expect(reconcileDismissalOverlay(failed, [row])).toEqual(failed);
    expect(reconcileDismissalOverlay(failed, [])).toEqual({});
    expect(reconcileDismissalOverlay(failed, [{ ...row, dismissalPending: true }])).toEqual({});
  });

  it("keeps in-flight entries through reconciliation", () => {
    const inFlight = committing(target);
    expect(reconcileDismissalOverlay(inFlight, [])).toEqual(inFlight);
  });

  it("picks the newest targets, breaking ties by the order given, and caps them", () => {
    const rows = [
      { ...row, target: { ...target, notificationKey: "old" }, postedAt: 1 },
      { ...row, target: { ...target, notificationKey: "tie-first" }, postedAt: 5 },
      { ...row, target: { ...target, notificationKey: "newest" }, postedAt: 9 },
      { ...row, target: { ...target, notificationKey: "tie-second" }, postedAt: 5 },
    ];
    expect(newestDismissTargets(rows).map(item => item.notificationKey)).toEqual(["newest", "tie-first", "tie-second", "old"]);
    expect(newestDismissTargets(rows, 2).map(item => item.notificationKey)).toEqual(["newest", "tie-first"]);
    expect(newestDismissTargets([])).toEqual([]);
  });

  it("caps the default at 100 targets", () => {
    const rows = Array.from({ length: 101 }, (_, index) => ({ ...row, target: { ...target, notificationKey: `n${index}` }, postedAt: index }));
    const picked = newestDismissTargets(rows);
    expect(picked).toHaveLength(100);
    expect(picked[0].notificationKey).toBe("n100");
    expect(picked.some(item => item.notificationKey === "n0")).toBe(false);
  });

  it("classifies update busy as non-retryable and worker failures as manual retry only", () => {
    expect(classifyDismissalError({ code: "update-busy" })).toMatchObject({ autoRetry: false, canRetry: false });
    expect(classifyDismissalError({ code: "update-in-progress" })).toMatchObject({ autoRetry: true, canRetry: true });
    expect(classifyDismissalError(new Error("worker failed"))).toMatchObject({ autoRetry: false, canRetry: true });
    expect(classifyDismissalError({ code: "credentials-required" }).reason).toContain("locked");
  });
});
