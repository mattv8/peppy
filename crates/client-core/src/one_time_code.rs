//! Finds one-time codes in received message and mirrored notification text.
//!
//! Precision beats recall: any ambiguity yields `None`, so no Copy action is
//! offered for text that merely contains digits.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OneTimeCode {
    pub code: String,
    pub source: OneTimeCodeSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneTimeCodeSource {
    /// WICG `sms-one-time-codes` origin-bound form: `@host #code`.
    OriginBound,
    /// Keyword-anchored heuristic match.
    Heuristic,
}

const MAX_INPUT_BYTES: usize = 2_048;
const MAX_HEURISTIC_CHARS: usize = 500;
const STRONG_KEYWORD_WINDOW_CHARS: usize = 30;
const LABEL_WINDOW_CHARS: usize = 15;

/// `(keyword, strong)`. Latin keywords match on word boundaries; other scripts match as substrings.
const KEYWORDS: &[(&str, bool)] = &[
    ("code", false),
    ("otp", true),
    ("passcode", true),
    ("passcodes", true),
    ("password", false),
    ("pin", false),
    ("verification", true),
    ("verify", true),
    ("one-time", true),
    ("one time", true),
    ("2fa", true),
    ("two-factor", true),
    ("security code", true),
    ("login code", true),
    ("auth", false),
    ("authentication", true),
    ("sign-in", true),
    ("código", false),
    ("clave", false),
    ("senha", false),
    ("vérification", false),
    ("bestätigungscode", false),
    ("sicherheitscode", false),
    ("verifizierungscode", false),
    ("codice", false),
    ("код", true),
    ("验证码", true),
    ("校验码", true),
    ("动态码", true),
    ("認証コード", true),
    ("確認コード", true),
    ("인증번호", true),
];

/// Words that mark a following number as an order, account or reference number.
const LABELS: &[&str] = &[
    "order",
    "tracking",
    "ref",
    "reference",
    "invoice",
    "account",
    "acct",
    "ending in",
    "card",
    "booking",
    "confirmation number",
    "ticket",
    "case",
    "member",
    "id",
];

/// ISO 4217 codes that, written right before a number, mark it as an amount.
const CURRENCY_CODES: &[&str] = &[
    "USD", "EUR", "GBP", "JPY", "CNY", "RMB", "INR", "AUD", "CAD", "CHF", "SEK", "NOK", "DKK",
    "NZD", "SGD", "HKD", "KRW", "BRL", "MXN", "ZAR", "RUB", "TRY", "PLN", "CZK", "HUF", "ILS",
    "AED", "SAR", "THB", "MYR", "IDR", "PHP", "VND", "TWD", "NGN", "EGP", "PKR", "BDT",
];

/// Detects a one-time code in `text`, returning it only when exactly one distinct candidate survives.
pub fn detect(text: &str) -> Option<OneTimeCode> {
    let text = bounded(text);
    origin_bound(text).or_else(|| heuristic(text))
}

/// Notification rule: the text first, then the title. The two are never joined,
/// because short-code sender titles would make otherwise unambiguous text ambiguous.
pub(crate) fn detect_notification(text: &str, title: &str) -> Option<OneTimeCode> {
    detect(text).or_else(|| detect(title))
}

