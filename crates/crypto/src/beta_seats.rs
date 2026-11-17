use crate::{CryptoError, KeyPurpose, PurposeKey, ensure_sodium};
use libsodium_rs::crypto_box::{self, KeyPair, PublicKey};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

pub const MAX_EMAIL_BYTES: usize = 320;
const PREFIX: &[u8] = b"peppy-beta-seat-v1\0";
const CONTEXT_BYTES: usize = 32;
const MIN_SEALED_BYTES: usize = crypto_box::SEALBYTES + PREFIX.len() + CONTEXT_BYTES + 1;
const MAX_SEALED_BYTES: usize =
    crypto_box::SEALBYTES + PREFIX.len() + CONTEXT_BYTES + MAX_EMAIL_BYTES;

pub struct BetaSeatKeypair(KeyPair);

impl BetaSeatKeypair {
    pub fn from_purpose_key(key: &PurposeKey) -> Result<Self, CryptoError> {
        ensure_sodium()?;
        if key.purpose() != KeyPurpose::BetaSeats {
            return Err(CryptoError::InvalidProfile);
        }
        KeyPair::from_seed(key.as_bytes())
            .map(Self)
            .map_err(|_| CryptoError::OperationFailed)
    }

    pub fn public_key(&self) -> [u8; 32] {
        *self.0.public_key.as_bytes()
    }
}

impl Drop for BetaSeatKeypair {
    fn drop(&mut self) {
        self.0.secret_key.zeroize();
    }
}

pub struct BetaSeatContext {
    pub program_id: Uuid,
    pub seat_id: Uuid,
}

pub fn seal_email(
    public_key: &[u8; 32],
    context: &BetaSeatContext,
    email: &str,
) -> Result<Vec<u8>, CryptoError> {
    ensure_sodium()?;
    let email = email.as_bytes();
    if email.is_empty() || email.len() > MAX_EMAIL_BYTES {
        return Err(CryptoError::InvalidCiphertext);
    }
    let public_key = PublicKey::from_bytes(public_key).map_err(|_| CryptoError::OperationFailed)?;
    let mut plaintext = Zeroizing::new(Vec::with_capacity(
        PREFIX.len() + CONTEXT_BYTES + email.len(),
    ));
    plaintext.extend_from_slice(PREFIX);
    plaintext.extend_from_slice(context.program_id.as_bytes());
    plaintext.extend_from_slice(context.seat_id.as_bytes());
    plaintext.extend_from_slice(email);
    crypto_box::seal_box(&plaintext, &public_key).map_err(|_| CryptoError::OperationFailed)
}

pub fn open_email(
    keypair: &BetaSeatKeypair,
    context: &BetaSeatContext,
    sealed: &[u8],
) -> Result<Zeroizing<String>, CryptoError> {
    ensure_sodium()?;
    if !(MIN_SEALED_BYTES..=MAX_SEALED_BYTES).contains(&sealed.len()) {
        return Err(CryptoError::AuthenticationFailed);
    }
    let plaintext = Zeroizing::new(
        crypto_box::open_sealed_box(sealed, &keypair.0.public_key, &keypair.0.secret_key)
            .map_err(|_| CryptoError::AuthenticationFailed)?,
    );
    let context_start = PREFIX.len();
    let email_start = context_start + CONTEXT_BYTES;
    if plaintext[..context_start] != *PREFIX
        || plaintext[context_start..context_start + 16] != *context.program_id.as_bytes()
        || plaintext[context_start + 16..email_start] != *context.seat_id.as_bytes()
    {
        return Err(CryptoError::AuthenticationFailed);
    }
    let email = std::str::from_utf8(&plaintext[email_start..])
        .map_err(|_| CryptoError::AuthenticationFailed)?;
    Ok(Zeroizing::new(email.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KeyProfile, derive_purpose_key, derive_root_key};

    fn keypair() -> (BetaSeatKeypair, PurposeKey) {
        let profile = KeyProfile::new(Uuid::from_u128(1), 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let purpose_key = derive_purpose_key(&root, &profile, KeyPurpose::BetaSeats).unwrap();
        let keypair = BetaSeatKeypair::from_purpose_key(&purpose_key).unwrap();
        (keypair, purpose_key)
    }

    fn context() -> BetaSeatContext {
        BetaSeatContext {
            program_id: Uuid::from_u128(2),
            seat_id: Uuid::from_u128(3),
        }
    }

    #[test]
    fn seals_and_opens_an_email() {
        let (keypair, _) = keypair();
        let context = context();
        let sealed = seal_email(&keypair.public_key(), &context, "person@example.com").unwrap();
        assert_eq!(
            open_email(&keypair, &context, &sealed).unwrap().as_str(),
            "person@example.com"
        );
    }

    #[test]
    fn rejects_wrong_context_or_key() {
        let (first_keypair, _) = keypair();
        let context = context();
        let sealed =
            seal_email(&first_keypair.public_key(), &context, "person@example.com").unwrap();
        assert_eq!(
            open_email(
                &first_keypair,
                &BetaSeatContext {
                    program_id: Uuid::from_u128(4),
                    seat_id: context.seat_id,
                },
                &sealed,
            ),
            Err(CryptoError::AuthenticationFailed)
        );
        let (wrong_keypair, _) = keypair();
        assert_eq!(
            open_email(&wrong_keypair, &context, &sealed),
            Err(CryptoError::AuthenticationFailed)
        );
    }

    #[test]
    fn derives_a_stable_public_key() {
        let (_, purpose_key) = keypair();
        let first = BetaSeatKeypair::from_purpose_key(&purpose_key).unwrap();
        let second = BetaSeatKeypair::from_purpose_key(&purpose_key).unwrap();
        assert_eq!(first.public_key(), second.public_key());
    }

    #[test]
    fn rejects_a_wrong_purpose_and_email_size_bounds() {
        let profile = KeyProfile::new(Uuid::from_u128(1), 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let wrong_purpose =
            derive_purpose_key(&root, &profile, KeyPurpose::TesterRegistry).unwrap();
        assert!(matches!(
            BetaSeatKeypair::from_purpose_key(&wrong_purpose),
            Err(CryptoError::InvalidProfile)
        ));

        let (keypair, _) = keypair();
        let context = context();
        assert!(seal_email(&keypair.public_key(), &context, "").is_err());
        assert!(
            seal_email(
                &keypair.public_key(),
                &context,
                &"a".repeat(MAX_EMAIL_BYTES + 1),
            )
            .is_err()
        );
    }
}
