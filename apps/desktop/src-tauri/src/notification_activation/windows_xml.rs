use super::ActionAvailability;
use crate::notification_persistence::NotificationPersistence;

pub(crate) fn toast_xml(
    title: &str,
    body: &str,
    icon_uri: Option<&str>,
    audio: &str,
    persistence: NotificationPersistence,
    actions: impl Into<ActionAvailability>,
) -> String {
    toast_xml_with_icons(title, body, icon_uri, audio, persistence, actions, None)
}

pub(crate) fn toast_xml_with_icons(
    title: &str,
    body: &str,
    icon_uri: Option<&str>,
    audio: &str,
    persistence: NotificationPersistence,
    actions: impl Into<ActionAvailability>,
    action_icons: Option<ActionIconUris<'_>>,
) -> String {
    let actions = actions.into();
    let icon = icon_uri.map_or_else(String::new, |uri| {
        format!(
            "<image placement=\"appLogoOverride\" src=\"{}\"/>",
            escape_attribute(uri)
        )
    });
    // Indefinite banners keep the system dismiss action. Every action lives in this one
    // `<actions>` element, so a copy action never duplicates it.
    let icons = action_icons.filter(|icons| icons.all_valid());
    let (attributes, dismiss) = match persistence.seconds {
        Some(seconds) if seconds <= 7 => (" duration=\"short\"", String::new()),
        Some(_) => (" duration=\"long\"", String::new()),
        None => (
            " scenario=\"reminder\"",
            action(
                "",
                "dismiss",
                "system",
                icons.as_ref().map(|icons| icons.dismiss.as_ref()),
            ),
        ),
    };
    let copy = if actions.copy {
        action(
            "Copy code",
            "copy-code",
            "foreground",
            icons.as_ref().map(|icons| icons.copy.as_ref()),
        )
    } else {
        String::new()
    };
    let reveal = if actions.reveal {
        action(
            "Show message",
            "reveal",
            "foreground",
            icons.as_ref().map(|icons| icons.show.as_ref()),
        )
    } else {
        String::new()
    };
    let reply = if actions.reply {
        format!(
            "<input id=\"reply\" type=\"text\" placeHolderContent=\"Type a reply\"/>{}",
            action(
                "Send",
                "reply",
                "foreground",
                icons.as_ref().map(|icons| icons.send.as_ref())
            )
            .replace("/>", " hint-inputId=\"reply\"/>")
        )
    } else {
        String::new()
    };
    let actions = if dismiss.is_empty() && copy.is_empty() && reveal.is_empty() && reply.is_empty()
    {
        String::new()
    } else {
        format!("<actions>{dismiss}{copy}{reveal}{reply}</actions>")
    };
    format!(
        "<toast{attributes}><visual><binding template=\"ToastGeneric\">{icon}<text>{}</text><text>{}</text></binding></visual>{audio}{actions}</toast>",
        escape_text(title),
        escape_text(body),
    )
}

pub(crate) struct ActionIconUris<'a> {
    pub show: std::borrow::Cow<'a, str>,
    pub send: std::borrow::Cow<'a, str>,
    pub copy: std::borrow::Cow<'a, str>,
    pub dismiss: std::borrow::Cow<'a, str>,
}

impl ActionIconUris<'_> {
    fn all_valid(&self) -> bool {
        [
            self.show.as_ref(),
            self.send.as_ref(),
            self.copy.as_ref(),
            self.dismiss.as_ref(),
        ]
        .iter()
        .all(|uri| uri.starts_with("file:"))
    }
}

fn action(content: &str, arguments: &str, activation_type: &str, icon_uri: Option<&str>) -> String {
    let image = icon_uri
        .map(|uri| format!(" imageUri=\"{}\"", escape_attribute(uri)))
        .unwrap_or_default();
    format!(
        "<action content=\"{}\" arguments=\"{}\" activationType=\"{}\"{image}/>",
        escape_attribute(content),
        escape_attribute(arguments),
        activation_type
    )
}

