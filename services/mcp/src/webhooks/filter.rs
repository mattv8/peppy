/// A webhook's sender-filter mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    All,
    Only,
    Except,
}

/// Borrowed identity information for one configured filter entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterEntryRef<'a> {
    pub address: &'a str,
    pub contact: Option<(&'a str, &'a str)>,
}

/// Returns whether a sender is included by the configured filter.
pub fn matches(
    mode: FilterMode,
    entries: &[FilterEntryRef<'_>],
    sender: &str,
    resolved_contact: Option<(&str, &str)>,
) -> bool {
    let entry_matches = |entry: &FilterEntryRef<'_>| {
        addresses_match(entry.address, sender)
            || (entry.contact.is_some() && entry.contact == resolved_contact)
    };
    match mode {
        FilterMode::All => true,
        FilterMode::Only => entries.iter().any(entry_matches),
        FilterMode::Except => !entries.iter().any(entry_matches),
    }
}

/// Phone-like addresses contain only digits and `+ ( ) - . space`, with an optional
/// leading `+`. Anything else (for example `BANK1`) is an alphanumeric sender ID.
fn phone_digits(value: &str) -> Option<String> {
    let value = value.trim();
    let value_without_plus = value.strip_prefix('+').unwrap_or(value);
    let allowed = value_without_plus
        .chars()
        .all(|c| c.is_ascii_digit() || "()-. ".contains(c));
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    (allowed && !digits.is_empty()).then_some(digits)
}

fn addresses_match(left: &str, right: &str) -> bool {
    match (phone_digits(left), phone_digits(right)) {
        (Some(left_digits), Some(right_digits)) => {
            left_digits == right_digits
                || (!(left.trim().starts_with('+') && right.trim().starts_with('+'))
                    && (is_explicit_plus_one(left, &left_digits, &right_digits)
                        || is_explicit_plus_one(right, &right_digits, &left_digits)))
        }
        // Alphanumeric IDs compare in full, trimmed and case-insensitive. A
        // phone-like value never matches an alphanumeric ID.
        (None, None) => left.trim().eq_ignore_ascii_case(right.trim()),
        _ => false,
    }
}

fn is_explicit_plus_one(value: &str, international: &str, local: &str) -> bool {
    value.trim().starts_with('+')
        && international.len() == 11
        && international.starts_with('1')
        && local.len() == 10
        && &international[1..] == local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(address: &str) -> FilterEntryRef<'_> {
        FilterEntryRef {
            address,
            contact: None,
        }
    }

    #[test]
    fn matches_phone_numbers_and_short_codes() {
        assert!(matches(
            FilterMode::Only,
            &[entry("+1 (555) 010-2233")],
            "5550102233",
            None
        ));
        assert!(matches(FilterMode::Only, &[entry("40404")], "40404", None));
        assert!(!matches(
            FilterMode::Only,
            &[entry("40404")],
            "040404",
            None
        ));
    }

    #[test]
    fn explicit_international_numbers_do_not_match_by_ten_digit_suffix() {
        let entries = [entry("+447700900123")];
        assert!(!matches(FilterMode::Only, &entries, "+917700900123", None));
        assert!(matches(FilterMode::Except, &entries, "+917700900123", None));
        assert!(matches(FilterMode::Only, &entries, "+447700900123", None));
    }

    #[test]
    fn explicit_international_and_ten_digit_country_codes_do_not_use_local_fallback() {
        for (configured, sender) in [
            ("+12425550123", "+2425550123"),
            ("+2425550123", "+12425550123"),
        ] {
            let entries = [entry(configured)];
            assert!(!matches(FilterMode::Only, &entries, sender, None));
            assert!(matches(FilterMode::Except, &entries, sender, None));
        }
        assert!(matches(
            FilterMode::Only,
            &[entry("+12425550123")],
            "2425550123",
            None
        ));
    }

    #[test]
    fn matches_one_and_two_digit_phone_like_values_after_trimming() {
        assert!(matches(FilterMode::Only, &[entry(" 1 ")], "1", None));
        assert!(matches(FilterMode::Only, &[entry(" +12 ")], "12", None));
        assert!(!matches(FilterMode::Except, &[entry(" 1 ")], "1", None));
        assert!(!matches(FilterMode::Except, &[entry(" +12 ")], "12", None));
    }

    #[test]
    fn matches_alphanumeric_senders_without_case_or_whitespace() {
        assert!(matches(
            FilterMode::Only,
            &[entry(" AMAZON ")],
            "amazon",
            None
        ));
        assert!(!matches(
            FilterMode::Only,
            &[entry("AMAZON")],
            "AMAZ0N",
            None
        ));
    }

    #[test]
    fn alphanumeric_ids_never_match_each_other_or_phone_numbers() {
        // Regression: alphanumeric IDs must compare in full, not by digits.
        assert!(!matches(FilterMode::Only, &[entry("BANK1")], "SHOP1", None));
        assert!(!matches(FilterMode::Only, &[entry("BANK1")], "1", None));
        assert!(!matches(FilterMode::Only, &[entry("1")], "BANK1", None));
        assert!(matches(FilterMode::Only, &[entry("bank1")], "BANK1", None));
        assert!(matches(FilterMode::Only, &[entry("40404")], "40404", None));
        assert!(!matches(FilterMode::Only, &[entry("40404")], "4O4O4", None));
        assert!(matches(
            FilterMode::Only,
            &[entry("+1 (555) 010-2233")],
            "5550102233",
            None
        ));
    }

    #[test]
    fn contact_identity_matches_even_when_number_changed() {
        let entries = [FilterEntryRef {
            address: "+15550101",
            contact: Some(("book", "contact")),
        }];
        assert!(matches(
            FilterMode::Only,
            &entries,
            "+15550202",
            Some(("book", "contact"))
        ));
        assert!(!matches(
            FilterMode::Only,
            &entries,
            "+15550202",
            Some(("book", "other"))
        ));
    }

    #[test]
    fn modes_and_empty_sender_follow_inclusion_rules() {
        assert!(matches(FilterMode::All, &[], "", None));
        assert!(!matches(FilterMode::Only, &[], "", None));
        assert!(matches(FilterMode::Except, &[], "", None));
        assert!(!matches(FilterMode::Except, &[entry("")], "", None));
    }
}