fn bounded(text: &str) -> &str {
    if text.len() <= MAX_INPUT_BYTES {
        return text;
    }
    let mut end = MAX_INPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn origin_bound(text: &str) -> Option<OneTimeCode> {
    let line = text.lines().rev().find(|line| !line.trim().is_empty())?;
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let (origin, code_token, second_origin) = match tokens.as_slice() {
        [origin, code] => (*origin, *code, None),
        [origin, code, second] => (*origin, *code, Some(*second)),
        _ => return None,
    };
    if !is_origin_host(origin) || second_origin.is_some_and(|host| !is_origin_host(host)) {
        return None;
    }
    let code = code_token.strip_prefix('#')?;
    is_origin_code(code).then(|| OneTimeCode {
        code: code.to_owned(),
        source: OneTimeCodeSource::OriginBound,
    })
}

fn is_origin_host(token: &str) -> bool {
    token.strip_prefix('@').is_some_and(|host| {
        host.contains('.')
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
    })
}

fn is_origin_code(code: &str) -> bool {
    (4..=12).contains(&code.len()) && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

struct KeywordSpan {
    end: usize,
    strong: bool,
}

struct Candidate {
    start: usize,
    end: usize,
    value: String,
    alphanumeric: bool,
}

fn heuristic(text: &str) -> Option<OneTimeCode> {
    if text.trim().chars().count() > MAX_HEURISTIC_CHARS {
        return None;
    }
    // Candidates are cheap to find; the keyword scan only runs when there is something to anchor.
    let candidates = code_candidates(text);
    if candidates.is_empty() {
        return None;
    }
    let keywords = keyword_spans(text);
    if keywords.is_empty() {
        return None;
    }
    let phones = phone_spans(text);
    let urls = url_spans(text);
    let mut values: Vec<String> = Vec::new();
    for candidate in candidates {
        if overlaps_any(&candidate, &phones)
            || overlaps_any(&candidate, &urls)
            || rejected(text, &keywords, &candidate)
            || (candidate.alphanumeric && !strong_keyword_before(text, &keywords, candidate.start))
        {
            continue;
        }
        if !values.contains(&candidate.value) {
            values.push(candidate.value);
        }
    }
    match values.as_slice() {
        [value] => Some(OneTimeCode {
            code: value.clone(),
            source: OneTimeCodeSource::Heuristic,
        }),
        _ => None,
    }
}

fn keyword_spans(text: &str) -> Vec<KeywordSpan> {
    let mut spans = Vec::new();
    for (start, _) in text.char_indices() {
        for &(keyword, strong) in KEYWORDS {
            if let Some(end) = match_word(text, start, keyword) {
                spans.push(KeywordSpan { end, strong });
            }
        }
    }
    spans
}

/// Returns the end offset when `word` occurs at `start` (case-insensitive). ASCII words must sit on word boundaries.
fn match_word(text: &str, start: usize, word: &str) -> Option<usize> {
    let mut remaining = text[start..].chars();
    for expected in word.chars() {
        let actual = remaining.next()?;
        if !actual.to_lowercase().eq(expected.to_lowercase()) {
            return None;
        }
    }
    let end = text.len() - remaining.as_str().len();
    let bounded = !word.is_ascii()
        || (!text[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
            && !remaining.clone().next().is_some_and(char::is_alphanumeric));
    bounded.then_some(end)
}

fn contains_word(text: &str, word: &str) -> bool {
    text.char_indices()
        .any(|(start, _)| match_word(text, start, word).is_some())
}

fn strong_keyword_before(text: &str, keywords: &[KeywordSpan], start: usize) -> bool {
    keywords.iter().any(|keyword| {
        keyword.strong
            && keyword.end <= start
            && text[keyword.end..start].chars().count() <= STRONG_KEYWORD_WINDOW_CHARS
    })
}

fn code_candidates(text: &str) -> Vec<Candidate> {
    let bytes = text.as_bytes();
    let mut candidates = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
            index += 1;
        }
        let token = &text[start..index];
        if token.bytes().all(|byte| byte.is_ascii_digit()) {
            let (end, value) = grouped_digits(text, start, index);
            if (4..=10).contains(&value.len()) {
                candidates.push(Candidate {
                    start,
                    end,
                    value,
                    alphanumeric: false,
                });
            }
            index = end;
        } else if (4..=10).contains(&token.len())
            && token.bytes().any(|byte| byte.is_ascii_alphabetic())
            && token.bytes().any(|byte| byte.is_ascii_digit())
            && !token.bytes().any(|byte| byte.is_ascii_lowercase())
        {
            // Letters must all be uppercase: mixed-case and lowercase tokens are product names or words.
            candidates.push(Candidate {
                start,
                end: index,
                value: token.to_owned(),
                alphanumeric: true,
            });
        }
    }
    candidates
}

/// Joins digit groups separated by one space or hyphen (`123 456`, `12-34-56`, `1234-5678`).
///
/// Every group, the first included, is 2–5 digits. Space-separated groups must also match the first
/// group's width, so `482913 10 min` keeps `482913` instead of merging into `48291310`. A run that
/// is not 2–5 digits wide never groups; it stands alone.
fn grouped_digits(text: &str, start: usize, run_end: usize) -> (usize, String) {
    let bytes = text.as_bytes();
    let first_width = run_end - start;
    let mut end = run_end;
    let mut value = text[start..run_end].to_owned();
    if !(2..=5).contains(&first_width) {
        return (end, value);
    }
    while let Some(&separator @ (b' ' | b'-')) = bytes.get(end) {
        let group_start = end + 1;
        let group_end = group_start
            + bytes[group_start.min(bytes.len())..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
        let group_len = group_end - group_start;
        let same_width = separator == b'-' || group_len == first_width;
        let ends_token = bytes.get(group_end).is_some_and(u8::is_ascii_alphanumeric);
        if !(2..=5).contains(&group_len) || !same_width || ends_token {
            break;
        }
        value.push_str(&text[group_start..group_end]);
        end = group_end;
    }
    (end, value)
}

/// Byte ranges of phone-number shapes, so a candidate inside one is dropped whole.
fn phone_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() || (index > 0 && bytes[index - 1].is_ascii_digit()) {
            index += 1;
            continue;
        }
        let start = index;
        let mut groups = Vec::new();
        loop {
            let group_start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            groups.push(index - group_start);
            let continues = matches!(bytes.get(index), Some(b' ' | b'-' | b'.'))
                && bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
            if !continues {
                break;
            }
            index += 1;
        }
        let before = start.checked_sub(1).map(|previous| bytes[previous]);
        let shape_match = matches!(groups.as_slice(), [3, 3, 4] | [3, 4] | [1, 3, 3, 4]);
        let prefixed = before == Some(b'+') || (before == Some(b'(') && groups[0] == 3);
        if shape_match || prefixed {
            spans.push((start, index));
        }
    }
    spans
}