fn escape_text(text: &str) -> String {
    xml_1_0(text)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attribute(text: &str) -> String {
    escape_text(text)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_1_0(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            matches!(
                *character as u32,
                0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{toast_xml, toast_xml_with_icons, ActionAvailability, ActionIconUris};
    use crate::notification_persistence::NotificationPersistence;

    #[test]
    fn escapes_text_and_icon_uri_attributes() {
        assert_eq!(
            toast_xml(
                "T<&>\"'",
                "B<&>\"'",
                Some("file:///C:/A&B<>'\".png"),
                "<audio silent=\"true\"/>",
                NotificationPersistence { seconds: Some(5) },
                false,
            ),
            "<toast duration=\"short\"><visual><binding template=\"ToastGeneric\"><image placement=\"appLogoOverride\" src=\"file:///C:/A&amp;B&lt;&gt;&apos;&quot;.png\"/><text>T&lt;&amp;&gt;\"'</text><text>B&lt;&amp;&gt;\"'</text></binding></visual><audio silent=\"true\"/></toast>"
        );
    }

    #[test]
    fn filters_xml_1_0_invalid_scalars_from_text_and_attributes() {
        assert_eq!(
            toast_xml(
                "valid\t\n\r\u{0000}\u{0008}\u{000B}\u{000C}\u{001F}\u{D7FF}\u{E000}",
                "body\u{0000}\u{10FFFF}",
                Some("file:///icon\u{0000}.png"),
                "",
                NotificationPersistence { seconds: Some(5) },
                false,
            ),
            "<toast duration=\"short\"><visual><binding template=\"ToastGeneric\"><image placement=\"appLogoOverride\" src=\"file:///icon.png\"/><text>valid\t\n\r\u{D7FF}\u{E000}</text><text>body\u{10FFFF}</text></binding></visual></toast>"
        );
    }

    #[test]
    fn omits_image_and_preserves_supplied_audio() {
        assert_eq!(
            toast_xml(
                "Title",
                "",
                None,
                "<audio src=\"ms-winsoundevent:Notification.Default\"/>",
                NotificationPersistence { seconds: Some(60) },
                false,
            ),
            "<toast duration=\"long\"><visual><binding template=\"ToastGeneric\"><text>Title</text><text></text></binding></visual><audio src=\"ms-winsoundevent:Notification.Default\"/></toast>"
        );
    }

    #[test]
    fn maps_boundaries_and_indefinite_reminders() {
        for seconds in [5, 7] {
            assert!(toast_xml(
                "",
                "",
                None,
                "",
                NotificationPersistence {
                    seconds: Some(seconds)
                },
                false,
            )
            .starts_with("<toast duration=\"short\">"));
        }
        for seconds in [8, 60] {
            assert!(toast_xml(
                "",
                "",
                None,
                "",
                NotificationPersistence {
                    seconds: Some(seconds)
                },
                false,
            )
            .starts_with("<toast duration=\"long\">"));
        }
        assert_eq!(
            toast_xml(
                "",
                "",
                None,
                "<audio silent=\"true\"/>",
                NotificationPersistence { seconds: None },
                false,
            ),
            "<toast scenario=\"reminder\"><visual><binding template=\"ToastGeneric\"><text></text><text></text></binding></visual><audio silent=\"true\"/><actions><action content=\"\" arguments=\"dismiss\" activationType=\"system\"/></actions></toast>"
        );
    }

    #[test]
    fn merges_copy_action_with_each_persistence_mode() {
        for persistence in [
            NotificationPersistence { seconds: Some(5) },
            NotificationPersistence { seconds: Some(60) },
            NotificationPersistence { seconds: None },
        ] {
            let xml = toast_xml("", "", None, "", persistence, true);
            assert_eq!(xml.matches("<actions>").count(), 1);
            assert!(xml.contains("content=\"Copy code\" arguments=\"copy-code\""));
        }
    }

    #[test]
    fn indefinite_copy_keeps_dismiss_and_copy_in_one_actions_element() {
        assert_eq!(
            toast_xml(
                "",
                "",
                None,
                "",
                NotificationPersistence { seconds: None },
                true,
            ),
            "<toast scenario=\"reminder\"><visual><binding template=\"ToastGeneric\"><text></text><text></text></binding></visual><actions><action content=\"\" arguments=\"dismiss\" activationType=\"system\"/><action content=\"Copy code\" arguments=\"copy-code\" activationType=\"foreground\"/></actions></toast>"
        );
    }

    #[test]
    fn reply_and_copy_share_one_actions_container_for_finite_and_reminder_toasts() {
        for persistence in [
            NotificationPersistence { seconds: Some(60) },
            NotificationPersistence { seconds: None },
        ] {
            let xml = toast_xml(
                "",
                "",
                None,
                "",
                persistence,
                super::ActionAvailability {
                    copy: true,
                    reveal: false,
                    reply: true,
                },
            );
            assert_eq!(xml.matches("<actions>").count(), 1);
            assert!(xml.contains(
                "<input id=\"reply\" type=\"text\" placeHolderContent=\"Type a reply\"/>"
            ));
            assert!(xml.contains("content=\"Send\" arguments=\"reply\" activationType=\"foreground\" hint-inputId=\"reply\""));
            assert!(xml.contains("content=\"Copy code\" arguments=\"copy-code\""));
            if persistence.seconds.is_none() {
                assert!(xml.contains("arguments=\"dismiss\" activationType=\"system\""));
            }
        }
    }

    #[test]
    fn action_icons_are_emitted_only_when_every_button_has_a_file_uri() {
        let icons = ActionIconUris {
            show: "file:///show.png".into(),
            send: "file:///send.png".into(),
            copy: "file:///copy.png".into(),
            dismiss: "file:///dismiss.png".into(),
        };
        let xml = toast_xml_with_icons(
            "",
            "",
            None,
            "",
            NotificationPersistence { seconds: None },
            ActionAvailability {
                copy: true,
                reveal: true,
                reply: true,
            },
            Some(icons),
        );
        for uri in ["show", "send", "copy", "dismiss"] {
            assert!(xml.contains(&format!("imageUri=\"file:///{uri}.png\"")));
        }
        let plain = toast_xml_with_icons(
            "",
            "",
            None,
            "",
            NotificationPersistence { seconds: Some(60) },
            ActionAvailability {
                reveal: true,
                ..ActionAvailability::default()
            },
            Some(ActionIconUris {
                show: "not-a-file".into(),
                send: "file:///send.png".into(),
                copy: "file:///copy.png".into(),
                dismiss: "file:///dismiss.png".into(),
            }),
        );
        assert!(!plain.contains("imageUri="));
        assert!(plain.contains("content=\"Show message\""));
    }

    #[test]
    fn copy_absent_leaves_persistence_xml_unchanged() {
        for (seconds, expected) in [
            (Some(5), "<toast duration=\"short\"><visual><binding template=\"ToastGeneric\"><text></text><text></text></binding></visual></toast>"),
            (None, "<toast scenario=\"reminder\"><visual><binding template=\"ToastGeneric\"><text></text><text></text></binding></visual><actions><action content=\"\" arguments=\"dismiss\" activationType=\"system\"/></actions></toast>"),
        ] {
            assert_eq!(
                toast_xml("", "", None, "", NotificationPersistence { seconds }, false),
                expected
            );
        }
    }
}
