//! Registry-wide guard: no command registered with `tauri::generate_handler!` takes a
//! secret-named parameter. The sources are read at test time, so a new command or parameter is
//! checked without editing this file.
//!
//! Matching rules:
//! - Registered names: every entry of the `generate_handler!` list and of the `command_handler!`
//!   invocation in `src/lib.rs`, by last path segment. `#[cfg(...)]` attributes on an entry are
//!   ignored, so a platform-gated command is still checked.
//! - Definitions: each `#[tauri::command]` function under `src/`, found through any further
//!   attributes and doc comments. A registered name must resolve to exactly one definition.
//! - Injected parameters are skipped by type (last path segment `State`, `AppHandle`, `Window`,
//!   `WebviewWindow` or `Webview`). Tauri supplies these from the runtime, not from the frontend.
//! - Secret-named parameters: the name, lowercased, contains `secret`, `password`, `passphrase`,
//!   `mnemonic`, `private_key` or `recovery`, or ends with `token`. Command names are not matched.
//! - Reviewed exceptions are `(command, parameter)` pairs in `ALLOWED_SECRET_PARAMETERS`.
//!
//! This file is excluded from the source scan because its own fixtures contain the attribute text.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

const SECRET_MARKERS: [&str; 6] = [
    "secret",
    "password",
    "passphrase",
    "mnemonic",
    "private_key",
    "recovery",
];

const INJECTED_TYPES: [&str; 5] = ["State", "AppHandle", "Window", "WebviewWindow", "Webview"];

/// Reviewed secret-named parameters that a registered command may take, as `(command, parameter)`.
/// Empty: no registered command takes a secret-named parameter today.
const ALLOWED_SECRET_PARAMETERS: &[(&str, &str)] = &[];

#[derive(Debug)]
struct Parameter {
    name: String,
    ty: String,
}

#[derive(Debug)]
struct Definition {
    file: String,
    name: String,
    parameters: Vec<Parameter>,
}

fn crate_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_crate_file(relative: &str) -> String {
    fs::read_to_string(crate_path(relative))
        .unwrap_or_else(|error| panic!("could not read {relative}: {error}"))
}

fn unparsed(file: &str) -> ! {
    panic!("could not parse the function after #[tauri::command] in {file}")
}

/// Bracket effect of the byte at `index`: +1 opens, -1 closes, 0 otherwise. `->` is not a bracket.
fn bracket_step(bytes: &[u8], index: usize) -> i32 {
    match bytes[index] {
        b'(' | b'[' | b'{' | b'<' => 1,
        b')' | b']' | b'}' => -1,
        b'>' if index == 0 || bytes[index - 1] != b'-' => -1,
        _ => 0,
    }
}

/// Index of the bracket that closes the one opened at `open`.
fn closing_index(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0;
    for index in open..bytes.len() {
        depth += bracket_step(bytes, index);
        if depth == 0 {
            return Some(index);
        }
    }
    None
}

