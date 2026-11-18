//! Structural checks for message reactions. Pure functions: no storage, no clocks.

use crate::MessageReaction;
use std::collections::HashSet;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const EMOJI_BYTE_LIMIT: usize = 32;
pub(crate) const MAX_REACTIONS_PER_MESSAGE: usize = 32;
pub(crate) const MAX_ACTORS_PER_REACTION: usize = 100;

const ZERO_WIDTH_JOINER: char = '\u{200D}';
const VARIATION_TEXT: char = '\u{FE0E}';
const VARIATION_EMOJI: char = '\u{FE0F}';
const KEYCAP: char = '\u{20E3}';

/// The one emoji contract shared by capture, core, the Go link and the desktop UI. The complete
/// boundary is `packages/contracts/fixtures/reaction-emoji.json`; this function matches it exactly.
///
/// An accepted reaction is one grapheme of at most `EMOJI_BYTE_LIMIT` UTF-8 bytes that contains an
/// Extended_Pictographic character, or is a pair of regional indicators (a country flag). It has no
/// U+FE0E, no leading or trailing U+200D, no keycap (U+20E3), and no tag character.
pub(crate) fn valid_emoji(value: &str) -> bool {
    value.len() <= EMOJI_BYTE_LIMIT
        && value.graphemes(true).count() == 1
        && is_accepted_emoji(value)
}

/// Full message reaction set: bounded, one reaction per emoji, one reaction per person.
pub(crate) fn valid_message_reactions(reactions: &[MessageReaction]) -> bool {
    if reactions.len() > MAX_REACTIONS_PER_MESSAGE {
        return false;
    }
    let mut emojis = HashSet::new();
    let mut self_seen = false;
    let mut addresses = HashSet::new();
    for reaction in reactions {
        if !valid_emoji(&reaction.emoji)
            || !emojis.insert(reaction.emoji.as_str())
            || reaction.actors.is_empty()
            || reaction.actors.len() > MAX_ACTORS_PER_REACTION
        {
            return false;
        }
        for actor in &reaction.actors {
            if actor.is_self && std::mem::replace(&mut self_seen, true) {
                return false;
            }
            if let Some(address) = &actor.address
                && (address.is_empty() || !addresses.insert(address.as_str()))
            {
                return false;
            }
        }
    }
    true
}

fn is_accepted_emoji(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty()
        || chars.contains(&KEYCAP)
        || chars.contains(&VARIATION_TEXT)
        || chars.iter().any(|&c| is_tag(c))
        || chars.first() == Some(&ZERO_WIDTH_JOINER)
        || chars.last() == Some(&ZERO_WIDTH_JOINER)
    {
        return false;
    }
    // A country flag is exactly one pair of regional indicators.
    if chars.len() == 2 && chars.iter().all(|&c| is_regional_indicator(c)) {
        return true;
    }
    chars
        .split(|&c| c == ZERO_WIDTH_JOINER)
        .all(is_emoji_element)
}

/// One ZWJ-separated element: an Extended_Pictographic base, optionally a variation selector
/// (U+FE0F), optionally a skin tone modifier, and optionally U+FE0F again. Nothing else.
fn is_emoji_element(chars: &[char]) -> bool {
    let Some((&base, mut rest)) = chars.split_first() else {
        return false;
    };
    if !is_extended_pictographic(base) {
        return false;
    }
    if let [first, tail @ ..] = rest
        && *first == VARIATION_EMOJI
    {
        rest = tail;
    }
    if let [first, tail @ ..] = rest
        && is_skin_tone_modifier(*first)
    {
        rest = tail;
    }
    if let [first, tail @ ..] = rest
        && *first == VARIATION_EMOJI
    {
        rest = tail;
    }
    rest.is_empty()
}

