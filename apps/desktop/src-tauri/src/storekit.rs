//! StoreKit bridge used by the desktop purchase service. Callback JSON is copied before Swift returns.

use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

#[derive(Debug)]
pub enum StoreKitError {
    // Constructed only by the non-macOS stub.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unsupported,
    Bridge,
    InvalidData,
}

impl fmt::Display for StoreKitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "StoreKit is unavailable on this platform",
            Self::Bridge => "StoreKit bridge failed",
            Self::InvalidData => "StoreKit returned invalid data",
        })
    }
}

impl Error for StoreKitError {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub id: String,
    pub display_name: String,
    pub display_price: String,
    pub period: Option<String>,
    pub kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseResult {
    pub result: String,
    pub signed_transaction: Option<String>,
    pub transaction_id: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    pub transaction_id: String,
    pub signed_transaction: String,
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Product, PurchaseResult, StoreKitError, Transaction};
    use serde::{de::DeserializeOwned, Deserialize, Serialize};
    use std::ffi::{c_char, c_void, CStr, CString};
    use tokio::sync::{mpsc, oneshot};

    type Completion = unsafe extern "C" fn(*mut c_void, *const c_char);
    type ObserverCompletion = unsafe extern "C" fn(*mut c_void, *const c_char);

    unsafe extern "C" {
        fn storekit_products(request: *const c_char, context: *mut c_void, completion: Completion);
        fn storekit_purchase(request: *const c_char, context: *mut c_void, completion: Completion);
        fn storekit_finish(request: *const c_char, context: *mut c_void, completion: Completion);
        fn storekit_unfinished(context: *mut c_void, completion: Completion);
        fn storekit_current_entitlements(context: *mut c_void, completion: Completion);
        fn storekit_sync(context: *mut c_void, completion: Completion);
        fn storekit_storefront_country(context: *mut c_void, completion: Completion);
        fn storekit_observe_transaction_updates(
            context: *mut c_void,
            callback: ObserverCompletion,
        ) -> i64;
    }

    unsafe extern "C" fn complete(context: *mut c_void, json: *const c_char) {
        // Swift invokes each completion once and the callback copies its temporary UTF-8 buffer.
        let sender = unsafe {
            Box::from_raw(context.cast::<oneshot::Sender<Result<String, StoreKitError>>>())
        };
        let result = if json.is_null() {
            Err(StoreKitError::Bridge)
        } else {
            Ok(unsafe { CStr::from_ptr(json) }
                .to_string_lossy()
                .into_owned())
        };
        let _ = sender.send(result);
    }

    struct Observer<T> {
        receiver: mpsc::UnboundedReceiver<Result<T, StoreKitError>>,
    }

    impl<T> Observer<T> {
        async fn recv(&mut self) -> Option<Result<T, StoreKitError>> {
            self.receiver.recv().await
        }
    }

    unsafe extern "C" fn transaction_update(context: *mut c_void, json: *const c_char) {
        unsafe { send_update::<Transaction>(context, json) };
    }

    unsafe fn send_update<T: DeserializeOwned>(context: *mut c_void, json: *const c_char) {
        let sender = unsafe { &*context.cast::<mpsc::UnboundedSender<Result<T, StoreKitError>>>() };
        let value = if json.is_null() {
            Err(StoreKitError::Bridge)
        } else {
            serde_json::from_slice(unsafe { CStr::from_ptr(json) }.to_bytes())
                .map_err(|_| StoreKitError::InvalidData)
        };
        let _ = sender.send(value);
    }

