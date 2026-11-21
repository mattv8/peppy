use peppy_client_core::Error as CoreError;

/// Shared errors remain host-unsserialized until their final boundary mapper.
#[derive(Debug)]
pub enum ContactError {
    Ui {
        code: &'static str,
        message: &'static str,
    },
    Core(CoreError),
}

impl ContactError {
    pub fn ui(code: &'static str, message: &'static str) -> Self {
        Self::Ui { code, message }
    }
}

impl From<CoreError> for ContactError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

pub type ContactResult<T> = Result<T, ContactError>;

/// Refusal before core runs: the host has no live connection to the server.
pub fn delete_book_offline() -> ContactError {
    ContactError::ui(
        "offline",
        "Connect to the server to delete this book for all devices.",
    )
}

/// Map the deletion producer's static refusal reasons at both host boundaries.
pub fn delete_book_error(error: CoreError) -> ContactError {
    match error {
        CoreError::InvalidRequest("contact book deletion needs an up-to-date sync") => {
            ContactError::ui(
                "sync-not-current",
                "Finish syncing to delete this book for all devices.",
            )
        }
        CoreError::InvalidRequest("contact book was already deleted for all devices") => {
            ContactError::ui(
                "contact-book-deleted",
                "This contact book was already deleted.",
            )
        }
        CoreError::InvalidRequest(
            "contact book owner is not a removed device"
            | "contact book belongs to this device"
            | "contact book owner was succeeded by another device",
        ) => ContactError::ui(
            "contact-book-not-deletable",
            "This contact book can't be deleted for all devices.",
        ),
        CoreError::InvalidRequest("contact book is not on this device") => {
            ContactError::ui("not-found", "The contact book was not found.")
        }
        CoreError::InvalidRequest("contact book deletion input is invalid") => {
            ContactError::ui("invalid-contact-edit", "The contact book is invalid.")
        }
        other => ContactError::Core(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_and_message(error: ContactError) -> (&'static str, &'static str) {
        match error {
            ContactError::Ui { code, message } => (code, message),
            ContactError::Core(error) => panic!("expected a UI refusal, got {error:?}"),
        }
    }

    #[test]
    fn offline_and_sync_refusals_have_stable_codes_and_copy() {
        assert_eq!(
            code_and_message(delete_book_offline()),
            (
                "offline",
                "Connect to the server to delete this book for all devices."
            )
        );
        assert_eq!(
            code_and_message(delete_book_error(CoreError::InvalidRequest(
                "contact book deletion needs an up-to-date sync"
            ))),
            (
                "sync-not-current",
                "Finish syncing to delete this book for all devices."
            )
        );
    }

    #[test]
    fn unrelated_core_refusals_keep_their_core_error() {
        assert!(matches!(
            delete_book_error(CoreError::InvalidRequest(
                "contact book is not on this device"
            )),
            ContactError::Ui {
                code: "not-found",
                ..
            }
        ));
        assert!(matches!(
            delete_book_error(CoreError::KeysUnavailable),
            ContactError::Core(CoreError::KeysUnavailable)
        ));
    }
}
