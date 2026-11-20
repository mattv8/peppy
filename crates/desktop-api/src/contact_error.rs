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

/// Map the deletion producer's static refusal reasons at both host boundaries.
pub fn delete_book_error(error: CoreError) -> ContactError {
    match error {
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