fn overlaps_any(candidate: &Candidate, spans: &[(usize, usize)]) -> bool {
    spans
        .iter()
        .any(|&(start, end)| candidate.start < end && start < candidate.end)
}

fn rejected(text: &str, keywords: &[KeywordSpan], candidate: &Candidate) -> bool {
    let before = &text[..candidate.start];
    let after = &text[candidate.end..];
    currency_or_percent(before, after)
        || date_time_or_version(before, after)
        || label_before(before)
        || (!candidate.alphanumeric && grouped_year(candidate))
        || (!candidate.alphanumeric
            && candidate.end - candidate.start == 4
            && is_year(&candidate.value)
            && !keyword_immediately_before(text, keywords, candidate.start))
}

/// A grouped run that starts with a year is an ISO-style date (`2024-05-06`), not a code.
fn grouped_year(candidate: &Candidate) -> bool {
    candidate.end - candidate.start > candidate.value.len() && is_year(&candidate.value[..4])
}

/// Byte ranges of URL and email spans. A span is a run of ASCII URL characters, so it stops at
/// whitespace and at any non-ASCII character (CJK punctuation does not swallow a trailing link).
fn url_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !is_url_byte(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_url_byte(bytes[index]) {
            index += 1;
        }
        if is_url_like(&text[start..index]) {
            spans.push((start, index));
        }
    }
    spans
}

fn is_url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&byte)
}

/// Scheme or `www.` URLs, email addresses (`x@y.z`), and schemeless `host.tld/path` links.
fn is_url_like(run: &str) -> bool {
    let email = run
        .split_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
    let host_path = run
        .split_once('/')
        .is_some_and(|(host, _)| host.contains('.'));
    run.contains("://") || run.contains("www.") || email || host_path
}

fn currency_or_percent(before: &str, after: &str) -> bool {
    if after.trim_start_matches(' ').starts_with('%') {
        return true;
    }
    let before = before.trim_end_matches(' ');
    if before.ends_with(['$', '€', '£', '¥', '₹']) || rupee_marker(before) {
        return true;
    }
    let letters_before = before.trim_end_matches(|character: char| character.is_ascii_uppercase());
    let iso_code = &before[letters_before.len()..];
    iso_code.len() == 3 && CURRENCY_CODES.contains(&iso_code)
}

/// `Rs` or `Rs.` (rupee prefix) as a standalone word right before the number.
fn rupee_marker(before: &str) -> bool {
    let before = before.strip_suffix('.').unwrap_or(before);
    before
        .strip_suffix("Rs")
        .is_some_and(|rest| !rest.ends_with(char::is_alphabetic))
}

/// Rejects a candidate that is directly joined to a digit by `:`, `/` or `.` (times, dates, versions, decimals).
fn date_time_or_version(before: &str, after: &str) -> bool {
    let preceded = before
        .strip_suffix([':', '/', '.'])
        .is_some_and(|rest| rest.ends_with(|character: char| character.is_ascii_digit()));
    let followed = after
        .strip_prefix([':', '/', '.'])
        .is_some_and(|rest| rest.starts_with(|character: char| character.is_ascii_digit()));
    preceded || followed
}

fn label_before(before: &str) -> bool {
    let window: Vec<char> = before.chars().rev().take(LABEL_WINDOW_CHARS).collect();
    let window: String = window.into_iter().rev().collect();
    LABELS.iter().any(|label| contains_word(&window, label))
}

fn is_year(value: &str) -> bool {
    value
        .parse::<u16>()
        .is_ok_and(|year| (1900..=2099).contains(&year))
}

/// True when a keyword ends right before `start`, allowing only an optional `:` or `is` between them.
fn keyword_immediately_before(text: &str, keywords: &[KeywordSpan], start: usize) -> bool {
    keywords.iter().any(|keyword| {
        keyword.end <= start
            && matches!(
                text[keyword.end..start]
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
                "" | ":" | "is" | "is:"
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_rule_prefers_text_then_title() {
        assert_eq!(
            detect_notification("Your code is 482913", "55512").map(|code| code.code),
            Some("482913".into())
        );
        assert_eq!(
            detect_notification("hello", "Code 739204").map(|code| code.code),
            Some("739204".into())
        );
        assert_eq!(
            detect_notification("Code 123456", "Code 654321").map(|code| code.code),
            Some("123456".into())
        );
    }

    #[test]
    fn notification_text_and_title_are_never_joined() {
        assert_eq!(
            detect_notification("Your verification code", "482913"),
            None
        );
    }
}