    fn observer<T: DeserializeOwned>(
        start: impl FnOnce(*mut c_void, ObserverCompletion) -> i64,
        callback: ObserverCompletion,
    ) -> Result<Observer<T>, StoreKitError> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let context = Box::into_raw(Box::new(sender)).cast();
        let handle = start(context, callback);
        if handle == 0 {
            unsafe {
                drop(Box::from_raw(
                    context.cast::<mpsc::UnboundedSender<Result<T, StoreKitError>>>(),
                ))
            };
            return Err(StoreKitError::Unsupported);
        }
        Ok(Observer { receiver })
    }

    pub struct TransactionUpdates(Observer<Transaction>);

    impl TransactionUpdates {
        pub async fn recv(&mut self) -> Option<Result<Transaction, StoreKitError>> {
            self.0.recv().await
        }
    }

    async fn call<T: DeserializeOwned>(
        invoke: impl FnOnce(*mut c_void, Completion),
    ) -> Result<T, StoreKitError> {
        let (sender, receiver) = oneshot::channel::<Result<String, StoreKitError>>();
        let context = Box::into_raw(Box::new(sender)).cast();
        invoke(context, complete);
        let json = receiver.await.map_err(|_| StoreKitError::Bridge)??;
        serde_json::from_str(&json).map_err(|_| StoreKitError::InvalidData)
    }

    async fn call_json<T: DeserializeOwned>(
        value: impl Serialize,
        invoke: impl FnOnce(*const c_char, *mut c_void, Completion),
    ) -> Result<T, StoreKitError> {
        let json =
            CString::new(serde_json::to_string(&value).map_err(|_| StoreKitError::InvalidData)?)
                .map_err(|_| StoreKitError::InvalidData)?;
        call(|context, completion| invoke(json.as_ptr(), context, completion)).await
    }

    pub async fn products(ids: &[String]) -> Result<Vec<Product>, StoreKitError> {
        call_json(
            serde_json::json!({"ids": ids}),
            |json, context, callback| unsafe { storekit_products(json, context, callback) },
        )
        .await
    }
    pub async fn purchase(
        product_id: &str,
        app_account_token: &str,
    ) -> Result<PurchaseResult, StoreKitError> {
        call_json(
            serde_json::json!({"productId": product_id, "appAccountToken": app_account_token}),
            |json, context, callback| unsafe { storekit_purchase(json, context, callback) },
        )
        .await
    }
    pub async fn finish(transaction_id: &str) -> Result<bool, StoreKitError> {
        #[derive(Deserialize)]
        struct Response {
            finished: bool,
        }
        call_json(
            serde_json::json!({"transactionId": transaction_id}),
            |json, context, callback| unsafe { storekit_finish(json, context, callback) },
        )
        .await
        .map(|response: Response| response.finished)
    }
    pub async fn unfinished() -> Result<Vec<Transaction>, StoreKitError> {
        call(|context, callback| unsafe { storekit_unfinished(context, callback) }).await
    }
    pub async fn current_entitlements() -> Result<Vec<Transaction>, StoreKitError> {
        call(|context, callback| unsafe { storekit_current_entitlements(context, callback) }).await
    }
    pub async fn sync() -> Result<bool, StoreKitError> {
        #[derive(Deserialize)]
        struct Response {
            synced: bool,
        }
        call(|context, callback| unsafe { storekit_sync(context, callback) })
            .await
            .map(|response: Response| response.synced)
    }
    pub async fn storefront_country() -> Result<Option<String>, StoreKitError> {
        #[derive(Deserialize)]
        struct Response {
            country: Option<String>,
        }
        call(|context, callback| unsafe { storekit_storefront_country(context, callback) })
            .await
            .map(|response: Response| response.country)
    }
    pub fn observe_transaction_updates() -> Result<TransactionUpdates, StoreKitError> {
        observer(
            |context, callback| unsafe { storekit_observe_transaction_updates(context, callback) },
            transaction_update,
        )
        .map(TransactionUpdates)
    }
}

#[cfg(target_os = "macos")]
#[allow(unused_imports)] // S2 consumes this facade after its module lands.
pub use platform::*;

#[cfg(not(target_os = "macos"))]
pub async fn products(_: &[String]) -> Result<Vec<Product>, StoreKitError> {
    Err(StoreKitError::Unsupported)
}
#[cfg(not(target_os = "macos"))]
pub async fn purchase(_: &str, _: &str) -> Result<PurchaseResult, StoreKitError> {
    Err(StoreKitError::Unsupported)
}
#[cfg(not(target_os = "macos"))]
pub async fn finish(_: &str) -> Result<bool, StoreKitError> {
    Err(StoreKitError::Unsupported)
}
#[cfg(not(target_os = "macos"))]
pub async fn unfinished() -> Result<Vec<Transaction>, StoreKitError> {
    Err(StoreKitError::Unsupported)
}
#[cfg(not(target_os = "macos"))]
pub async fn current_entitlements() -> Result<Vec<Transaction>, StoreKitError> {
    Err(StoreKitError::Unsupported)
}
#[cfg(not(target_os = "macos"))]
pub async fn sync() -> Result<bool, StoreKitError> {
    Err(StoreKitError::Unsupported)
}
#[cfg(not(target_os = "macos"))]
pub async fn storefront_country() -> Result<Option<String>, StoreKitError> {
    Err(StoreKitError::Unsupported)
}

#[cfg(not(target_os = "macos"))]
pub struct TransactionUpdates;
#[cfg(not(target_os = "macos"))]
impl TransactionUpdates {
    pub async fn recv(&mut self) -> Option<Result<Transaction, StoreKitError>> {
        None
    }
}
#[cfg(not(target_os = "macos"))]
pub fn observe_transaction_updates() -> Result<TransactionUpdates, StoreKitError> {
    Err(StoreKitError::Unsupported)
}
#[cfg(test)]
mod tests {
    use super::Transaction;

    #[test]
    fn transaction_entries_require_an_id_and_signed_payload() {
        let transaction: Transaction = serde_json::from_value(serde_json::json!({
            "transactionId": "42",
            "signedTransaction": "signed-payload"
        }))
        .unwrap();
        assert_eq!(transaction.transaction_id, "42");
        assert_eq!(transaction.signed_transaction, "signed-payload");
        assert!(serde_json::from_value::<Transaction>(serde_json::json!({
            "signedTransaction": "signed-payload"
        }))
        .is_err());
    }
}