/// Splits on commas that are outside any bracket or generic argument list.
fn split_top_level(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for index in 0..bytes.len() {
        if bytes[index] == b',' && depth == 0 {
            parts.push(&text[start..index]);
            start = index + 1;
        } else {
            depth += bracket_step(bytes, index);
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Blanks comments and the contents of string and char literals with spaces, before any bracket
/// or attribute matching. Newlines and byte offsets are kept. Lifetimes are left alone. Raw
/// strings are not recognised, since no signature in this crate uses one.
fn blank_non_code(text: &str) -> String {
    let source = text.as_bytes();
    let mut out = source.to_vec();
    let mut index = 0;
    while index < source.len() {
        match non_code_len(&source[index..]) {
            Some(len) => {
                for byte in &mut out[index..index + len] {
                    if *byte != b'\n' {
                        *byte = b' ';
                    }
                }
                index += len;
            }
            None => index += 1,
        }
    }
    String::from_utf8(out).expect("blanking ASCII bytes keeps UTF-8 valid")
}

/// Length of the comment or literal that starts `rest`, if one starts there.
fn non_code_len(rest: &[u8]) -> Option<usize> {
    if rest.starts_with(b"//") {
        return Some(rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len()));
    }
    if rest.starts_with(b"/*") {
        return Some(block_comment_len(rest));
    }
    match rest[0] {
        b'"' => Some(string_literal_len(rest)),
        b'\'' => char_literal_len(rest),
        _ => None,
    }
}

/// Rust block comments nest.
fn block_comment_len(rest: &[u8]) -> usize {
    let mut depth = 0;
    let mut index = 0;
    while index < rest.len() {
        if rest[index..].starts_with(b"/*") {
            depth += 1;
            index += 2;
        } else if rest[index..].starts_with(b"*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += 1;
        }
    }
    rest.len()
}

fn string_literal_len(rest: &[u8]) -> usize {
    let mut index = 1;
    while index < rest.len() {
        match rest[index] {
            b'\\' => index += 2,
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    rest.len()
}

/// Char literals such as `'('` or `'\''`. A lifetime such as `'a` is not a literal.
fn char_literal_len(rest: &[u8]) -> Option<usize> {
    if rest.get(1) == Some(&b'\\') {
        rest.get(3..)
            .unwrap_or_default()
            .iter()
            .position(|&b| b == b'\'')
            .map(|position| position + 4)
    } else if rest.get(2) == Some(&b'\'') {
        Some(3)
    } else {
        None
    }
}

/// Drops leading `#[...]` attributes from a fragment.
fn strip_attributes(mut text: &str) -> &str {
    while let Some(rest) = text.strip_prefix("#[") {
        let end = rest
            .find(']')
            .unwrap_or_else(|| panic!("unterminated attribute in `{text}`"));
        text = rest[end + 1..].trim_start();
    }
    text
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_secret_named(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_MARKERS.iter().any(|marker| lower.contains(marker)) || lower.ends_with("token")
}

fn parameter(fragment: &str) -> Parameter {
    let fragment = strip_attributes(fragment.trim());
    let fragment = fragment.strip_prefix("mut ").unwrap_or(fragment).trim();
    let (name, ty) = fragment
        .split_once(':')
        .unwrap_or_else(|| panic!("parameter without a type: `{fragment}`"));
    let name = name.trim();
    assert!(
        is_identifier(name),
        "unsupported parameter pattern `{name}`"
    );
    let head = ty.split('<').next().unwrap_or_default();
    Parameter {
        name: name.to_string(),
        ty: head
            .rsplit("::")
            .next()
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

/// Parses the function that follows the `#[tauri::command]` attribute ending just before `index`.
fn definition_after(file: &str, text: &str, mut index: usize) -> Definition {
    loop {
        let rest = &text[index..];
        let trimmed = rest.trim_start();
        index += rest.len() - trimmed.len();
        if trimmed.starts_with("#[") {
            index = closing_index(text, index + 1).unwrap_or_else(|| unparsed(file)) + 1;
        } else if trimmed.starts_with("//") {
            index += trimmed.find('\n').unwrap_or(trimmed.len());
        } else {
            break;
        }
    }

    let mut header = text[index..].trim_start();
    if let Some(rest) = header.strip_prefix("pub") {
        header = if rest.starts_with('(') {
            rest[closing_index(rest, 0).unwrap_or_else(|| unparsed(file)) + 1..].trim_start()
        } else {
            rest.trim_start()
        };
    }
    if let Some(rest) = header.strip_prefix("async") {
        header = rest.trim_start();
    }
    header = header
        .strip_prefix("fn")
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .unwrap_or_else(|| unparsed(file))
        .trim_start();

    let name_len = header
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(header.len());
    let name = &header[..name_len];
    header = header[name_len..].trim_start();
    if header.starts_with('<') {
        header =
            header[closing_index(header, 0).unwrap_or_else(|| unparsed(file)) + 1..].trim_start();
    }
    if !header.starts_with('(') {
        unparsed(file);
    }
    let end = closing_index(header, 0).unwrap_or_else(|| unparsed(file));
    Definition {
        file: file.to_string(),
        name: name.to_string(),
        parameters: split_top_level(&header[1..end])
            .into_iter()
            .map(str::trim)
            .filter(|fragment| !fragment.is_empty())
            .map(parameter)
            .collect(),
    }
}

fn definitions_in(file: &str, text: &str) -> Vec<Definition> {
    const ATTRIBUTE: &str = "#[tauri::command";
    let text = blank_non_code(text);
    let text = text.as_str();
    let mut definitions = Vec::new();
    let mut search = 0;
    while let Some(offset) = text[search..].find(ATTRIBUTE) {
        let attribute = search + offset;
        let end = text[attribute..]
            .find(']')
            .unwrap_or_else(|| panic!("unterminated #[tauri::command] in {file}"));
        let after = attribute + end + 1;
        definitions.push(definition_after(file, text, after));
        search = after;
    }
    definitions
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension() == Some(OsStr::new("rs")) {
            files.push(path);
        }
    }
}

fn source_definitions() -> BTreeMap<String, Vec<Definition>> {
    let mut files = Vec::new();
    collect_rust_files(&crate_path("src"), &mut files);
    files.sort();
    let mut definitions: BTreeMap<String, Vec<Definition>> = BTreeMap::new();
    for file in files {
        if file.file_name() == Some(OsStr::new("command_registry_tests.rs")) {
            continue;
        }
        let relative = file
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&file)
            .display()
            .to_string();
        let text = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("could not read {relative}: {error}"));
        for definition in definitions_in(&relative, &text) {
            definitions
                .entry(definition.name.clone())
                .or_default()
                .push(definition);
        }
    }
    definitions
}

/// Last path segments of every entry in the `generate_handler!` list and the `command_handler!`
/// invocation. Each entry is one path per line, optionally preceded by attributes.
fn handler_entries(lib: &str) -> Vec<String> {
    let lib = blank_non_code(lib);
    let mut entries = Vec::new();
    for opener in ["generate_handler![", "command_handler!("] {
        let mut search = 0;
        while let Some(offset) = lib[search..].find(opener) {
            let open = search + offset + opener.len() - 1;
            let end = closing_index(&lib, open)
                .unwrap_or_else(|| panic!("unterminated {opener} in src/lib.rs"));
            for line in lib[open + 1..end].lines() {
                let line = strip_attributes(line.trim()).trim_end_matches(',').trim();
                if line.is_empty() || line.starts_with('$') {
                    continue;
                }
                assert!(
                    line.split("::").all(is_identifier),
                    "unexpected handler entry `{line}` in src/lib.rs"
                );
                entries.push(line.rsplit("::").next().unwrap_or_default().to_string());
            }
            search = end;
        }
    }
    entries
}

fn registered_commands() -> BTreeSet<String> {
    let entries = handler_entries(&read_crate_file("src/lib.rs"));
    let unique: BTreeSet<String> = entries.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        entries.len(),
        "a command is registered more than once"
    );
    assert!(!unique.is_empty(), "no registered commands were found");
    unique
}

fn build_manifest_commands() -> BTreeSet<String> {
    let build = read_crate_file("build.rs");
    let start = build
        .find("const COMMANDS: &[&str] = &[")
        .expect("build.rs declares COMMANDS");
    let body = &build[start..];
    let body = &body[..body.find("];").expect("COMMANDS is closed")];
    body.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

fn names(parameters: &[Parameter]) -> Vec<&str> {
    parameters.iter().map(|p| p.name.as_str()).collect()
}

fn single_definition(source: &str) -> Definition {
    let mut definitions = definitions_in("snippet.rs", source);
    assert_eq!(
        definitions.len(),
        1,
        "expected one #[tauri::command] function"
    );
    definitions.remove(0)
}

#[test]
fn line_comment_with_a_closing_paren_keeps_later_parameters() {
    let definition = single_definition(
        r#"
#[tauri::command]
fn example(
    // 1) Credentials are collected natively.
    api_token: String,
) {}
"#,
    );

    assert_eq!(names(&definition.parameters), ["api_token"]);
    assert!(is_secret_named(&definition.parameters[0].name));
}

#[test]
fn block_comments_and_literals_do_not_disturb_the_signature() {
    let definition = single_definition(
        r#"
/* #[tauri::command]
fn ghost(token: String) {} */
#[tauri::command(rename_all = "a]b")]
pub async fn configure<'a>(
    /* closes ) and opens ( */
    #[serde(rename = "x)")]
    label: &'a str,
    // a trailing ) in a comment
    note: Option<String>, /* ) */
    shared_secret: String,
) -> Result<(), String> {
    let _ = ")";
    Ok(())
}
"#,
    );

    assert_eq!(definition.name, "configure");
    assert_eq!(
        names(&definition.parameters),
        ["label", "note", "shared_secret"]
    );
    assert!(is_secret_named(&definition.parameters[2].name));
}

#[test]
fn blanking_keeps_lifetimes_and_char_literals_apart() {
    let source = "fn f<'a>(c: char) { let _ = '('; let _ = '\\''; } // )";

    // The lifetime stays, the literals and the comment become spaces, and offsets are unchanged.
    assert_eq!(
        blank_non_code(source),
        "fn f<'a>(c: char) { let _ =    ; let _ =     ; }     "
    );
    assert_eq!(blank_non_code(source).len(), source.len());
}

#[test]
fn scanner_reads_multiline_generic_and_gated_signatures() {
    let source = r#"
#[tauri::command]
pub async fn sample<'a, T: Fn() -> u8>(
    // a comment, with a comma, that is ignored
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    mut token_value: Option<String>,
    callback: T,
) -> Result<(), String> { Ok(()) }

#[tauri::command(rename_all = "snake_case")]
#[cfg(test)]
fn gated(shared_secret: String) {}
"#;

    let definitions = definitions_in("sample.rs", source);

    assert_eq!(definitions.len(), 2);
    assert_eq!(definitions[0].name, "sample");
    assert_eq!(
        names(&definitions[0].parameters),
        ["window", "state", "token_value", "callback"]
    );
    let types: Vec<&str> = definitions[0]
        .parameters
        .iter()
        .map(|p| p.ty.as_str())
        .collect();
    assert_eq!(types, ["WebviewWindow", "State", "Option", "T"]);
    assert_eq!(definitions[1].name, "gated");
    assert_eq!(names(&definitions[1].parameters), ["shared_secret"]);
}

#[test]
fn secret_name_rule_matches_the_documented_markers() {
    for secret in [
        "shared_secret",
        "Password",
        "passphrase",
        "mnemonic_words",
        "private_key_pem",
        "recovery_code",
        "access_token",
        "token",
    ] {
        assert!(is_secret_named(secret), "{secret} should match");
    }
    for ordinary in ["token_value", "max_tokens", "items_count", "id", "body"] {
        assert!(!is_secret_named(ordinary), "{ordinary} should not match");
    }
}

#[test]
fn registered_commands_match_the_build_manifest() {
    let registered = registered_commands();
    let manifest = build_manifest_commands();
    assert_eq!(
        registered,
        manifest,
        "generate_handler! and build.rs COMMANDS differ: only registered {:?}; only in build.rs {:?}",
        registered.difference(&manifest).collect::<Vec<_>>(),
        manifest.difference(&registered).collect::<Vec<_>>(),
    );
}

#[test]
fn every_registered_command_resolves_to_one_definition() {
    let definitions = source_definitions();
    let mut missing = Vec::new();
    let mut ambiguous = Vec::new();
    for name in registered_commands() {
        match definitions.get(&name).map(Vec::len) {
            None => missing.push(name),
            Some(1) => {}
            Some(_) => ambiguous.push(name),
        }
    }
    assert!(
        missing.is_empty(),
        "registered commands without a #[tauri::command] definition: {missing:?}"
    );
    assert!(
        ambiguous.is_empty(),
        "registered commands with more than one definition: {ambiguous:?}"
    );
}

#[test]
fn no_registered_command_takes_a_secret_named_parameter() {
    let definitions = source_definitions();
    let registered = registered_commands();
    let is_allowed = |command: &str, parameter: &str| {
        ALLOWED_SECRET_PARAMETERS
            .iter()
            .any(|&(allowed_command, allowed_parameter)| {
                allowed_command == command && allowed_parameter == parameter
            })
    };

    let mut violations = Vec::new();
    for name in &registered {
        for definition in definitions.get(name).into_iter().flatten() {
            for parameter in definition
                .parameters
                .iter()
                .filter(|p| !INJECTED_TYPES.contains(&p.ty.as_str()))
            {
                if is_secret_named(&parameter.name) && !is_allowed(name, &parameter.name) {
                    violations.push(format!(
                        "{name}({}: {}) in {}",
                        parameter.name, parameter.ty, definition.file
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "registered commands take secret-named parameters: {violations:?}"
    );

    for &(command, parameter) in ALLOWED_SECRET_PARAMETERS {
        let matches = definitions.get(command).is_some_and(|found| {
            found
                .iter()
                .any(|d| d.parameters.iter().any(|p| p.name == parameter))
        });
        assert!(
            registered.contains(command) && matches && is_secret_named(parameter),
            "allow-list entry ({command}, {parameter}) matches no registered command parameter"
        );
    }
}
