//! Deterministic avatar identities shared by desktop surfaces.

use std::collections::BTreeSet;

pub const PALETTE_SIZE: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvatarKind {
    Person,
    Group,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationAvatar {
    pub seed: String,
    pub kind: AvatarKind,
}

/// Normalizes an address into a stable avatar seed key.
pub fn seed_key(address: &str) -> String {
    let trimmed = collapse_ascii_whitespace(address);
    if trimmed.is_empty() {
        return trimmed;
    }

    if is_phone_like(&trimmed) {
        let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return trimmed;
        }
        if digits.len() >= 7 {
            if trimmed.starts_with('+') {
                return format!("+{digits}");
            }
            if digits.len() == 10 {
                return format!("+1{digits}");
            }
            if digits.len() == 11 && digits.starts_with('1') {
                return format!("+{digits}");
            }
        }
        return digits;
    }

    trimmed.to_ascii_lowercase()
}

pub fn fnv1a32(value: &str) -> u32 {
    value.as_bytes().iter().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

pub fn palette_index(seed: &str) -> usize {
    (fnv1a32(seed) as usize) % PALETTE_SIZE
}

pub fn conversation_avatar(
    id: &str,
    name: &str,
    participants: Option<&[String]>,
    recipientless: bool,
) -> ConversationAvatar {
    if recipientless {
        return person_avatar(id);
    }

    let addresses: Vec<&str> = participants
        .filter(|participants| !participants.is_empty())
        .map(|participants| participants.iter().map(String::as_str).collect())
        .unwrap_or_else(|| name.split(", ").collect());
    let keys: BTreeSet<String> = addresses
        .into_iter()
        .map(seed_key)
        .filter(|key| !key.is_empty())
        .collect();

    match keys.len() {
        0 => person_avatar(id),
        1 => ConversationAvatar {
            seed: keys.into_iter().next().expect("one seed key"),
            kind: AvatarKind::Person,
        },
        _ => ConversationAvatar {
            seed: format!("group:{}", keys.into_iter().collect::<Vec<_>>().join(",")),
            kind: AvatarKind::Group,
        },
    }
}

pub fn contact_seed<'a>(phones: impl IntoIterator<Item = &'a str>, contact_id: &str) -> String {
    phones
        .into_iter()
        .map(seed_key)
        .find(|key| !key.is_empty())
        .unwrap_or_else(|| format!("contact:{contact_id}"))
}

pub fn initials(name: &str, max: usize) -> String {
    name.split_whitespace()
        .filter_map(|word| {
            word.chars()
                .next()
                .filter(|character| character.is_alphabetic())
        })
        .flat_map(char::to_uppercase)
        .take(max)
        .collect()
}

fn collapse_ascii_whitespace(value: &str) -> String {
    let mut result = String::new();
    let mut pending_space = false;

    for character in value.chars() {
        if matches!(character, ' ' | '\t' | '\n' | '\r') {
            pending_space = !result.is_empty();
        } else {
            if pending_space {
                result.push(' ');
                pending_space = false;
            }
            result.push(character);
        }
    }

    result
}

fn is_phone_like(value: &str) -> bool {
    let value = value.strip_prefix('+').unwrap_or(value);
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_digit() || matches!(character, ' ' | '(' | ')' | '.' | '-')
        })
}

