use crate::notification_persistence::NotificationPersistence;

pub(crate) fn toast_xml(
    title: &str,
    body: &str,
    icon_uri: Option<&str>,
    audio: &str,
    persistence: NotificationPersistence,
) -> String {
    let icon = icon_uri.map_or_else(String::new, |uri| {
        format!(
            "<image placement=\"appLogoOverride\" src=\"{}\"/>",
            escape_attribute(uri)
        )
    });
    let (attributes, actions) = match persistence.seconds {
        Some(seconds) if seconds <= 7 => (" duration=\"short\"", ""),
        Some(_) => (" duration=\"long\"", ""),
        None => (
            " scenario=\"reminder\"",
            "<actions><action content=\"\" arguments=\"dismiss\" activationType=\"system\"/></actions>",
        ),
    };
    format!(
        "<toast{attributes}><visual><binding template=\"ToastGeneric\">{icon}<text>{}</text><text>{}</text></binding></visual>{audio}{actions}</toast>",
        escape_text(title),
        escape_text(body),
    )
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attribute(text: &str) -> String {
    escape_text(text)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::toast_xml;
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
            ),
            "<toast duration=\"short\"><visual><binding template=\"ToastGeneric\"><image placement=\"appLogoOverride\" src=\"file:///C:/A&amp;B&lt;&gt;&apos;&quot;.png\"/><text>T&lt;&amp;&gt;\"'</text><text>B&lt;&amp;&gt;\"'</text></binding></visual><audio silent=\"true\"/></toast>"
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
                }
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
                }
            )
            .starts_with("<toast duration=\"long\">"));
        }
        assert_eq!(
            toast_xml("", "", None, "<audio silent=\"true\"/>", NotificationPersistence { seconds: None }),
            "<toast scenario=\"reminder\"><visual><binding template=\"ToastGeneric\"><text></text><text></text></binding></visual><audio silent=\"true\"/><actions><action content=\"\" arguments=\"dismiss\" activationType=\"system\"/></actions></toast>"
        );
    }
}
