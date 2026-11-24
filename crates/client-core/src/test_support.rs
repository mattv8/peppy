use crate::{Client, Error, RcsProviderRow, RcsProviderRowRoute, Store};
use std::sync::MutexGuard;

#[doc(hidden)]
pub struct StoreLockForTest<'a>(MutexGuard<'a, Store>);

impl Drop for StoreLockForTest<'_> {
    fn drop(&mut self) {
        let _ = &self.0;
    }
}

impl Client {
    #[doc(hidden)]
    pub fn route_rcs_provider_row_at(
        &self,
        row: RcsProviderRow,
        now_ms: i64,
    ) -> Result<RcsProviderRowRoute, Error> {
        self.route_rcs_provider_row_result_at(row, move || now_ms)
            .map(|result| result.route)
    }

    #[doc(hidden)]
    pub fn hold_store_lock_for_test(&self) -> Result<StoreLockForTest<'_>, Error> {
        self.lock().map(StoreLockForTest)
    }
}
