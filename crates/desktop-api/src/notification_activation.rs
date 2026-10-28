use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Identifier-only intent scoped to the active native identity instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct NotificationActivation {
    pub request_id: String,
    pub session_generation: String,
    pub destination: NotificationActivationDestination,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(tag = "kind", rename_all = "camelCase")]
pub enum NotificationActivationDestination {
    Conversation {
        #[serde(rename = "conversationId")]
        #[ts(rename = "conversationId")]
        conversation_id: String,
    },
    Mirrored {
        target: NotificationActivationTarget,
    },
    Main,
}

/// A notification's complete identity, including its original lifetime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct NotificationActivationTarget {
    pub source_device_id: String,
    pub notification_key: String,
    pub lifetime: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn notification_activation_roundtrips_every_destination_with_exact_wire_shape() {
        let destinations = [
            (
                NotificationActivationDestination::Conversation {
                    conversation_id: "conversation".into(),
                },
                json!({ "kind": "conversation", "conversationId": "conversation" }),
            ),
            (
                NotificationActivationDestination::Mirrored {
                    target: NotificationActivationTarget {
                        source_device_id: "phone".into(),
                        notification_key: "key".into(),
                        lifetime: "18446744073709551615".into(),
                    },
                },
                json!({
                    "kind": "mirrored",
                    "target": {
                        "sourceDeviceId": "phone",
                        "notificationKey": "key",
                        "lifetime": "18446744073709551615",
                    },
                }),
            ),
            (
                NotificationActivationDestination::Main,
                json!({ "kind": "main" }),
            ),
        ];
        for (destination, wire_destination) in destinations {
            let activation = NotificationActivation {
                request_id: "18446744073709551615".into(),
                session_generation: "opaque-generation".into(),
                destination,
            };
            let wire = json!({
                "requestId": "18446744073709551615",
                "sessionGeneration": "opaque-generation",
                "destination": wire_destination,
            });
            assert_eq!(serde_json::to_value(&activation).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<NotificationActivation>(wire).unwrap(),
                activation
            );
        }
    }

    #[test]
    fn notification_activation_requires_full_mirrored_identity() {
        for missing in ["sourceDeviceId", "notificationKey", "lifetime"] {
            let mut target = json!({
                "sourceDeviceId": "phone", "notificationKey": "key", "lifetime": "1",
            });
            target.as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<NotificationActivationDestination>(json!({
                    "kind": "mirrored", "target": target,
                }))
                .is_err()
            );
        }
    }
}
