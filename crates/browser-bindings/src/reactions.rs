//! Reaction commands for the browser worker. They use the same DTOs and routing as the desktop
//! host (`peppy_desktop_api::reactions`), with the browser's gateway context and vault.

use crate::{BrowserCore, Failure, core, invalid, unknown_command};
use peppy_client_core::MessageId;
use serde::Deserialize;
use serde_json::Value;
use std::str::FromStr;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReactInput {
    message_id: String,
    emoji: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DismissInput {
    request_id: String,
}

impl BrowserCore {
    pub(super) fn reactions_command(&self, command: &str, args: Value) -> Result<Value, Failure> {
        match command {
            "react_to_message" => self.react_to_message(args),
            "dismiss_reaction_request" => self.dismiss_reaction_request(args),
            _ => Err(unknown_command()),
        }
    }

    fn react_to_message(&self, args: Value) -> Result<Value, Failure> {
        let input: ReactInput = serde_json::from_value(args).map_err(|_| invalid())?;
        let message_id = MessageId::from_str(&input.message_id).map_err(|_| invalid())?;
        let gateways = self.context.gateways.as_deref().unwrap_or_default();
        let view =
            peppy_desktop_api::reactions::react(self.client()?, message_id, input.emoji, gateways)
                .map_err(core)?;
        serde_json::to_value(view).map_err(|_| core(peppy_client_core::Error::Database))
    }

    fn dismiss_reaction_request(&self, args: Value) -> Result<Value, Failure> {
        let input: DismissInput = serde_json::from_value(args).map_err(|_| invalid())?;
        // The browser has no composer windows, so there is no conversation scope to apply.
        peppy_desktop_api::reactions::dismiss(self.client()?, &input.request_id, None)
            .map_err(core)?;
        Ok(Value::Object(Default::default()))
    }
}
