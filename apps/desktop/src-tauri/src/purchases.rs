//! Native App Store purchasing. Signed StoreKit payloads never leave this module except in the
//! authenticated request that reconciles them with Peppy Hosted.

use crate::{
    distribution::Distribution,
    error::{BridgeError, BridgeResult},
    hosted::{self, HostedState},
    net::{read_bounded, MAX_JSON_BYTES},
    storekit, AppState,
};
use peppy_hosted_api::{
    billing_apple::{
        routes, AppleAccountTokenResponse, AppleTransactionRequest, AppleTransactionResponse,
        AppleTransactionResult, BillingCatalogProduct, BillingCatalogResponse, BillingProductKind,
    },
    billing_web::{BillingManageableIn, BillingProvider},
};
use serde::Serialize;
use std::{collections::HashSet, time::Duration};
use tauri::Manager;
use tauri_plugin_opener::OpenerExt;

const SUBSCRIPTIONS_URL: &str = "https://apps.apple.com/account/subscriptions";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PurchaseOptions {
    app_store: bool,
    website: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoreProduct {
    id: String,
    kind: String,
    display_name: String,
    display_price: String,
    period: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_bytes: Option<i64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BillingSummary {
    provider: BillingProvider,
    manageable_in: BillingManageableIn,
    plan_active: bool,
    storage_packs: i64,
    storage_renews_without_plan: bool,
    deletion_pending: bool,
}

fn invalid_response() -> BridgeError {
    BridgeError::new(
        "server-response",
        "Peppy Hosted returned an invalid response.",
    )
}

fn no_billing_summary() -> BillingSummary {
    BillingSummary {
        provider: BillingProvider::None,
        manageable_in: BillingManageableIn::None,
        plan_active: false,
        storage_packs: 0,
        storage_renews_without_plan: false,
        deletion_pending: false,
    }
}

fn summary_from_catalog(catalog: &BillingCatalogResponse) -> BillingSummary {
    let summary = &catalog.summary;
    BillingSummary {
        provider: summary.provider,
        manageable_in: summary.manageable_in,
        plan_active: summary.plan_active,
        storage_packs: summary.storage_packs,
        storage_renews_without_plan: summary.storage_renews_without_plan,
        deletion_pending: summary.deletion_pending,
    }
}

fn unsupported() -> BridgeError {
    BridgeError::new(
        "unsupported",
        "App Store purchases are unavailable on this platform.",
    )
}

fn purchase_options_for(distribution: Distribution, storefront: Option<&str>) -> PurchaseOptions {
    match distribution {
        Distribution::Direct => PurchaseOptions {
            app_store: false,
            website: true,
        },
        Distribution::AppStore if storefront == Some("USA") => PurchaseOptions {
            app_store: true,
            website: true,
        },
        Distribution::AppStore => PurchaseOptions {
            app_store: true,
            website: false,
        },
    }
}

async fn current_options() -> PurchaseOptions {
    if Distribution::detect() != Distribution::AppStore {
        return purchase_options_for(Distribution::Direct, None);
    }
    let storefront = storekit::storefront_country().await.ok().flatten();
    purchase_options_for(Distribution::AppStore, storefront.as_deref())
}

fn http(origin: &str) -> BridgeResult<reqwest::Client> {
    crate::tls::http_builder(origin)?
        .redirect(reqwest::redirect::Policy::none())
        .https_only(!crate::origin::is_loopback_http(origin))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .user_agent("Peppy-Desktop")
        .build()
        .map_err(|_| BridgeError::new("network", "Could not create the native network client."))
}

async fn response_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> BridgeResult<T> {
    let body = read_bounded(response, MAX_JSON_BYTES)
        .await
        .map_err(|_| invalid_response())?;
    serde_json::from_slice(&body).map_err(|_| invalid_response())
}

async fn catalog(state: &AppState, hosted: &HostedState) -> BridgeResult<BillingCatalogResponse> {
    let origin = hosted::hosted_origin()?;
    let bearer = hosted::enrollment_account_session(state, hosted, &origin).await?;
    let response = http(&origin)?
        .get(format!("{origin}{}", routes::CATALOG))
        .bearer_auth(&*bearer)
        .send()
        .await
        .map_err(|_| BridgeError::new("offline", "Could not reach Peppy Hosted."))?;
    if !response.status().is_success() {
        return Err(BridgeError::new(
            "billing-unavailable",
            "Peppy Hosted could not load billing information.",
        ));
    }
    response_json(response).await
}

async fn account_token(
    state: &AppState,
    hosted: &HostedState,
) -> BridgeResult<AppleAccountTokenResponse> {
    let origin = hosted::hosted_origin()?;
    let bearer = hosted::enrollment_account_session(state, hosted, &origin).await?;
    let response = http(&origin)?
        .get(format!("{origin}{}", routes::APPLE_ACCOUNT_TOKEN))
        .bearer_auth(&*bearer)
        .send()
        .await
        .map_err(|_| BridgeError::new("offline", "Could not reach Peppy Hosted."))?;
    if !response.status().is_success() {
        return Err(BridgeError::new(
            "billing-unavailable",
            "Peppy Hosted could not prepare this purchase.",
        ));
    }
    response_json(response).await
}

async fn post_transaction(
    state: &AppState,
    hosted: &HostedState,
    signed_transaction: &str,
) -> BridgeResult<AppleTransactionResult> {
    let origin = hosted::hosted_origin()?;
    let bearer = hosted::enrollment_account_session(state, hosted, &origin).await?;
    post_transaction_at(&origin, &bearer, signed_transaction).await
}

async fn post_transaction_at(
    origin: &str,
    bearer: &str,
    signed_transaction: &str,
) -> BridgeResult<AppleTransactionResult> {
    let request = AppleTransactionRequest {
        signed_transaction: signed_transaction.to_owned(),
    };
    let response = http(origin)?
        .post(format!("{origin}{}", routes::APPLE_TRANSACTIONS))
        .bearer_auth(bearer)
        .json(&request)
        .send()
        .await
        .map_err(|_| BridgeError::new("offline", "Could not reach Peppy Hosted."))?;
    match response.status() {
        reqwest::StatusCode::OK => response_json::<AppleTransactionResponse>(response)
            .await
            .map(|body| body.result),
        reqwest::StatusCode::ACCEPTED => Ok(AppleTransactionResult::Pending),
        reqwest::StatusCode::CONFLICT => Ok(AppleTransactionResult::Conflict),
        _ => Err(BridgeError::new(
            "store-unavailable",
            "Peppy Hosted could not apply this purchase.",
        )),
    }
}

async fn reconcile_transaction(
    state: &AppState,
    hosted: &HostedState,
    signed_transaction: &str,
    transaction_id: Option<&str>,
) -> BridgeResult<AppleTransactionResult> {
    let result = post_transaction(state, hosted, signed_transaction).await?;
    reconcile_posted_transaction(result, transaction_id, |transaction_id| async move {
        storekit::finish(&transaction_id)
            .await
            .map_err(storekit_error)
    })
    .await
}

async fn reconcile_posted_transaction<F, Fut>(
    result: AppleTransactionResult,
    transaction_id: Option<&str>,
    finish: F,
) -> BridgeResult<AppleTransactionResult>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = BridgeResult<bool>>,
{
    if result == AppleTransactionResult::Granted {
        finish(transaction_id.ok_or_else(invalid_response)?.to_owned()).await?;
    }
    Ok(result)
}

fn storekit_error(error: storekit::StoreKitError) -> BridgeError {
    match error {
        storekit::StoreKitError::Unsupported => unsupported(),
        _ => BridgeError::new(
            "store-unavailable",
            "The App Store is unavailable right now.",
        ),
    }
}

fn catalog_product(
    product: &BillingCatalogProduct,
    store: storekit::Product,
    storage_pack_bytes: i64,
) -> StoreProduct {
    StoreProduct {
        id: store.id,
        kind: match product.kind {
            BillingProductKind::Plan => "plan",
            BillingProductKind::Storage => "storage",
        }
        .into(),
        display_name: store.display_name,
        display_price: store.display_price,
        period: product
            .interval
            .as_deref()
            .and_then(normalized_period)
            .or_else(|| store.period.as_deref().and_then(normalized_period))
            .map(str::to_owned),
        storage_bytes: (product.kind == BillingProductKind::Storage)
            .then(|| product.storage_pack_count.unwrap_or_default() * storage_pack_bytes),
    }
}

fn normalized_period(interval: &str) -> Option<&'static str> {
    match interval {
        "month" | "1 month" => Some("month"),
        "year" | "1 year" => Some("year"),
        _ => None,
    }
}

fn purchased_payload(purchase: &storekit::PurchaseResult) -> BridgeResult<(&str, &str)> {
    Ok((
        purchase
            .signed_transaction
            .as_deref()
            .ok_or_else(invalid_response)?,
        purchase
            .transaction_id
            .as_deref()
            .ok_or_else(invalid_response)?,
    ))
}

/// Starts launch reconciliation and the long-lived StoreKit observer for an App Store install.
pub(crate) fn start(app: tauri::AppHandle) {
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    #[cfg(target_os = "macos")]
    {
        let app_for_updates = app.clone();
        tauri::async_runtime::spawn(async move {
            if Distribution::detect() != Distribution::AppStore {
                return;
            }
            let state = app_for_updates.state::<AppState>();
            let hosted = app_for_updates.state::<HostedState>();
            let _ = reconcile_unfinished(&state, &hosted).await;
            let Ok(mut updates) = storekit::observe_transaction_updates() else {
                return;
            };
            while let Some(update) = updates.recv().await {
                if let Ok(transaction) = update {
                    let _ = reconcile_transaction(
                        &state,
                        &hosted,
                        &transaction.signed_transaction,
                        Some(&transaction.transaction_id),
                    )
                    .await;
                }
            }
        });
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
async fn reconcile_unfinished(state: &AppState, hosted: &HostedState) -> BridgeResult<()> {
    let transactions = storekit::unfinished().await.map_err(storekit_error)?;
    let mut failures = 0;
    for transaction in transactions {
        if reconcile_transaction(
            state,
            hosted,
            &transaction.signed_transaction,
            Some(&transaction.transaction_id),
        )
        .await
        .is_err()
        {
            failures += 1;
        }
    }
    if failures != 0 {
        eprintln!("peppy: {failures} unfinished App Store transactions could not be reconciled");
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn purchase_options(
    window: tauri::WebviewWindow,
) -> BridgeResult<PurchaseOptions> {
    crate::require_main(window.label())?;
    Ok(current_options().await)
}

#[tauri::command]
pub(crate) async fn store_products(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
) -> BridgeResult<Vec<StoreProduct>> {
    crate::require_main(window.label())?;
    if Distribution::detect() != Distribution::AppStore {
        return Err(unsupported());
    }
    let catalog = catalog(&state, &hosted).await.map_err(|error| {
        if error.code == "hosted-session-required" {
            error
        } else {
            BridgeError::new(
                "store-unavailable",
                "Purchases are unavailable right now. Try again.",
            )
        }
    })?;
    let storage_pack_bytes = catalog.storage_pack_bytes;
    let products: Vec<_> = catalog
        .products
        .iter()
        .filter(|product| product.provider == BillingProvider::Apple)
        .collect();
    let ids = products
        .iter()
        .map(|product| product.id.clone())
        .collect::<Vec<_>>();
    let store_products = storekit::products(&ids).await.map_err(|_| {
        BridgeError::new(
            "store-unavailable",
            "Purchases are unavailable right now. Try again.",
        )
    })?;
    let result = store_products
        .into_iter()
        .filter_map(|store| {
            products
                .iter()
                .find(|product| product.id == store.id)
                .map(|product| catalog_product(product, store, storage_pack_bytes))
        })
        .collect::<Vec<_>>();
    if result.is_empty() {
        return Err(BridgeError::new(
            "store-unavailable",
            "Purchases are unavailable right now. Try again.",
        ));
    }
    Ok(result)
}

#[tauri::command]
pub(crate) async fn store_purchase(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
    product_id: String,
) -> BridgeResult<String> {
    crate::require_main(window.label())?;
    if Distribution::detect() != Distribution::AppStore {
        return Err(unsupported());
    }
    let token = account_token(&state, &hosted).await?;
    let purchase = storekit::purchase(&product_id, &token.token.to_string())
        .await
        .map_err(storekit_error)?;
    match purchase.result.as_str() {
        "cancelled" => Ok("cancelled".into()),
        "pending" => Ok("pending".into()),
        "purchased" => {
            let (signed_transaction, transaction_id) = purchased_payload(&purchase)?;
            match reconcile_transaction(&state, &hosted, signed_transaction, Some(transaction_id))
                .await?
            {
                AppleTransactionResult::Granted => Ok("purchased".into()),
                AppleTransactionResult::Pending => Ok("pending".into()),
                AppleTransactionResult::Conflict => Err(BridgeError::new(
                    "store-conflict",
                    "This purchase conflicts with the billing provider for your Peppy account.",
                )),
                AppleTransactionResult::Rejected => Err(BridgeError::new(
                    "store-rejected",
                    "This purchase could not be applied to your Peppy account.",
                )),
            }
        }
        _ => Err(BridgeError::new(
            "store-unavailable",
            "The App Store could not verify this purchase.",
        )),
    }
}

#[tauri::command]
pub(crate) async fn store_restore(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    if Distribution::detect() != Distribution::AppStore {
        return Err(unsupported());
    }
    storekit::sync().await.map_err(storekit_error)?;
    let mut transactions = HashSet::new();
    transactions.extend(storekit::unfinished().await.map_err(storekit_error)?);
    transactions.extend(
        storekit::current_entitlements()
            .await
            .map_err(storekit_error)?,
    );
    let mut failures = 0;
    for transaction in transactions {
        if reconcile_transaction(
            &state,
            &hosted,
            &transaction.signed_transaction,
            Some(&transaction.transaction_id),
        )
        .await
        .is_err()
        {
            failures += 1;
        }
    }
    if failures != 0 {
        eprintln!("peppy: {failures} restored App Store transactions could not be reconciled");
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn open_store_subscriptions(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    app.opener()
        .open_url(SUBSCRIPTIONS_URL, None::<String>)
        .map_err(|_| BridgeError::new("open-url", "Could not open App Store subscriptions."))
}

#[tauri::command]
pub(crate) async fn billing_summary(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
) -> BridgeResult<BillingSummary> {
    crate::require_main(window.label())?;
    Ok(catalog(&state, &hosted)
        .await
        .map(|catalog| summary_from_catalog(&catalog))
        .unwrap_or_else(|_| no_billing_summary()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::StatusCode, routing::post, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    async fn transaction_server(status: StatusCode) -> String {
        async fn handler(State(status): State<StatusCode>) -> (StatusCode, &'static str) {
            (status, r#"{"result":"granted"}"#)
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route(routes::APPLE_TRANSACTIONS, post(handler))
                    .with_state(status),
            )
            .await
            .unwrap();
        });
        format!("http://{address}")
    }

    #[test]
    fn purchase_options_matrix_is_the_rust_fixture_for_ui_consumers() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../src/bridge/purchase-options.fixture.json"
        ))
        .unwrap();
        for (key, distribution, storefront) in [
            ("direct", Distribution::Direct, None),
            ("appStoreUnitedStates", Distribution::AppStore, Some("USA")),
            (
                "appStoreOtherStorefront",
                Distribution::AppStore,
                Some("CAN"),
            ),
            ("appStoreUnknownStorefront", Distribution::AppStore, None),
        ] {
            assert_eq!(
                serde_json::to_value(purchase_options_for(distribution, storefront)).unwrap(),
                fixture[key],
                "{key}"
            );
        }
    }

    #[tokio::test]
    async fn reconciliation_finishes_only_granted_http_transactions() {
        for (status, expected_finishes) in [
            (StatusCode::OK, 1),
            (StatusCode::ACCEPTED, 0),
            (StatusCode::CONFLICT, 0),
            (StatusCode::INTERNAL_SERVER_ERROR, 0),
        ] {
            let origin = transaction_server(status).await;
            let finishes = Arc::new(AtomicUsize::new(0));
            let result = post_transaction_at(&origin, "test-token", "signed").await;
            if let Ok(result) = result {
                let finishes = finishes.clone();
                reconcile_posted_transaction(result, Some("42"), move |_| {
                    let finishes = finishes.clone();
                    async move {
                        finishes.fetch_add(1, Ordering::SeqCst);
                        Ok(true)
                    }
                })
                .await
                .unwrap();
            }
            assert_eq!(
                finishes.load(Ordering::SeqCst),
                expected_finishes,
                "{status}"
            );
        }
    }

    #[tokio::test]
    async fn restore_reposts_each_deduplicated_transaction() {
        let origin = transaction_server(StatusCode::ACCEPTED).await;
        let transactions = HashSet::from([
            storekit::Transaction {
                transaction_id: "one".into(),
                signed_transaction: "signed-one".into(),
            },
            storekit::Transaction {
                transaction_id: "two".into(),
                signed_transaction: "signed-two".into(),
            },
        ]);

        let mut reposted = 0;
        for transaction in transactions {
            let result =
                post_transaction_at(&origin, "test-token", &transaction.signed_transaction)
                    .await
                    .unwrap();
            reconcile_posted_transaction(result, Some(&transaction.transaction_id), |_| async {
                panic!("pending restore transactions must not finish")
            })
            .await
            .unwrap();
            reposted += 1;
        }
        assert_eq!(reposted, 2);
    }

    #[test]
    fn catalog_summary_defaults_fail_closed() {
        let catalog: BillingCatalogResponse =
            serde_json::from_value(serde_json::json!({"products": [], "eligibility": "eligible"}))
                .unwrap();

        assert_eq!(
            serde_json::to_value(summary_from_catalog(&catalog)).unwrap(),
            serde_json::json!({
                "provider": "none",
                "manageableIn": "none",
                "planActive": false,
                "storagePacks": 0,
                "storageRenewsWithoutPlan": false,
                "deletionPending": false,
            })
        );
    }

    #[test]
    fn storage_products_use_catalog_interval_and_pack_bytes() {
        let product = BillingCatalogProduct {
            id: "org.peppy.desktop.storage.packs.3".into(),
            kind: BillingProductKind::Storage,
            provider: BillingProvider::Apple,
            interval: Some("year".into()),
            storage_pack_count: Some(3),
        };
        let store = storekit::Product {
            id: product.id.clone(),
            display_name: "Storage".into(),
            display_price: "$3".into(),
            period: Some("1 year".into()),
            kind: "storage".into(),
        };

        let result = catalog_product(&product, store, 1_024);
        assert_eq!(result.period.as_deref(), Some("year"));
        assert_eq!(result.storage_bytes, Some(3_072));

        let product = BillingCatalogProduct {
            interval: None,
            ..product
        };
        let store = storekit::Product {
            id: product.id.clone(),
            display_name: "Storage".into(),
            display_price: "$3".into(),
            period: Some("1 month".into()),
            kind: "storage".into(),
        };
        assert_eq!(
            catalog_product(&product, store, 1_024).period.as_deref(),
            Some("month")
        );
    }

    #[test]
    fn purchased_result_without_transaction_id_is_rejected_before_posting() {
        let purchase = storekit::PurchaseResult {
            result: "purchased".into(),
            signed_transaction: Some("signed".into()),
            transaction_id: None,
        };

        assert_eq!(
            purchased_payload(&purchase).unwrap_err().code,
            "server-response"
        );
    }
}
