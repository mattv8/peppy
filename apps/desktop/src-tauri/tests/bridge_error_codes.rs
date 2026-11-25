use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const TEST_ONLY_CODES: &[&str] = &[
    // mcp::tests::admitted_command_blocks_restart_across_an_await
    "fake-transport",
    // secure_store::tests::bundled_store_preserves_recognized_inner_categories_and_redacts_unknown_errors
    "raw-store-error",
];

fn rust_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn literal_after<'a>(source: &'a str, marker: &str) -> &'a str {
    let remainder = source
        .split_once(marker)
        .expect("missing mapping")
        .1
        .trim_start();
    let remainder = remainder.strip_prefix('"').expect("expected literal");
    remainder.split_once('"').expect("unterminated literal").0
}

// Mask literal and comment contents, preserving byte offsets and delimiters in code.
fn code_mask(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"//") {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while depth > 0 {
                assert!(i < bytes.len(), "unterminated block comment");
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'r'
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'b')
        {
            let mut quote = i + 1;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if bytes.get(quote) != Some(&b'"') {
                i += 1;
                continue;
            }
            let hashes = quote - i - 1;
            i = quote + 1;
            loop {
                assert!(i < bytes.len(), "unterminated raw string");
                if bytes[i] == b'"'
                    && bytes
                        .get(i + 1..i + 1 + hashes)
                        .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
                {
                    i += 1 + hashes;
                    break;
                }
                i += 1;
            }
        } else if bytes[i] == b'"' {
            i += 1;
            loop {
                assert!(i < bytes.len(), "unterminated string");
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'\'' {
            // A char has one scalar or one escape followed by a closing apostrophe.
            let tail = &source[i + 1..];
            let end = if tail.starts_with('\\') {
                if tail.starts_with("\\u{") {
                    tail.find('}').map(|n| n + 1)
                } else if tail.starts_with("\\x") {
                    Some(4)
                } else {
                    Some(2)
                }
            } else {
                tail.chars().next().map(char::len_utf8)
            };
            if let Some(end) = end.filter(|end| tail.as_bytes().get(*end) == Some(&b'\'')) {
                i += end + 2;
            } else {
                i += 1; // lifetime or label
                continue;
            }
        } else {
            i += 1;
            continue;
        }
        for byte in &mut output[start..i] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(output).unwrap()
}

fn test_ranges(source: &str) -> Vec<std::ops::Range<usize>> {
    let mask = code_mask(source);
    let bytes = mask.as_bytes();
    let mut ranges = Vec::new();
    for (start, marker) in mask.match_indices("#[cfg(test)]") {
        let mut i = start + marker.len();
        let statement = mask[i..].trim_start().starts_with("let ");
        let mut braces = 0usize;
        let mut parens = 0usize;
        let mut brackets = 0usize;
        let mut has_body = false;
        loop {
            assert!(
                i < bytes.len(),
                "unbalanced #[cfg(test)] item at byte {start}"
            );
            match bytes[i] {
                b'(' => parens += 1,
                b')' => parens = parens.checked_sub(1).expect("unbalanced parentheses"),
                b'[' => brackets += 1,
                b']' => brackets = brackets.checked_sub(1).expect("unbalanced brackets"),
                b'{' if parens == 0 && brackets == 0 => {
                    braces += 1;
                    has_body = true;
                }
                b'}' if parens == 0 && brackets == 0 => {
                    braces = braces.checked_sub(1).expect("unbalanced test item");
                    if braces == 0 && has_body {
                        i += 1;
                        let rest = mask[i..].trim_start();
                        if rest.starts_with("else") || statement {
                            continue;
                        }
                        break;
                    }
                }
                b';' | b',' if braces == 0 && parens == 0 && brackets == 0 => {
                    i += 1;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        ranges.push(start..i);
    }
    ranges
}

#[test]
fn test_ranges_skip_only_gated_items_despite_literals_and_lifetimes() {
    let source = r###"
#[cfg(test)]
mod tests {
    fn sample<'a>(x: &'a str) {
        let _ = "} \" {";
        let _ = r#"} { " quoted"#;
        let _ = r"} {";
        let _ = br#" } " { "#;
        let _ = '}';
        let _ = '\u{7d}';
        let _ = '\'';
        /* } /* { */ } */
        // }
    }
}
fn production() { BridgeError::new(code, "message"); }
#[cfg(test)]
fn helper() { let _ = '}'; }
#[cfg(test)]
let _ = value;
#[cfg(test)]
let _ = match value { _ => 1 };
#[cfg(test)]
if flag { one(); } else { two(); }
fn later() {}
"###;
    let ranges = test_ranges(source);
    for needle in ["fn sample", "fn helper", "let _ = value"] {
        let offset = source.find(needle).unwrap();
        assert!(
            ranges.iter().any(|range| range.contains(&offset)),
            "{needle}"
        );
    }
    for needle in ["fn production", "fn later"] {
        let offset = source.find(needle).unwrap();
        assert!(
            !ranges.iter().any(|range| range.contains(&offset)),
            "{needle}"
        );
    }
    let otherwise = source.find("two()").unwrap();
    assert!(ranges.iter().any(|range| range.contains(&otherwise)));
}

#[test]
#[should_panic(expected = "unbalanced #[cfg(test)] item")]
fn unbalanced_test_item_fails_loudly() {
    test_ranges("#[cfg(test)] mod tests { fn broken() { }");
}

#[test]
#[should_panic(expected = "test-only code fake-transport occurs in production")]
fn test_only_code_after_test_module_is_rejected() {
    check_test_only_occurrences(
        Path::new("production.rs"),
        r#"#[cfg(test)] mod tests { fn probe() {} }
           fn production() { BridgeError::new("fake-transport", "error"); }"#,
    );
}

fn check_test_only_occurrences(path: &Path, source: &str) {
    let name = path.file_name().unwrap().to_str().unwrap();
    let test_file = name == "tests.rs" || name.ends_with("_tests.rs");
    let ranges = test_ranges(source);
    for code in TEST_ONLY_CODES {
        let literal = format!("\"{code}\"");
        for (offset, _) in source.match_indices(&literal) {
            assert!(
                test_file || ranges.iter().any(|range| range.contains(&offset)),
                "test-only code {code} occurs in production: {}",
                path.display()
            );
        }
    }
}

fn without_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            let mut quoted = false;
            let mut escaped = false;
            let bytes = line.as_bytes();
            for (index, byte) in bytes.iter().copied().enumerate() {
                if quoted {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        quoted = false;
                    }
                } else if byte == b'"' {
                    quoted = true;
                } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
                    return &line[..index];
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn bridge_error_codes_match_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let src = root.join("apps/desktop/src-tauri/src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    let mut codes = BTreeSet::new();
    let mut nonliteral = BTreeMap::new();
    for path in files {
        let source = fs::read_to_string(&path).unwrap();
        let relative = path.strip_prefix(&src).unwrap().to_str().unwrap();
        check_test_only_occurrences(&path, &source);
        let ranges = test_ranges(&source);
        let test_file = path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("_tests.rs")
            || path.file_name().unwrap() == "tests.rs";
        for (remainder, _) in source.match_indices("BridgeError::new") {
            let remainder = source[remainder..]
                .strip_prefix("BridgeError::new")
                .unwrap()
                .trim_start();
            let Some(remainder) = remainder.strip_prefix('(') else {
                continue;
            };
            let argument = remainder.split_once(',').unwrap().0.trim();
            if let Some(code) = argument.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                codes.insert(code.to_owned());
            } else if !test_file
                && !ranges
                    .iter()
                    .any(|range| range.contains(&(source.len() - remainder.len())))
            {
                *nonliteral
                    .entry((relative.to_owned(), argument.to_owned()))
                    .or_insert(0) += 1;
            }
        }
    }
    let expected: BTreeMap<(String, String), usize> = [
        // ContactError::Ui forwards the shared contact API's static codes.
        ("contacts.rs", "code", 1),
        // ComposeError::Ui forwards the shared compose API's static codes.
        ("session.rs", "code", 1),
        // Production snapshot refusal uses this constant.
        ("sync.rs", "SNAPSHOT_TOO_LARGE_CODE", 1),
        // The webhook mapper forwards an entry from WEBHOOK_ERRORS.
        ("mcp.rs", "known", 1),
        // The claim mapper uses its bounded known-code mapping or fallback.
        (
            "enrollment.rs",
            "error_code.unwrap_or(\"server-rejected\")",
            1,
        ),
    ]
    .into_iter()
    .map(|(file, arg, count)| ((file.to_owned(), arg.to_owned()), count))
    .collect();
    assert_eq!(nonliteral, expected, "review every nonliteral call site");

    let error = fs::read_to_string(src.join("error.rs")).unwrap();
    for method in ["pub fn host_state()", "pub fn no_session()"] {
        let body = error.split_once(method).unwrap().1;
        codes.insert(literal_after(body, "Self::new(").to_owned());
    }
    let sync = fs::read_to_string(src.join("sync.rs")).unwrap();
    codes.insert(literal_after(&sync, "const SNAPSHOT_TOO_LARGE_CODE: &str =").to_owned());
    let enrollment = fs::read_to_string(src.join("enrollment.rs")).unwrap();
    let mapping = enrollment
        .split_once("fn known_claim_error_code(")
        .unwrap()
        .1
        .split_once("\n}\n")
        .unwrap()
        .0;
    for (tail, _) in mapping.match_indices("=> Some(") {
        codes.insert(literal_after(&mapping[tail..], "=> Some(").to_owned());
    }
    codes.insert("server-rejected".to_owned());
    let mcp = fs::read_to_string(src.join("mcp.rs")).unwrap();
    let mapping = mcp
        .split_once("const WEBHOOK_ERRORS:")
        .unwrap()
        .1
        .split_once("];")
        .unwrap()
        .0;
    for (tail, _) in mapping.match_indices('(') {
        let rest = mapping[tail + 1..].trim_start();
        if let Some(rest) = rest.strip_prefix('"') {
            codes.insert(rest.split_once('"').unwrap().0.to_owned());
        }
    }
    // These are the shared UI-error producers forwarded by the Tauri host.
    for file in [
        "compose.rs",
        "quick_reply.rs",
        "contacts.rs",
        "contact_error.rs",
    ] {
        let source = fs::read_to_string(root.join("crates/desktop-api/src").join(file)).unwrap();
        check_test_only_occurrences(Path::new(file), &source);
        let uncommented = without_line_comments(&source);
        let ranges = test_ranges(&uncommented);
        let mut dynamic = BTreeMap::new();
        for (tail, _) in uncommented.match_indices("ui(") {
            let before = &uncommented[..tail];
            if before
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                || before.trim_end().ends_with("fn")
            {
                continue;
            }
            let rest = uncommented[tail + 3..].trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                codes.insert(rest.split_once('"').unwrap().0.to_owned());
            } else if !ranges.iter().any(|range| range.contains(&tail)) {
                let argument = rest.split_once(',').unwrap().0.trim();
                *dynamic.entry(argument.to_owned()).or_insert(0) += 1;
            }
        }
        let expected: BTreeMap<String, usize> = if file == "compose.rs" {
            // gateway_sendable selects the code from its availability match.
            [("code".to_owned(), 1)].into_iter().collect()
        } else {
            BTreeMap::new()
        };
        assert_eq!(dynamic, expected, "nonliteral ui calls in {file}");
        if file == "compose.rs" {
            let mapping = source
                .split_once("let (code, message) = match gateway.availability {")
                .unwrap()
                .1
                .split_once("\n    };")
                .unwrap()
                .0;
            for (offset, _) in mapping.match_indices("=> (") {
                codes.insert(literal_after(&mapping[offset..], "=> (").to_owned());
            }
        }
        for (tail, _) in source.match_indices("code: \"") {
            codes.insert(literal_after(&source[tail..], "code: ").to_owned());
        }
    }
    for code in TEST_ONLY_CODES {
        assert!(codes.remove(*code), "stale test-only code: {code}");
    }
    let actual: Vec<_> = codes.into_iter().collect();
    let path = root.join("packages/contracts/fixtures/bridge-error-codes.json");
    if std::env::var("REGENERATE_BRIDGE_ERROR_CODES").as_deref() == Ok("1") {
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&actual).unwrap()),
        )
        .unwrap();
    }
    let expected: Vec<String> = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        expected, actual,
        "regenerate with REGENERATE_BRIDGE_ERROR_CODES=1"
    );
}
