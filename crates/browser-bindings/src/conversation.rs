use super::*;

pub(super) fn handles(command: &str) -> bool {
    matches!(command, "set_conversation_pinned" | "set_conversation_tone")
}

impl BrowserCore {
    pub(super) fn conversation_command(
        &mut self,
        command: &str,
        args: Value,
    ) -> Result<Value, Failure> {
        match command {
            "set_conversation_pinned" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Input {
                    conversation_id: String,
                    pinned: bool,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                let conversation_id =
                    ConversationId::from_str(&input.conversation_id).map_err(|_| invalid())?;
                self.client()?
                    .set_conversation_pinned(conversation_id, input.pinned)
                    .map_err(core)?;
            }
            "set_conversation_tone" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Input {
                    seed: String,
                    palette_index: Option<u8>,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                self.client()?
                    .set_conversation_tone(&input.seed, input.palette_index)
                    .map_err(core)?;
            }
            _ => return Err(unknown_command()),
        }
        Ok(json!({}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::IncomingSms;
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    fn response(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        serde_json::from_str(&core.dispatch(&json!({"command": command, "args": args}).to_string()))
            .unwrap()
    }

    #[test]
    fn exposes_only_the_conversation_commands() {
        assert!(handles("set_conversation_pinned"));
        assert!(handles("set_conversation_tone"));
        assert!(!handles("set_conversation_pin"));
    }

    #[test]
    fn pin_mutation_rejects_locked_sessions_and_projects_the_core_pin_state() {
        let directory = tempfile::tempdir().unwrap();
        let vault = VaultId::new();
        let device = DeviceId::new();
        let mut core = BrowserCore::new(directory.path().to_owned());
        response(
            &mut core,
            "open",
            json!({"vaultId": vault, "deviceId": device, "databaseKey": vec![1; 32], "origin": "https://peppy.test", "deviceRole": "device"}),
        );
        assert!(
            response(&mut core, "snapshot", json!({}))["value"]
                .get("toneOverrides")
                .is_none()
        );
        let conversation = ConversationId::new();
        assert_eq!(
            response(
                &mut core,
                "set_conversation_pinned",
                json!({"conversationId": conversation, "pinned": true}),
            )["error"]["code"],
            "locked"
        );

        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("browser fixture", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        assert_eq!(
            response(
                &mut core,
                "unlock",
                json!({"profile": profile, "header": header, "passphrase": "browser fixture"}),
            )["ok"],
            true
        );
        core.client()
            .unwrap()
            .capture_incoming(IncomingSms {
                conversation_id: Some(conversation),
                sender_address: "+15555550100".into(),
                body: "hello".into(),
                provider_message_id: Some("conversation-pin".into()),
                imported: false,
            })
            .unwrap();

        assert_eq!(
            response(
                &mut core,
                "set_conversation_pinned",
                json!({"conversationId": conversation, "pinned": true}),
            )["ok"],
            true
        );
        assert_eq!(
            response(&mut core, "snapshot", json!({}))["value"]["conversations"][0]["pinned"],
            true
        );
        assert_eq!(
            response(
                &mut core,
                "set_conversation_tone",
                json!({"seed": "contact:+15555550100", "paletteIndex": 60}),
            )["ok"],
            true
        );
        assert_eq!(
            response(&mut core, "snapshot", json!({}))["value"]["toneOverrides"],
            json!({"contact:+15555550100": 60})
        );
    }

    #[test]
    fn tone_mutation_rejects_invalid_json_palette_indexes_without_wrapping() {
        let directory = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(directory.path().to_owned());

        let response = response(
            &mut core,
            "set_conversation_tone",
            json!({"seed": "contact:+15555550100", "paletteIndex": 256}),
        );

        assert_eq!(response["error"]["code"], "invalid-request");
    }
}