fn person_avatar(id: &str) -> ConversationAvatar {
    ConversationAvatar {
        seed: format!("conversation:{id}"),
        kind: AvatarKind::Person,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_keys_hashes_and_palette_indexes_match_contract_vectors() {
        let vectors = [
            ("+18015551234", "+18015551234", Some(0x6bbd_46e1), 81),
            ("(801) 555-1234", "+18015551234", Some(0x6bbd_46e1), 81),
            ("801-555-1234", "+18015551234", None, 81),
            ("1 801 555 1234", "+18015551234", None, 81),
            ("  +1 (801) 555-1234 ", "+18015551234", None, 81),
            ("5556", "5556", Some(0xd76d_9558), 96),
            ("+15555550199", "+15555550199", Some(0xb855_db52), 90),
            ("+44 20 7946 0958", "+442079460958", Some(0x63b4_0aa8), 112),
            ("020 7946 0958", "02079460958", Some(0x3bec_62f1), 49),
            ("AMAZON", "amazon", Some(0x0e9c_0ded), 45),
            ("Amazon", "amazon", None, 45),
            (
                "alerts@bank.example",
                "alerts@bank.example",
                Some(0x1f64_b798),
                16,
            ),
            ("Ünïcode Name", "Ünïcode name", Some(0xd274_dcaa), 26),
        ];

        for (input, expected_key, expected_hash, expected_index) in vectors {
            let key = seed_key(input);
            assert_eq!(key, expected_key, "seed key for {input:?}");
            assert_eq!(palette_index(&key), expected_index, "index for {input:?}");
            if let Some(expected_hash) = expected_hash {
                assert_eq!(fnv1a32(&key), expected_hash, "hash for {input:?}");
            }
        }
        assert_eq!(seed_key(""), "");
        assert_eq!(fnv1a32(""), 0x811c_9dc5);
        assert_eq!(fnv1a32("a"), 0xe40c_292c);
    }

    #[test]
    fn conversation_avatars_match_contract_vectors() {
        let cases = [
            (
                "c1",
                "+18015551234",
                None,
                false,
                "+18015551234",
                AvatarKind::Person,
                81,
            ),
            (
                "c2",
                "(801) 555-1234, +15555550199",
                None,
                false,
                "group:+15555550199,+18015551234",
                AvatarKind::Group,
                67,
            ),
            (
                "c4",
                "New message",
                None,
                true,
                "conversation:c4",
                AvatarKind::Person,
                115,
            ),
            (
                "c5",
                "",
                Some(vec![]),
                false,
                "conversation:c5",
                AvatarKind::Person,
                56,
            ),
        ];

        for (id, name, participants, recipientless, seed, kind, index) in cases {
            let avatar = conversation_avatar(id, name, participants.as_deref(), recipientless);
            assert_eq!(
                avatar,
                ConversationAvatar {
                    seed: seed.into(),
                    kind
                }
            );
            assert_eq!(palette_index(&avatar.seed), index);
        }

        let c3_participants = vec!["+15555550199".into(), "+18015551234".into()];
        let c3 = conversation_avatar("c3", "+15555550199", Some(&c3_participants), false);
        assert_eq!(c3.seed, "group:+15555550199,+18015551234");
        assert_eq!(c3.kind, AvatarKind::Group);
        assert_eq!(palette_index(&c3.seed), 67);

        let c6_participants = vec!["😀".into(), "豈".into()];
        let c6 = conversation_avatar("c6", "x", Some(&c6_participants), false);
        assert_eq!(c6.seed, "group:豈,😀");
        assert_eq!(c6.kind, AvatarKind::Group);
        assert_eq!(palette_index(&c6.seed), 84);
    }

    #[test]
    fn contact_seed_uses_first_non_empty_phone_key_or_contact_id() {
        assert_eq!(
            contact_seed(["", " (801) 555-1234 "], "contact-1"),
            "+18015551234"
        );
        assert_eq!(contact_seed(["", " \t"], "contact-1"), "contact:contact-1");
    }

    #[test]
    fn initials_match_contract_vectors() {
        let vectors = [
            ("Ada Lovelace", "AL"),
            ("Ada\u{a0}Lovelace", "AL"),
            ("ß x", "SS"),
            ("+18016944617", ""),
            ("  7 Bob  Élodie", "BÉ"),
            ("李 小龍", "李小"),
            ("straße", "S"),
            ("Ada", "A"),
        ];

        for (name, expected) in vectors {
            assert_eq!(initials(name, 2), expected, "initials for {name:?}");
        }
        assert_eq!(initials("Ada Lovelace", 1), "A");
    }
}
