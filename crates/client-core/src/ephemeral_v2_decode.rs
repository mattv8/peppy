//! Stateless authentication boundary for Ephemeral V2 frames.

use std::collections::BTreeMap;

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use peppy_crypto::{decrypt, EncryptedEnvelope};
use peppy_protocol::{
    ChallengePlaintextV2, EphemeralSignalV2, EphemeralSignalV2Kind, SessionAckPlaintextV2,
    TypingOriginV2, TypingPlaintextV2,
};

use super::{DeviceId, EpochKeys, VaultId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AuthenticatedPlaintext {
    Typing(TypingPlaintextV2),
    Challenge(ChallengePlaintextV2),
    SessionAck(SessionAckPlaintextV2),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthenticatedFrame {
    pub(crate) sender_device_id: DeviceId,
    pub(crate) session_id: [u8; 16],
    pub(crate) sequence: u64,
    pub(crate) remaining_ttl_ms: u64,
    pub(crate) plaintext: AuthenticatedPlaintext,
}

pub(crate) fn decode_authenticated_frame(
    effective_wall_ms: u64,
    vault_id: VaultId,
    local_device_id: DeviceId,
    local_session_id: [u8; 16],
    trusted_roster: &[DeviceId],
    frame: &EphemeralSignalV2,
    unlocked_epochs: &BTreeMap<u32, EpochKeys>,
) -> Option<AuthenticatedFrame> {
    if trusted_roster.len() > 100
        || frame.validate().is_err()
        || frame.sender_device_id == local_device_id
        || !trusted_roster.contains(&frame.sender_device_id)
    {
        return None;
    }

    let sent_at_ms = frame.sent_at_ms.parse::<u64>().ok()?;
    let expires_at_ms = frame.expires_at_ms.parse::<u64>().ok()?;
    if sent_at_ms > effective_wall_ms || effective_wall_ms >= expires_at_ms {
        return None;
    }
    let remaining_ttl_ms = expires_at_ms.checked_sub(effective_wall_ms)?;
    let sequence = frame.sequence.parse::<u64>().ok()?;
    let session_id = parse_session_id(&frame.session_id)?;
    let key = &unlocked_epochs.get(&frame.key_epoch)?.event;
    let encrypted = parse_envelope(&frame.ciphertext_b64)?;
    let aad = frame.aad_bytes(vault_id).ok()?;
    let plaintext = decrypt(key, &aad, &encrypted).ok()?;
    let plaintext = deserialize_and_check(frame, &plaintext, local_device_id, local_session_id)?;

    Some(AuthenticatedFrame {
        sender_device_id: frame.sender_device_id,
        session_id,
        sequence,
        remaining_ttl_ms,
        plaintext,
    })
}

fn parse_session_id(value: &str) -> Option<[u8; 16]> {
    let decoded = URL_SAFE_NO_PAD.decode(value).ok()?;
    decoded.try_into().ok()
}

fn parse_envelope(ciphertext_b64: &str) -> Option<EncryptedEnvelope> {
    let bytes = STANDARD.decode(ciphertext_b64).ok()?;
    let (nonce, ciphertext) = bytes.split_at_checked(24)?;
    Some(EncryptedEnvelope {
        nonce: nonce.try_into().ok()?,
        ciphertext: ciphertext.to_vec(),
    })
}

fn deserialize_and_check(
    frame: &EphemeralSignalV2,
    plaintext: &[u8],
    local_device_id: DeviceId,
    local_session_id: [u8; 16],
) -> Option<AuthenticatedPlaintext> {
    match frame.kind {
        EphemeralSignalV2Kind::Typing => {
            let value = serde_json::from_slice::<TypingPlaintextV2>(plaintext).ok()?;
            if value.origin == TypingOriginV2::VaultDevice
                && value.gateway_device_id != Some(local_device_id)
            {
                return None;
            }
            Some(AuthenticatedPlaintext::Typing(value))
        }
        EphemeralSignalV2Kind::Challenge => {
            let value = serde_json::from_slice::<ChallengePlaintextV2>(plaintext).ok()?;
            if value.responder_device_id != local_device_id
                || value.challenger_device_id != frame.sender_device_id
                || parse_session_id(&value.challenger_session_id)?
                    != parse_session_id(&frame.session_id)?
            {
                return None;
            }
            Some(AuthenticatedPlaintext::Challenge(value))
        }
        EphemeralSignalV2Kind::SessionAck => {
            let value = serde_json::from_slice::<SessionAckPlaintextV2>(plaintext).ok()?;
            if value.responder_device_id != frame.sender_device_id
                || parse_session_id(&value.responder_session_id)?
                    != parse_session_id(&frame.session_id)?
                || value.challenger_device_id != local_device_id
                || parse_session_id(&value.challenger_session_id)? != local_session_id
            {
                return None;
            }
            Some(AuthenticatedPlaintext::SessionAck(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{derive_purpose_key, derive_root_key, encrypt, KeyProfile, KeyPurpose};
    use peppy_protocol::{TypingStateV2, EPHEMERAL_V2_PROTOCOL_VERSION};
    use uuid::Uuid;

    fn device(value: u128) -> DeviceId {
        DeviceId(Uuid::from_u128(value))
    }

    fn vault(value: u128) -> VaultId {
        VaultId(Uuid::from_u128(value))
    }

    fn key_map(vault_id: VaultId, epoch: u32, passphrase: &str) -> BTreeMap<u32, EpochKeys> {
        let profile = KeyProfile::new(vault_id.0, epoch).unwrap();
        let root = derive_root_key(passphrase, &profile).unwrap();
        let derive = |purpose| derive_purpose_key(&root, &profile, purpose).unwrap();
        BTreeMap::from([(
            epoch,
            EpochKeys {
                profile: profile.clone(),
                fingerprint: profile.fingerprint().unwrap(),
                command: derive(KeyPurpose::Command),
                event: derive(KeyPurpose::Event),
                compaction: None,
                local_identity_seal: None,
                handoff: None,
            },
        )])
    }

    fn independent_aad(frame: &EphemeralSignalV2, vault_id: VaultId) -> Vec<u8> {
        let kind = match frame.kind {
            EphemeralSignalV2Kind::Typing => "typing",
            EphemeralSignalV2Kind::Challenge => "challenge",
            EphemeralSignalV2Kind::SessionAck => "session_ack",
        };
        let mut aad = b"peppy-ephemeral-typing-v2\0".to_vec();
        aad.extend_from_slice(vault_id.0.as_bytes());
        aad.extend_from_slice(&EPHEMERAL_V2_PROTOCOL_VERSION.to_be_bytes());
        aad.push(kind.len() as u8);
        aad.extend_from_slice(kind.as_bytes());
        aad.extend_from_slice(frame.sender_device_id.0.as_bytes());
        aad.extend_from_slice(&frame.key_epoch.to_be_bytes());
        aad.extend_from_slice(&parse_session_id(&frame.session_id).unwrap());
        aad.extend_from_slice(&frame.sent_at_ms.parse::<u64>().unwrap().to_be_bytes());
        aad.extend_from_slice(&frame.expires_at_ms.parse::<u64>().unwrap().to_be_bytes());
        aad.extend_from_slice(&frame.sequence.parse::<u64>().unwrap().to_be_bytes());
        aad
    }

    fn seal_frame(
        kind: EphemeralSignalV2Kind,
        sender: DeviceId,
        session_id: [u8; 16],
        epoch: u32,
        plaintext: &[u8],
        vault_id: VaultId,
        keys: &BTreeMap<u32, EpochKeys>,
    ) -> EphemeralSignalV2 {
        let mut frame = EphemeralSignalV2 {
            protocol_version: EPHEMERAL_V2_PROTOCOL_VERSION,
            kind,
            sender_device_id: sender,
            key_epoch: epoch,
            session_id: URL_SAFE_NO_PAD.encode(session_id),
            sent_at_ms: "100".into(),
            expires_at_ms: "15100".into(),
            sequence: "7".into(),
            ciphertext_b64: STANDARD.encode([0]),
        };
        let envelope = encrypt(
            &keys.get(&epoch).unwrap().event,
            &independent_aad(&frame, vault_id),
            plaintext,
        )
        .unwrap();
        let mut bytes = envelope.nonce.to_vec();
        bytes.extend_from_slice(&envelope.ciphertext);
        frame.ciphertext_b64 = STANDARD.encode(bytes);
        frame
    }

    #[test]
    fn decrypts_all_kinds_with_independent_normative_aad() {
        let vault_id = vault(1);
        let local = device(2);
        let sender = device(3);
        let keys = key_map(vault_id, 1, "correct horse battery staple");
        let session = [4; 16];
        let local_session = [5; 16];
        let typing = TypingPlaintextV2 {
            version: EPHEMERAL_V2_PROTOCOL_VERSION,
            origin: TypingOriginV2::VaultDevice,
            conversation_id: Uuid::from_u128(6),
            state: TypingStateV2::Started,
            gateway_device_id: Some(local),
        };
        let challenge = ChallengePlaintextV2 {
            version: EPHEMERAL_V2_PROTOCOL_VERSION,
            challenger_device_id: sender,
            challenger_session_id: URL_SAFE_NO_PAD.encode(session),
            responder_device_id: local,
            nonce_b64: STANDARD.encode([7; 16]),
        };
        let ack = SessionAckPlaintextV2 {
            version: EPHEMERAL_V2_PROTOCOL_VERSION,
            responder_device_id: sender,
            responder_session_id: URL_SAFE_NO_PAD.encode(session),
            challenger_device_id: local,
            challenger_session_id: URL_SAFE_NO_PAD.encode(local_session),
            echoed_nonce_b64: STANDARD.encode([8; 16]),
            sequence_floor: "1".into(),
        };

        for (kind, plaintext) in [
            (
                EphemeralSignalV2Kind::Typing,
                serde_json::to_vec(&typing).unwrap(),
            ),
            (
                EphemeralSignalV2Kind::Challenge,
                serde_json::to_vec(&challenge).unwrap(),
            ),
            (
                EphemeralSignalV2Kind::SessionAck,
                serde_json::to_vec(&ack).unwrap(),
            ),
        ] {
            let frame = seal_frame(kind, sender, session, 1, &plaintext, vault_id, &keys);
            let decoded = decode_authenticated_frame(
                100,
                vault_id,
                local,
                local_session,
                &[sender],
                &frame,
                &keys,
            );
            assert!(decoded.is_some());
        }
    }

    #[test]
    fn rejects_aad_tampering_wrong_vault_and_unknown_but_accepts_old_unlocked_epoch() {
        let vault_id = vault(10);
        let local = device(11);
        let sender = device(12);
        let other_sender = device(13);
        let mut keys = key_map(vault_id, 1, "correct horse battery staple");
        keys.extend(key_map(vault_id, 2, "correct horse battery staple"));
        let payload = serde_json::to_vec(&TypingPlaintextV2 {
            version: 2,
            origin: TypingOriginV2::VaultDevice,
            conversation_id: Uuid::from_u128(14),
            state: TypingStateV2::Started,
            gateway_device_id: Some(local),
        })
        .unwrap();
        let frame = seal_frame(
            EphemeralSignalV2Kind::Typing,
            sender,
            [15; 16],
            1,
            &payload,
            vault_id,
            &keys,
        );
        assert!(decode_authenticated_frame(
            100,
            vault_id,
            local,
            [16; 16],
            &[sender],
            &frame,
            &keys
        )
        .is_some());
        assert!(decode_authenticated_frame(
            100,
            vault(99),
            local,
            [16; 16],
            &[sender],
            &frame,
            &keys
        )
        .is_none());
        let wrong_keys = key_map(vault_id, 1, "another correct horse battery staple");
        assert!(decode_authenticated_frame(
            100,
            vault_id,
            local,
            [16; 16],
            &[sender],
            &frame,
            &wrong_keys
        )
        .is_none());
        let mut tampered = frame.clone();
        tampered.sender_device_id = other_sender;
        assert!(decode_authenticated_frame(
            100,
            vault_id,
            local,
            [16; 16],
            &[sender, other_sender],
            &tampered,
            &keys
        )
        .is_none());
        let old = seal_frame(
            EphemeralSignalV2Kind::Typing,
            sender,
            [17; 16],
            2,
            &payload,
            vault_id,
            &keys,
        );
        assert!(
            decode_authenticated_frame(100, vault_id, local, [16; 16], &[sender], &old, &keys)
                .is_some()
        );
        let mut unknown = frame;
        unknown.key_epoch = 3;
        assert!(decode_authenticated_frame(
            100,
            vault_id,
            local,
            [16; 16],
            &[sender],
            &unknown,
            &keys
        )
        .is_none());
    }

    #[test]
    fn rejects_wrong_address_session_kind_extreme_times_and_malformed_ciphertext() {
        let vault_id = vault(20);
        let local = device(21);
        let sender = device(22);
        let keys = key_map(vault_id, 1, "correct horse battery staple");
        let wrong_address = serde_json::to_vec(&TypingPlaintextV2 {
            version: 2,
            origin: TypingOriginV2::VaultDevice,
            conversation_id: Uuid::from_u128(23),
            state: TypingStateV2::Started,
            gateway_device_id: Some(device(24)),
        })
        .unwrap();
        let frame = seal_frame(
            EphemeralSignalV2Kind::Typing,
            sender,
            [25; 16],
            1,
            &wrong_address,
            vault_id,
            &keys,
        );
        assert!(decode_authenticated_frame(
            100,
            vault_id,
            local,
            [26; 16],
            &[sender],
            &frame,
            &keys
        )
        .is_none());

        let challenge = ChallengePlaintextV2 {
            version: 2,
            challenger_device_id: sender,
            challenger_session_id: URL_SAFE_NO_PAD.encode([27; 16]),
            responder_device_id: local,
            nonce_b64: STANDARD.encode([28; 16]),
        };
        let wrong_session = seal_frame(
            EphemeralSignalV2Kind::Challenge,
            sender,
            [29; 16],
            1,
            &serde_json::to_vec(&challenge).unwrap(),
            vault_id,
            &keys,
        );
        assert!(decode_authenticated_frame(
            100,
            vault_id,
            local,
            [26; 16],
            &[sender],
            &wrong_session,
            &keys
        )
        .is_none());
        let kind_mismatch = seal_frame(
            EphemeralSignalV2Kind::Typing,
            sender,
            [30; 16],
            1,
            &serde_json::to_vec(&challenge).unwrap(),
            vault_id,
            &keys,
        );
        assert!(decode_authenticated_frame(
            100,
            vault_id,
            local,
            [26; 16],
            &[sender],
            &kind_mismatch,
            &keys
        )
        .is_none());

        let mut malformed = frame.clone();
        malformed.ciphertext_b64 = STANDARD.encode([0; 23]);
        assert!(decode_authenticated_frame(
            100,
            vault_id,
            local,
            [26; 16],
            &[sender],
            &malformed,
            &keys
        )
        .is_none());
        let mut extreme = frame;
        extreme.sent_at_ms = u64::MAX.to_string();
        extreme.expires_at_ms = u64::MAX.to_string();
        assert!(decode_authenticated_frame(
            u64::MAX,
            vault_id,
            local,
            [26; 16],
            &[sender],
            &extreme,
            &keys
        )
        .is_none());
    }
}
