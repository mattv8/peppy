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
