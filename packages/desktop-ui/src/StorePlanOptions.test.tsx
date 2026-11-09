import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BillingElsewhereNotice, type BillingElsewhereCopy } from "./BillingElsewhereNotice";
import type { StoreProduct } from "./bridge-contracts";
import { StorePlanOptions, type StorePlanOptionsCopy } from "./StorePlanOptions";

afterEach(cleanup);

const copy = {
  hosted_subscribe_cta: "Subscribe", hosted_subscribe_body: "Choose a plan.", hosted_purchase_pending: "Pending", hosted_purchase_verifying: "Verifying", production_pairing_checking: "Checking purchases…", attachment_storage_allowance: "50 GB included", try_again: "Try again", privacy_policy: "Privacy Policy", store_period_month: "per month", store_period_year: "per year", store_renewal_notice: "Renews automatically.", store_terms_cta: "Terms of Use", store_restore_cta: "Restore purchases", store_restore_working: "Restoring purchases…", store_restore_done: "Restore finished.", store_restore_failed: "Purchases couldn't be restored.", store_purchase_waiting: "Complete the purchase.", store_purchase_pending_body: "Waiting for approval.", store_purchase_failed: "The purchase couldn't be completed.", store_purchase_rejected: "This purchase couldn't be applied.", store_unavailable: "Purchases are unavailable right now.", billing_elsewhere_title: "Subscription billed elsewhere", billing_elsewhere_website_body: "This subscription is billed on the Peppy website.", billing_elsewhere_website_cta: "Manage on the website", billing_elsewhere_other_body: "This subscription was started outside the App Store.", billing_elsewhere_apple_body: "This subscription is billed through the App Store.", attachment_storage_billing_handoff: "Manage it there.", attachment_storage_billing_open_failed: "Couldn't open billing.", store_manage_cta: "Manage App Store subscriptions", store_manage_failed: "Couldn't open subscriptions.",
} satisfies StorePlanOptionsCopy & BillingElsewhereCopy;

const products = [
  { id: "plan.month", kind: "plan" as const, displayName: "Monthly", displayPrice: "$5", period: "month" },
  { id: "plan.year", kind: "plan" as const, displayName: "Yearly", displayPrice: "$50", period: "year" },
] satisfies StoreProduct[];