fn is_tag(c: char) -> bool {
    ('\u{E0020}'..='\u{E007F}').contains(&c)
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

fn is_skin_tone_modifier(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}

/// Extended_Pictographic code point ranges (Unicode emoji-data.txt).
fn is_extended_pictographic(c: char) -> bool {
    matches!(
        u32::from(c),
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139
            | 0x2194..=0x2199 | 0x21A9..=0x21AA | 0x231A..=0x231B | 0x2328 | 0x23CF
            | 0x23E9..=0x23F3 | 0x23F8..=0x23FA | 0x24C2 | 0x25AA..=0x25AB | 0x25B6 | 0x25C0
            | 0x25FB..=0x25FE | 0x2600..=0x27BF | 0x2934..=0x2935 | 0x2B05..=0x2B07
            | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55 | 0x3030 | 0x303D | 0x3297 | 0x3299
            | 0x1F000..=0x1F0FF | 0x1F10D..=0x1F10F | 0x1F12F | 0x1F16C..=0x1F171
            | 0x1F17E..=0x1F17F | 0x1F18E | 0x1F191..=0x1F19A | 0x1F1AD..=0x1F1E5
            | 0x1F201..=0x1F20F | 0x1F21A | 0x1F22F | 0x1F232..=0x1F23A | 0x1F23C..=0x1F23F
            | 0x1F249..=0x1F3FA | 0x1F400..=0x1F53D | 0x1F546..=0x1F64F | 0x1F680..=0x1F6FF
            | 0x1F774..=0x1F77F | 0x1F7D5..=0x1F7FF | 0x1F80C..=0x1F80F | 0x1F848..=0x1F84F
            | 0x1F85A..=0x1F85F | 0x1F888..=0x1F88F | 0x1F8AE..=0x1F8FF | 0x1F90C..=0x1F93A
            | 0x1F93C..=0x1F945 | 0x1F947..=0x1FAFF | 0x1FC00..=0x1FFFD
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReactionActor;

    /// The complete emoji boundary every lane reads. Read at run time, so a missing file fails the
    /// test with its path instead of silently falling back to a copy.
    fn emoji_fixture() -> serde_json::Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packages/contracts/fixtures/reaction-emoji.json"
        );
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("shared emoji fixture missing at {path}: {error}"));
        serde_json::from_str(&text).expect("shared emoji fixture is JSON")
    }

    fn strings(fixture: &serde_json::Value, key: &str) -> Vec<String> {
        fixture[key]
            .as_array()
            .unwrap_or_else(|| panic!("fixture lacks {key}"))
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("fixture entries are strings")
                    .to_owned()
            })
            .collect()
    }

    fn actor(is_self: bool, address: Option<&str>) -> ReactionActor {
        ReactionActor {
            is_self,
            address: address.map(str::to_owned),
        }
    }

    fn reaction(emoji: &str, actors: Vec<ReactionActor>) -> MessageReaction {
        MessageReaction {
            emoji: emoji.to_owned(),
            actors,
        }
    }

    #[test]
    fn reaction_emoji_matches_the_complete_shared_contract_fixture() {
        let fixture = emoji_fixture();
        let accept = strings(&fixture, "accept");
        let reject = strings(&fixture, "reject");
        assert!(accept.len() >= 10 && reject.len() >= 11, "fixture coverage");
        for emoji in &accept {
            assert!(valid_emoji(emoji), "{emoji:?} must be accepted");
        }
        for emoji in &reject {
            assert!(!valid_emoji(emoji), "{emoji:?} must be refused");
        }
        assert!(
            accept.iter().any(|emoji| emoji.len() == EMOJI_BYTE_LIMIT),
            "the fixture pins an accepted emoji at the 32-byte limit"
        );
    }

    #[test]
    fn reaction_emoji_refuses_each_rule_on_its_own() {
        for refused in [
            "\u{1F1E8}",
            "\u{263A}\u{FE0E}",
            "a\u{FE0E}",
            "1\u{FE0F}\u{20E3}",
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}",
            "\u{E0067}",
            "\u{1F44D}\u{200D}",
            "\u{200D}\u{1F44D}",
            "a",
            "1",
            "hi",
            "\u{1F44D}\u{1F44D}",
            "\u{1F1EC}\u{1F1E7}\u{1F1EB}\u{1F1F7}\u{1F1E9}\u{1F1EA}",
            "\u{1F3FD}\u{1F3FD}",
            "",
        ] {
            assert!(!valid_emoji(refused), "{refused:?} must be refused");
        }
    }

    #[test]
    fn reaction_emoji_limits_are_utf8_bytes() {
        let fourteen = "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}";
        assert_eq!(fourteen.len(), 14);
        assert!(valid_emoji(fourteen));
        let forty_one = "\u{1F468}\u{1F3FD}\u{200D}\u{1F469}\u{1F3FD}\u{200D}\u{1F467}\u{1F3FD}\u{200D}\u{1F466}\u{1F3FD}";
        assert_eq!(forty_one.len(), 41);
        assert!(!valid_emoji(forty_one));
    }

    #[test]
    fn reaction_set_accepts_a_full_bounded_set() {
        let reactions: Vec<MessageReaction> = (0..MAX_REACTIONS_PER_MESSAGE)
            .map(|index| {
                let emoji = char::from_u32(0x1F300 + index as u32).unwrap().to_string();
                let actors = (0..MAX_ACTORS_PER_REACTION)
                    .map(|actor_index| actor(false, Some(&format!("+1555-{index}-{actor_index}"))))
                    .collect();
                reaction(&emoji, actors)
            })
            .collect();
        assert!(valid_message_reactions(&reactions));
    }

    #[test]
    fn reaction_set_refuses_too_many_reactions_actors_and_empty_actor_lists() {
        let too_many: Vec<MessageReaction> = (0..=MAX_REACTIONS_PER_MESSAGE)
            .map(|index| {
                reaction(
                    &char::from_u32(0x1F300 + index as u32).unwrap().to_string(),
                    vec![actor(false, None)],
                )
            })
            .collect();
        assert!(!valid_message_reactions(&too_many));

        let too_many_actors = vec![reaction(
            "👍",
            (0..=MAX_ACTORS_PER_REACTION)
                .map(|index| actor(false, Some(&format!("+1{index}"))))
                .collect(),
        )];
        assert!(!valid_message_reactions(&too_many_actors));

        assert!(!valid_message_reactions(&[reaction("👍", Vec::new())]));
    }

    #[test]
    fn reaction_set_refuses_duplicate_emoji_and_duplicate_people() {
        assert!(!valid_message_reactions(&[
            reaction("👍", vec![actor(false, None)]),
            reaction("👍", vec![actor(false, None)]),
        ]));
        assert!(!valid_message_reactions(&[reaction(
            "👍",
            vec![actor(false, Some("+1555")), actor(false, Some("+1555"))],
        )]));
        assert!(!valid_message_reactions(&[
            reaction("👍", vec![actor(true, None)]),
            reaction("❤️", vec![actor(true, None)]),
        ]));
        assert!(!valid_message_reactions(&[reaction(
            "👍",
            vec![actor(false, Some(""))],
        )]));
    }

    #[test]
    fn reaction_set_allows_unresolved_actors_and_one_self_per_message() {
        assert!(valid_message_reactions(&[
            reaction("👍", vec![actor(false, None), actor(false, None)]),
            reaction("😂", vec![actor(true, None), actor(false, Some("+1555"))]),
        ]));
        assert!(valid_message_reactions(&[]));
    }
}
