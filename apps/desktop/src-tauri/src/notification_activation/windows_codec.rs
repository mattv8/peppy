#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActivationAction {
    Click,
    Copy,
    Reveal,
    Reply,
}

pub(super) fn parse_action(argument: Option<&str>) -> Option<ActivationAction> {
    match argument {
        Some("") => Some(ActivationAction::Click),
        Some("copy-code") => Some(ActivationAction::Copy),
        Some("reveal") => Some(ActivationAction::Reveal),
        Some("reply") => Some(ActivationAction::Reply),
        Some(_) | None => None,
    }
}

#[cfg(target_os = "windows")]
pub(super) fn action_argument(
    arguments: &windows::UI::Notifications::ToastActivatedEventArgs,
) -> Option<String> {
    arguments
        .Arguments()
        .ok()
        .and_then(|value| String::try_from(value).ok())
}

#[cfg(target_os = "windows")]
pub(super) fn reply_body(
    arguments: &windows::UI::Notifications::ToastActivatedEventArgs,
) -> Option<String> {
    use windows::{
        core::{Interface, HSTRING},
        Foundation::IPropertyValue,
    };

    arguments
        .UserInput()
        .ok()?
        .Lookup(&HSTRING::from("reply"))
        .ok()?
        .cast::<IPropertyValue>()
        .ok()?
        .GetString()
        .ok()
        .and_then(|value| String::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use super::{parse_action, ActivationAction};

    #[test]
    fn parses_only_allowlisted_activation_actions() {
        assert_eq!(parse_action(Some("")), Some(ActivationAction::Click));
        assert_eq!(
            parse_action(Some("copy-code")),
            Some(ActivationAction::Copy)
        );
        assert_eq!(parse_action(Some("reveal")), Some(ActivationAction::Reveal));
        assert_eq!(parse_action(Some("reply")), Some(ActivationAction::Reply));
        assert_eq!(parse_action(Some("dismiss")), None);
    }

    #[test]
    fn missing_activation_argument_is_not_a_default_click() {
        assert_eq!(parse_action(None), None);
    }
}
