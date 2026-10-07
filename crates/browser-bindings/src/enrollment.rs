//! Worker-private join cryptography. HTTP stays in the Worker scheduler, while ephemeral private
//! keys, sealed-intent decryption and proof signing never cross the Rust boundary.
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_hosted_client::{
    claim::{EnrollmentKey, pairing_proof_bytes},
    join::{
        JoinKeySecret, JoinRequestQr, encode_join_request_qr, generate_join_key, intent_digest_hex,
        open_intent_token,
    },
};

fn empty(raw: &RawValue) -> Result<(), Failure> {
    match serde_json::from_str::<serde_json::Map<String, Value>>(raw.get()) {
        Ok(value) if value.is_empty() => Ok(()),
        _ => Err(invalid()),
    }
}

pub(super) struct JoinSession {
    api_origin: String,
    request_id: Uuid,
    join_secret: JoinKeySecret,
    join_public: [u8; 32],
    device_id: Uuid,
    enrollment_key: Option<EnrollmentKey>,
    claim: Option<Claim>,
    challenge: Option<Challenge>,
}

struct Claim {
    intent: Zeroizing<String>,
    key_digest: String,
    sas: String,
}
struct Challenge {
    token: Zeroizing<String>,
    vault_id: String,
    key_epoch: u32,
    fingerprint: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Start {
    api_origin: String,
    join_request_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Offer {
    sealed_intent_token: String,
    intent_digest: String,
    api_origin: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimInput {
    key_digest: String,
    sas: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChallengeInput {
    challenge_token: String,
    vault_id: String,
    key_epoch: u32,
    profile_fingerprint: String,
    requested_role: String,
}

impl BrowserCore {
    pub(super) fn join_command(&mut self, command: &str, raw: &RawValue) -> Result<Value, Failure> {
        match command {
            "_worker_join_start" => self.join_start(raw),
            "_worker_join_request" => {
                empty(raw)?;
                self.join_request()
            }
            "_worker_join_open_offer" => self.join_open_offer(raw),
            "_worker_join_claim" => self.join_claim(raw),
            "_worker_join_challenge" => self.join_challenge(raw),
            "_worker_join_confirm" => {
                empty(raw)?;
                self.join_confirm()
            }
            "_worker_join_cancel" => {
                empty(raw)?;
                self.join = None;
                Ok(json!({}))
            }
            _ => Err(unknown_command()),
        }
    }

    fn join_start(&mut self, raw: &RawValue) -> Result<Value, Failure> {
        if self.client.is_some() || self.identity_session.is_some() {
            return Err(Failure::new(
                "identity-exists",
                "A local identity already exists. Nothing was reset.",
            ));
        }
        let input: Start = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let api_origin = join_origin(&input.api_origin)?;
        let request_id = Uuid::parse_str(&input.join_request_id).map_err(|_| invalid())?;
        let (join_secret, join_public) = generate_join_key()
            .map_err(|_| Failure::new("join-key", "Could not create a join key."))?;
        let qr_payload = encode_join_request_qr(&JoinRequestQr {
            https_origin: api_origin.clone(),
            join_request_id: request_id,
            join_key: URL_SAFE_NO_PAD.encode(join_public),
        });
        self.join = Some(JoinSession {
            api_origin,
            request_id,
            join_secret,
            join_public,
            device_id: Uuid::new_v4(),
            enrollment_key: None,
            claim: None,
            challenge: None,
        });
        Ok(json!({"qrPayload": qr_payload}))
    }

    fn join_request(&self) -> Result<Value, Failure> {
        let join = self.join.as_ref().ok_or_else(unavailable)?;
        Ok(json!({"joinRequestId": join.request_id, "deviceId": join.device_id}))
    }

    fn join_open_offer(&mut self, raw: &RawValue) -> Result<Value, Failure> {
        let input: Offer = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let join = self.join.as_mut().ok_or_else(unavailable)?;
        if join_origin(&input.api_origin)? != join.api_origin {
            return Err(invalid());
        }
        let sealed = URL_SAFE_NO_PAD
            .decode(input.sealed_intent_token)
            .map_err(|_| invalid())?;
        let intent = open_intent_token(&join.join_secret, &join.join_public, &sealed)
            .map_err(|_| Failure::new("join-offer-invalid", "The join offer is invalid."))?;
        if intent_digest_hex(&intent) != input.intent_digest {
            return Err(Failure::new(
                "join-offer-invalid",
                "The join offer is invalid.",
            ));
        }
        let key = EnrollmentKey::generate()
            .map_err(|_| Failure::new("join-key", "Could not create an enrollment key."))?;
        let public = key
            .public_key_base64url()
            .map_err(|_| Failure::new("join-key", "Could not read the enrollment key."))?;
        join.enrollment_key = Some(key);
        join.claim = Some(Claim {
            intent: Zeroizing::new(intent.to_string()),
            key_digest: String::new(),
            sas: String::new(),
        });
        Ok(
            json!({"intent": intent.as_str(), "deviceId": join.device_id, "publicKey": {"ed25519_public_key": public}}),
        )
    }

    fn join_claim(&mut self, raw: &RawValue) -> Result<Value, Failure> {
        let input: ClaimInput = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let join = self.join.as_mut().ok_or_else(unavailable)?;
        let (Some(key), Some(claim)) = (join.enrollment_key.as_ref(), join.claim.as_mut()) else {
            return Err(Failure::new(
                "join-state",
                "The join request is not awaiting a claim.",
            ));
        };
        let sas = key
            .pairing_sas(
                &claim.intent,
                &join.device_id.to_string(),
                &input.key_digest,
            )
            .map_err(|_| Failure::new("join-claim-invalid", "The join claim is invalid."))?;
        if sas != input.sas {
            return Err(Failure::new(
                "join-sas-mismatch",
                "The join code did not match.",
            ));
        }
        claim.key_digest = input.key_digest;
        claim.sas = sas;
        Ok(json!({}))
    }

    fn join_challenge(&mut self, raw: &RawValue) -> Result<Value, Failure> {
        let input: ChallengeInput = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let join = self.join.as_mut().ok_or_else(unavailable)?;
        if join
            .claim
            .as_ref()
            .is_none_or(|claim| claim.key_digest.is_empty())
            || input.requested_role != "device"
            || input.key_epoch == 0
        {
            return Err(Failure::new(
                "join-role-invalid",
                "The approved role is invalid.",
            ));
        }
        Uuid::parse_str(&input.vault_id).map_err(|_| invalid())?;
        if input.profile_fingerprint.len() != 64
            || !input
                .profile_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        join.challenge = Some(Challenge {
            token: Zeroizing::new(input.challenge_token),
            vault_id: input.vault_id,
            key_epoch: input.key_epoch,
            fingerprint: input.profile_fingerprint,
        });
        Ok(json!({}))
    }

    fn join_confirm(&mut self) -> Result<Value, Failure> {
        let join = self.join.as_mut().ok_or_else(unavailable)?;
        let (Some(key), Some(claim), Some(challenge)) = (
            join.enrollment_key.as_ref(),
            join.claim.as_ref(),
            join.challenge.as_ref(),
        ) else {
            return Err(Failure::new(
                "join-state",
                "The join request is not awaiting confirmation.",
            ));
        };
        let proof = pairing_proof_bytes(
            &challenge.token,
            &challenge.vault_id,
            &join.device_id.to_string(),
            &challenge.fingerprint,
            challenge.key_epoch,
            "device",
        )
        .map_err(|_| Failure::new("join-proof", "Could not create the pairing proof."))?;
        let signature = key
            .sign_pairing_proof(&proof)
            .map_err(|_| Failure::new("join-proof", "Could not sign the pairing proof."))?;
        let public_key = key
            .public_key_base64url()
            .map_err(|_| Failure::new("join-key", "Could not read the enrollment key."))?;
        Ok(
            json!({"apiOrigin": join.api_origin, "intent": claim.intent.as_str(), "deviceId": join.device_id, "publicKey": {"ed25519_public_key": public_key}, "challengeToken": challenge.token.as_str(), "vaultId": challenge.vault_id, "keyEpoch": challenge.key_epoch, "profileFingerprint": challenge.fingerprint, "signature": signature}),
        )
    }
}

/// Pairing's public API origin permits only local HTTP development; stored browser identity remains
/// bound by the stricter Worker-origin validator in `local_identity`.
fn join_origin(value: &str) -> Result<String, Failure> {
    let url = Url::parse(value).map_err(|_| invalid())?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_hosted_client::join::seal_intent_token;

    const API_ORIGIN: &str = "https://api.example.test";

    fn dispatch(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        serde_json::from_str(&core.dispatch(&json!({"command": command, "args": args}).to_string()))
            .unwrap()
    }

    fn start(core: &mut BrowserCore) {
        let response = dispatch(
            core,
            "_worker_join_start",
            json!({"apiOrigin": API_ORIGIN, "joinRequestId": Uuid::new_v4()}),
        );
        assert_eq!(response["ok"], true, "{response}");
        let payload = response["value"]["qrPayload"].as_str().unwrap();
        let qr: JoinRequestQr = serde_json::from_str(payload).unwrap();
        assert_eq!(qr.https_origin, API_ORIGIN);
    }

    #[test]
    fn join_rejects_aliases_and_malformed_offers_before_claiming() {
        assert_eq!(
            join_origin("http://127.0.0.1:8080/").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert!(join_origin("http://api.example.test").is_err());
        let mut core = BrowserCore::new(tempfile::tempdir().unwrap().keep());
        start(&mut core);
        let response = dispatch(
            &mut core,
            "_worker_join_open_offer",
            json!({"apiOrigin": "https://other.example.test", "sealedIntentToken": "bad", "intentDigest": "bad"}),
        );
        assert_eq!(response["error"]["code"], "invalid-request");
        assert!(core.join.as_ref().unwrap().claim.is_none());
        let response = dispatch(
            &mut core,
            "_worker_join_open_offer",
            json!({"apiOrigin": API_ORIGIN, "sealedIntentToken": "bad", "intentDigest": "bad"}),
        );
        assert_eq!(response["error"]["code"], "invalid-request");
        assert!(core.join.as_ref().unwrap().claim.is_none());
    }

    #[test]
    fn join_sas_and_proof_are_pinned_to_the_device_role() {
        let mut core = BrowserCore::new(tempfile::tempdir().unwrap().keep());
        start(&mut core);
        let join = core.join.as_ref().unwrap();
        let intent = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let sealed = URL_SAFE_NO_PAD.encode(seal_intent_token(&join.join_public, &intent).unwrap());
        let response = dispatch(
            &mut core,
            "_worker_join_open_offer",
            json!({"apiOrigin": API_ORIGIN, "sealedIntentToken": sealed, "intentDigest": intent_digest_hex(&intent)}),
        );
        assert_eq!(response["ok"], true, "{response}");
        let join = core.join.as_ref().unwrap();
        let claim = join.claim.as_ref().unwrap();
        let public = join
            .enrollment_key
            .as_ref()
            .unwrap()
            .public_key_base64url()
            .unwrap();
        let key: [u8; 32] = URL_SAFE_NO_PAD.decode(public).unwrap().try_into().unwrap();
        let digest = peppy_protocol::pairing_key_digest(&key);
        let sas = join
            .enrollment_key
            .as_ref()
            .unwrap()
            .pairing_sas(&claim.intent, &join.device_id.to_string(), &digest)
            .unwrap();
        assert_eq!(
            dispatch(
                &mut core,
                "_worker_join_claim",
                json!({"keyDigest": digest, "sas": sas})
            )["ok"],
            true
        );
        let challenge = URL_SAFE_NO_PAD.encode([8_u8; 32]);
        let bad_role = dispatch(
            &mut core,
            "_worker_join_challenge",
            json!({"challengeToken": challenge, "vaultId": Uuid::new_v4(), "keyEpoch": 1, "profileFingerprint": "b".repeat(64), "requestedRole": "gateway"}),
        );
        assert_eq!(bad_role["error"]["code"], "join-role-invalid");
        assert_eq!(
            dispatch(
                &mut core,
                "_worker_join_challenge",
                json!({"challengeToken": challenge, "vaultId": Uuid::new_v4(), "keyEpoch": 1, "profileFingerprint": "b".repeat(64), "requestedRole": "device"})
            )["ok"],
            true
        );
        let proof = dispatch(&mut core, "_worker_join_confirm", json!({}));
        assert_eq!(proof["ok"], true, "{proof}");
        assert_eq!(proof["value"]["apiOrigin"], API_ORIGIN);
        assert_eq!(
            proof["value"]["deviceId"],
            core.join.as_ref().unwrap().device_id.to_string()
        );
        assert!(
            proof["value"]["signature"]
                .as_str()
                .is_some_and(|signature| !signature.is_empty())
        );
    }
}