describe("StorePlanOptions", () => {
  it("renders App Store products without website wording when website purchases are unavailable", () => {
    render(<StorePlanOptions purchaseOptions={{ appStore: true, website: false }} products={products} state="ready" copy={copy} onPurchase={vi.fn()} onRestore={vi.fn()} />);
    expect(screen.getAllByRole("button", { name: "Subscribe" })).toHaveLength(2);
    expect(document.body.textContent?.toLowerCase()).not.toContain("website");
    expect(screen.getByRole("button", { name: /Terms of Use/ })).toBeInTheDocument();
  });

  it("sends the selected product ID and shows pending, cancelled, and error outcomes", async () => {
    const onPurchase = vi.fn().mockResolvedValueOnce("pending").mockResolvedValueOnce("cancelled").mockRejectedValueOnce(new Error("failed"));
    const { unmount } = render(<StorePlanOptions purchaseOptions={{ appStore: true, website: true }} products={products} state="ready" copy={copy} onPurchase={onPurchase} onRestore={vi.fn()} />);
    fireEvent.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    expect(await screen.findByText("Pending Waiting for approval.")).toBeInTheDocument();
    expect(onPurchase).toHaveBeenCalledWith("plan.month");
    unmount();
    const cancelled = vi.fn().mockResolvedValue("cancelled");
    const { unmount: unmountCancelled } = render(<StorePlanOptions purchaseOptions={{ appStore: true, website: true }} products={products} state="ready" copy={copy} onPurchase={cancelled} onRestore={vi.fn()} />);
    fireEvent.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    await waitFor(() => expect(document.getElementById("hosted-purchase-status")).toBeEmptyDOMElement());
    unmountCancelled();
    const failing = vi.fn().mockRejectedValue(new Error("failed"));
    render(<StorePlanOptions purchaseOptions={{ appStore: true, website: true }} products={products} state="ready" copy={copy} onPurchase={failing} onRestore={vi.fn()} />);
    fireEvent.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    expect(await screen.findByText(copy.store_purchase_failed)).toBeInTheDocument();
  });

  it("shows the unavailable state and restore action", () => {
    const retry = vi.fn();
    render(<StorePlanOptions purchaseOptions={{ appStore: true, website: false }} products={[]} state="unavailable" copy={copy} onPurchase={vi.fn()} onRestore={vi.fn()} onRetry={retry} />);
    expect(screen.getByText(copy.store_unavailable)).toHaveAttribute("role", "alert");
    fireEvent.click(screen.getByRole("button", { name: copy.try_again }));
    expect(retry).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: copy.store_restore_cta })).toBeInTheDocument();
  });

  it("uses error codes for rejection, availability, and provider conflicts", async () => {
    const { rerender } = render(<StorePlanOptions purchaseOptions={{ appStore: true, website: false }} products={products} state="ready" copy={copy} onPurchase={vi.fn().mockRejectedValue({ code: "store-rejected" })} onRestore={vi.fn()} />);
    fireEvent.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    expect(await screen.findByText(copy.store_purchase_rejected)).toBeInTheDocument();
    rerender(<StorePlanOptions purchaseOptions={{ appStore: true, website: false }} products={products} state="ready" copy={copy} onPurchase={vi.fn().mockRejectedValue({ code: "store-conflict" })} onRestore={vi.fn()} />);
    fireEvent.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    expect(await screen.findByText(copy.billing_elsewhere_other_body)).toBeInTheDocument();
    expect(document.getElementById("hosted-purchase-status")).toBeEmptyDOMElement();
    expect(screen.getByRole("button", { name: copy.store_restore_cta })).toBeEnabled();
  });

  it.each([
    [{ website: true }, "stripe", copy.billing_elsewhere_website_body, "stripe"],
    [{ website: true }, "apple", copy.billing_elsewhere_apple_body, "apple"],
    [{ website: true }, undefined, copy.billing_elsewhere_other_body, null],
    [{ website: true }, "none", copy.billing_elsewhere_other_body, null],
  ] as const)("uses the billing provider %s for the conflict notice", async (options, billingProvider, body, dataProvider) => {
    render(<StorePlanOptions purchaseOptions={{ appStore: true, ...options }} products={products} state="ready" copy={copy} billingProvider={billingProvider} onPurchase={vi.fn().mockRejectedValue({ code: "store-conflict" })} onRestore={vi.fn()} />);
    fireEvent.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    expect(await screen.findByText(body)).toBeInTheDocument();
    const notice = document.getElementById("billing-elsewhere-notice");
    if (dataProvider === null) expect(notice).not.toHaveAttribute("data-billing-provider");
    else expect(notice).toHaveAttribute("data-billing-provider", dataProvider);
  });

  it("shows a loading indicator and not an empty plan list", () => {
    render(<StorePlanOptions purchaseOptions={{ appStore: true, website: false }} products={[]} state="loading" copy={copy} onPurchase={vi.fn()} onRestore={vi.fn()} />);
    expect(screen.getByText(copy.production_pairing_checking)).toBeInTheDocument();
    expect(document.getElementById("hosted-plan-options")).toBeNull();
    expect(document.body.textContent?.toLowerCase()).not.toContain("website");
  });

  it("notifies the host after a completed purchase", async () => {
    const onPurchased = vi.fn();
    render(<StorePlanOptions purchaseOptions={{ appStore: true, website: false }} products={products} state="ready" copy={copy} onPurchase={vi.fn().mockResolvedValue("purchased")} onRestore={vi.fn()} onPurchased={onPurchased} />);
    fireEvent.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    await waitFor(() => expect(onPurchased).toHaveBeenCalledOnce());
  });
});

describe("BillingElsewhereNotice", () => {
  it.each([
    ["website", "stripe", copy.billing_elsewhere_website_body],
    ["other", "stripe", copy.billing_elsewhere_other_body],
    ["apple", "apple", copy.billing_elsewhere_apple_body],
  ] as const)("renders the %s variant", (variant, provider, body) => {
    render(<BillingElsewhereNotice variant={variant} provider={provider} copy={copy} onManageWebsite={vi.fn()} onManageAppStore={vi.fn()} />);
    expect(screen.getByText(body)).toBeInTheDocument();
    if (variant === "other") expect(screen.queryByRole("button")).toBeNull();
    else expect(screen.getByRole("button")).toBeInTheDocument();
  });
});
