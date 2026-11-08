use crate::{Client, Error, Store};
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
    pub fn hold_store_lock_for_test(&self) -> Result<StoreLockForTest<'_>, Error> {
        self.lock().map(StoreLockForTest)
    }
}
