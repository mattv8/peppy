use peppy_crypto::*;
use std::io::Cursor;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use uuid::Uuid;
use zeroize::Zeroize;

fn profile() -> KeyProfile {
    KeyProfile {
        crypto_suite: CRYPTO_SUITE_1,
        salt: [7; 16],
        vault_id: Uuid::nil(),
        key_epoch: 9,
    }
}
fn root() -> RootKey {
    derive_root_key("correct horse battery staple", &profile()).unwrap()
}
fn key_hex(key: &PurposeKey) -> String {
    let mut output = String::with_capacity(64);
    key.with_native_cache_bytes(|bytes| {
        for byte in bytes {
            use std::fmt::Write as _;
            write!(&mut output, "{byte:02x}").unwrap();
        }
    });
    output
}

#[test]
fn public_profile_fingerprint_is_frozen() {
    assert_eq!(
        profile().fingerprint().unwrap(),
        "5ce360c831ed3dc1c6616e8da679df33bebfa29f98227adf707b2f2fa2abf134"
    );
}
#[test]
fn kdf_unicode_and_suite_rules() {
    let p = profile();
    let composed = derive_root_key("café café café café café", &p).unwrap();
    let decomposed = derive_root_key(
        "cafe\u{301} cafe\u{301} cafe\u{301} cafe\u{301} cafe\u{301}",
        &p,
    )
    .unwrap();
    let sealed = encrypt(
        &derive_purpose_key(&composed, &p, KeyPurpose::Event).unwrap(),
        b"normalization",
        b"same key",
    )
    .unwrap();
    assert_eq!(
        decrypt(
            &derive_purpose_key(&decomposed, &p, KeyPurpose::Event).unwrap(),
            b"normalization",
            &sealed,
        )
        .unwrap(),
        b"same key"
    );
    let vector_root = root();
    let vector_key = derive_purpose_key(&vector_root, &p, KeyPurpose::Command).unwrap();
    assert_eq!(
        key_hex(&vector_key),
        "a5296c20a8b8097e7fb9da00df580748c566f969637c4dd691db2a3e0320f09c"
    );
    let mut invalid = p.clone();
    invalid.crypto_suite = 2;
    assert!(matches!(
        derive_root_key("anything", &invalid),
        Err(CryptoError::UnsupportedSuite)
    ));
    assert!(matches!(
        derive_root_key(" passphrase", &p),
        Err(CryptoError::SurroundingWhitespace)
    ));
}
#[test]
fn transport_purpose_key_vectors_remain_frozen() {
    let p = profile();
    let root = root();
    let vectors = [
        key_hex(&derive_purpose_key(&root, &p, KeyPurpose::Command).unwrap()),
        key_hex(&derive_purpose_key(&root, &p, KeyPurpose::Event).unwrap()),
        key_hex(&derive_purpose_key(&root, &p, KeyPurpose::Header).unwrap()),
        key_hex(&derive_purpose_key(&root, &p, KeyPurpose::Compaction).unwrap()),
    ];

    assert_eq!(
        vectors,
        [
            "a5296c20a8b8097e7fb9da00df580748c566f969637c4dd691db2a3e0320f09c",
            "3a9f2745d8defcc6d6b9f507b55f32338c24e0cad00da634ba8409293e5451ee",
            "94695c7610c386074dafcb8fe624dca688d70777bb96dcb42f18a8e176e8ae18",
            "b027f36d8ae79b0906bb2ee115e07dada87ee79b5a302bc2fc9d5d022f4d168c",
        ]
    );
}
#[test]
fn key_handle_binds_purpose_and_profile() {
    let p = profile();
    let root = root();
    let command = derive_purpose_key(&root, &p, KeyPurpose::Command).unwrap();
    let sealed = encrypt(&command, b"canonical aad", b"hello").unwrap();
    assert_eq!(
        decrypt(&command, b"canonical aad", &sealed).unwrap(),
        b"hello"
    );
    assert!(decrypt(&command, b"changed aad", &sealed).is_err());
    let event = derive_purpose_key(&root, &p, KeyPurpose::Event).unwrap();
    assert!(decrypt(&event, b"canonical aad", &sealed).is_err());
    let mut changed = p.clone();
    changed.key_epoch += 1;
    let changed_key = derive_purpose_key(&root, &changed, KeyPurpose::Command).unwrap();
    assert!(decrypt(&changed_key, b"canonical aad", &sealed).is_err());
}
#[test]
fn local_wrap_key_round_trips_only_with_its_profile_purpose_and_aad() {
    let p = profile();
    let root = root();
    let local_wrap = derive_purpose_key(&root, &p, KeyPurpose::LocalWrap).unwrap();
    assert_eq!(
        key_hex(&local_wrap),
        "e00df9ba82306cb3ee8001176e30a692edd3a9901b382099144120d10b6b4f05"
    );
    let sealed = encrypt(&local_wrap, b"local credential envelope", b"database key").unwrap();

    assert_eq!(
        decrypt(&local_wrap, b"local credential envelope", &sealed).unwrap(),
        b"database key"
    );
    assert!(decrypt(&local_wrap, b"different envelope", &sealed).is_err());
    assert!(
        decrypt(
            &derive_purpose_key(&root, &p, KeyPurpose::Header).unwrap(),
            b"local credential envelope",
            &sealed,
        )
        .is_err()
    );

    let mut different_profile = p.clone();
    different_profile.key_epoch += 1;
    let different_profile_key =
        derive_purpose_key(&root, &different_profile, KeyPurpose::LocalWrap).unwrap();
    assert_eq!(key_hex(&different_profile_key), key_hex(&local_wrap));
    assert!(
        decrypt(
            &different_profile_key,
            b"local credential envelope",
            &sealed,
        )
        .is_err()
    );
}
#[test]
fn compaction_hmac_is_epoch_scoped_and_length_framed() {
    let p = profile();
    let root = root();
    let key = derive_purpose_key(&root, &p, KeyPurpose::Compaction).unwrap();
    assert_ne!(
        compaction_hmac(&key, b"notification", &[b"ab", b"c"]),
        compaction_hmac(&key, b"notification", &[b"a", b"bc"]),
    );
    let mut later = p.clone();
    later.key_epoch += 1;
    later.salt[0] ^= 1;
    let later_root = derive_root_key("correct horse battery staple", &later).unwrap();
    let later_key = derive_purpose_key(&later_root, &later, KeyPurpose::Compaction).unwrap();
    assert_ne!(
        compaction_hmac(&key, b"notification", &[b"a"]),
        compaction_hmac(&later_key, b"notification", &[b"a"]),
    );
}
#[test]
fn vault_check_rejects_wrong_key_profile_and_epoch() {
    let p = profile();
    let root = root();
    let header = create_vault_check_header(&root, p.clone()).unwrap();
    assert!(verify_vault_check_header(&root, &p, &header).is_ok());
    assert!(
        verify_vault_check_header(
            &derive_root_key("another passphrase", &p).unwrap(),
            &p,
            &header
        )
        .is_err()
    );
    let mut epoch = p.clone();
    epoch.key_epoch += 1;
    assert_eq!(
        verify_vault_check_header(&root, &epoch, &header),
        Err(CryptoError::InvalidProfile)
    );
}
#[test]
fn authenticated_stream_does_not_overwrite_existing_destination() {
    let key = FileKey::generate().unwrap();
    let mut wire = Vec::new();
    encrypt_stream(
        Cursor::new(b"authenticated plaintext"),
        &mut wire,
        &key,
        b"object",
    )
    .unwrap();
    let directory = std::env::temp_dir().join(format!("peppy-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let destination = directory.join("plaintext");
    std::fs::write(&destination, b"existing plaintext").unwrap();

    assert!(decrypt_stream_to_path(Cursor::new(wire), &destination, &key, b"object").is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"existing plaintext");
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);

    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn stream_rejects_tampering_without_promotion() {
    let key = FileKey::generate().unwrap();
    let mut wire = Vec::new();
    encrypt_stream(
        Cursor::new(vec![42; STREAM_CHUNK_BYTES + 5]),
        &mut wire,
        &key,
        b"object",
    )
    .unwrap();
    let dest = std::env::temp_dir().join(format!("peppy-{}", Uuid::new_v4()));
    decrypt_stream_to_path(Cursor::new(&wire), &dest, &key, b"object").unwrap();
    assert_eq!(std::fs::read(&dest).unwrap().len(), STREAM_CHUNK_BYTES + 5);
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::remove_file(&dest).unwrap();
    for mut bad in [
        wire[..wire.len() - 1].to_vec(),
        {
            let mut x = wire.clone();
            x.push(1);
            x
        },
        {
            let mut x = wire.clone();
            x[5] ^= 1;
            x
        },
    ] {
        assert!(decrypt_stream_to_path(Cursor::new(&bad), &dest, &key, b"object").is_err());
        assert!(!dest.exists());
        bad.zeroize();
    }
}
