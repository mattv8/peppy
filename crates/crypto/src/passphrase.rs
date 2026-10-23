//! Shared passphrase generation and conservative local acceptance checks.

use std::{collections::BTreeSet, sync::OnceLock};

fn denylisted_words() -> &'static [&'static str] {
    static DENYLIST: OnceLock<Vec<&str>> = OnceLock::new();
    DENYLIST
        .get_or_init(|| {
            include_str!("../assets/passphrase_denylist.txt")
                .lines()
                .collect()
        })
        .as_slice()
}

fn denylist() -> &'static BTreeSet<&'static str> {
    static DENYLIST: OnceLock<BTreeSet<&str>> = OnceLock::new();
    DENYLIST.get_or_init(|| denylisted_words().iter().copied().collect())
}

fn word_at(index: usize) -> &'static str {
    include_str!("../assets/eff_large_wordlist.txt")
        .lines()
        .nth(index)
        .expect("maintained EFF word list")
        .split_once('\t')
        .expect("EFF word list format")
        .1
}

fn random_word_index() -> usize {
    let mut index = 0;
    for _ in 0..5 {
        let digit = loop {
            let mut byte = [0u8; 1];
            libsodium_rs::random::fill_bytes(&mut byte);
            if byte[0] < 252 {
                break (byte[0] % 6) as usize;
            }
        };
        index = index * 6 + digit;
    }
    index
}

fn random_word() -> &'static str {
    loop {
        let word = word_at(random_word_index());
        if !denylist().contains(word) {
            return word;
        }
    }
}

/// Generates six family-safe EFF large-list words.
///
/// The denylist excludes explicit anatomical-sexual, bodily-fluid, drug, crude,
/// insulting/slur-adjacent, and violence/gore terms. It leaves 7,716 of the 7,776
/// EFF words, or 12.914 bits per word; six words provide 77.482 bits of entropy.
/// Rejection sampling preserves a uniform distribution over those 7,716 words.
pub fn generate_passphrase() -> String {
    let _ = libsodium_rs::ensure_init();
    loop {
        let phrase = (0..6).map(|_| random_word()).collect::<Vec<_>>().join(" ");
        if passphrase_acceptable(&phrase) {
            return phrase;
        }
    }
}

pub fn passphrase_acceptable(passphrase: &str) -> bool {
    let words: Vec<_> = passphrase.split_whitespace().collect();
    if words.len() < 4 || passphrase.chars().count() < 24 {
        return false;
    }
    if passphrase
        .chars()
        .all(|character| character.is_ascii_digit() || character.is_whitespace())
    {
        return false;
    }
    let lowered: Vec<_> = words.iter().map(|word| word.to_ascii_lowercase()).collect();
    if lowered.iter().any(|word| {
        matches!(
            word.as_str(),
            "password" | "letmein" | "qwerty" | "correct" | "horse" | "battery" | "staple"
        )
    }) {
        return false;
    }
    lowered.windows(2).all(|pair| pair[0] != pair[1])
        && lowered
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == lowered.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_weak_and_accepts_distinct_six_words() {
        assert!(!passphrase_acceptable("correct horse battery staple"));
        assert!(!passphrase_acceptable(
            "alpha alpha charlie delta echo foxtrot"
        ));
        assert!(passphrase_acceptable(
            "alpha bravo charlie delta echo foxtrot"
        ));
    }

    #[test]
    fn denylist_is_sorted_unique_and_contains_eff_words() {
        let denylist = denylisted_words();
        let eff_words: std::collections::BTreeSet<_> =
            include_str!("../assets/eff_large_wordlist.txt")
                .lines()
                .map(|line| line.split_once('\t').expect("EFF word list format").1)
                .collect();

        assert!(denylist.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(denylist.iter().all(|word| eff_words.contains(word)));
    }

    #[test]
    fn generated_passphrases_exclude_denylisted_words() {
        let denylist: std::collections::BTreeSet<_> = denylisted_words().iter().copied().collect();

        for _ in 0..2_000 {
            let phrase = generate_passphrase();
            assert!(passphrase_acceptable(&phrase));
            assert_eq!(phrase.split_whitespace().count(), 6);
            assert!(
                phrase
                    .split_whitespace()
                    .all(|word| !denylist.contains(word))
            );
        }
    }

    #[test]
    fn acceptable_legacy_phrase_can_contain_a_denylisted_word() {
        assert!(passphrase_acceptable(
            "rectal bravo charlie delta echo foxtrot"
        ));
    }
}
