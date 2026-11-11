#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ActionAvailability {
    pub copy: bool,
    pub reveal: bool,
    pub reply: bool,
}

impl From<bool> for ActionAvailability {
    fn from(copy: bool) -> Self {
        Self {
            copy,
            ..Self::default()
        }
    }
}
