//! Shared passphrase generation and conservative local acceptance checks.

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

/// Resamples six EFF large-list words until they pass the conservative local UI gate.
pub fn generate_passphrase() -> String {
    let _ = libsodium_rs::ensure_init();
    loop {
        let phrase = (0..6)
            .map(|_| word_at(random_word_index()))
            .collect::<Vec<_>>()
            .join(" ");
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
}
