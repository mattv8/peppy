#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ActionAvailability {
    pub copy: bool,
    pub reveal: bool,
    pub reply: bool,
    pub mark_read: bool,
    /// The OS must report a user dismissal so the phone can clear the notification.
    pub report_dismiss: bool,
}

impl From<bool> for ActionAvailability {
    fn from(copy: bool) -> Self {
        Self {
            copy,
            ..Self::default()
        }
    }
}
