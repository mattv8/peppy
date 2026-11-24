//! Writes `packages/contracts/fixtures/tauri-commands.json`: every Tauri command of the desktop
//! app with the argument keys its webview bridge sends.
//!
//! The manifest is derived from source with `syn`, so every construct this tool cannot classify
//! fails closed (exit 2) instead of guessing. Keys follow tauri-macros 2.7.0: arguments are
//! converted with heck, exactly as the `#[tauri::command]` wrapper does at compile time.
//!
//! CI runs the same command with `--check`:
//! `cargo run --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --features command-manifest --bin generate-tauri-command-contracts -- --check`

use heck::{ToLowerCamelCase, ToSnakeCase};
use proc_macro2::{Span, TokenStream, TokenTree};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs, io,
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
};
use syn::{
    ext::IdentExt, punctuated::Punctuated, spanned::Spanned, Attribute, Expr, ExprLit, FnArg, Item,
    ItemFn, Lit, Meta, Pat, Token, Type, UseTree,
};

/// Message for command attribute arguments outside the `rename` / `rename_all` allowlist.
const UNSUPPORTED_ARGUMENT: &str =
    "unsupported tauri::command argument; only rename and rename_all are allowed";

/// Runtime parameter types Tauri injects without a payload key, by canonical path in tauri 2.12
/// (the root re-exports and `tauri::ipc`). A written path counts only when it resolves to one of
/// these exactly (see [`Scope`]).
const KEYLESS_PATHS: [&[&str]; 8] = [
    &["tauri", "State"],
    &["tauri", "AppHandle"],
    &["tauri", "Window"],
    &["tauri", "Webview"],
    &["tauri", "WebviewWindow"],
    &["tauri", "ipc", "Request"],
    &["tauri", "ipc", "CommandScope"],
    &["tauri", "ipc", "GlobalScope"],
];

/// Macros whose argument list registers command functions with the invoke handler.
const HANDLER_MACROS: [&str; 2] = ["generate_handler", "command_handler"];

const MANIFEST_RELATIVE_PATH: &str = "packages/contracts/fixtures/tauri-commands.json";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArgCase {
    Camel,
    Snake,
}

#[derive(Debug, PartialEq, Eq)]
struct Command {
    /// Name the webview invokes: the `rename` value or the function identifier.
    name: String,
    /// Rust function identifier, as registered in the invoke handler.
    ident: String,
    /// Argument keys in the invoke payload, sorted.
    args: Vec<String>,
}

struct CommandAttr {
    rename: Option<String>,
    arg_case: ArgCase,
}

fn main() -> ExitCode {
    let check = match env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => {
            eprintln!("usage: generate-tauri-command-contracts [--check]");
            return ExitCode::from(2);
        }
    };
    match run(check) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

/// Returns `Ok(false)` when `--check` finds a stale manifest.
fn run(check: bool) -> Result<bool, String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = fs::canonicalize(manifest_dir.join("../../.."))
        .map_err(|error| format!("cannot resolve the repository root: {error}"))?;
    let output = repo_root.join(MANIFEST_RELATIVE_PATH);

    let commands = discover_commands(&manifest_dir.join("src"), &repo_root)?;
    let build_names = build_command_names(&manifest_dir.join("build.rs"))?;
    let handler_idents = handler_registrations(&manifest_dir.join("src/lib.rs"))?;
    cross_check(&commands, &build_names, &handler_idents)?;

    let rendered = render(&commands);
    if !check {
        fs::write(&output, &rendered)
            .map_err(|error| format!("cannot write {}: {error}", output.display()))?;
        println!(
            "wrote {} ({} commands)",
            MANIFEST_RELATIVE_PATH,
            commands.len()
        );
        return Ok(true);
    }

    match compare_golden(fs::read_to_string(&output), &rendered, &output)? {
        Golden::Current => {
            println!(
                "{MANIFEST_RELATIVE_PATH} is up to date ({} commands)",
                commands.len()
            );
            Ok(true)
        }
        Golden::Missing => {
            eprintln!("{MANIFEST_RELATIVE_PATH} is missing; run without --check to create it.");
            Ok(false)
        }
        Golden::Stale(names) => {
            let differing = if names.is_empty() {
                "(header or ordering)".to_string()
            } else {
                names.join(", ")
            };
            eprintln!(
                "{MANIFEST_RELATIVE_PATH} is stale. Commands that differ: {differing}. Run without --check to regenerate."
            );
            Ok(false)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Golden {
    Current,
    Missing,
    /// Names of the commands whose lines differ.
    Stale(Vec<String>),
}

/// Only a missing golden is "missing"; any other read failure is an error, not staleness.
fn compare_golden(read: io::Result<String>, rendered: &str, path: &Path) -> Result<Golden, String> {
    match read {
        Ok(committed) if committed == rendered => Ok(Golden::Current),
        Ok(committed) => Ok(Golden::Stale(stale_commands(&committed, rendered))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Golden::Missing),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn discover_commands(src_dir: &Path, repo_root: &Path) -> Result<Vec<Command>, String> {
    let mut walk = ModuleWalk {
        repo_root: repo_root.to_path_buf(),
        visited: BTreeSet::new(),
        commands: Vec::new(),
    };
    // Only files in the module tree of a crate root are compiled, so only they can define
    // commands. Commands the walk misses surface in the cross-check against COMMANDS and the
    // handler registrations.
    for root in ["lib.rs", "main.rs"] {
        let path = src_dir.join(root);
        if path.is_file() {
            walk.visit_file(&path, &Scope::default(), None)?;
        }
    }
    let mut commands = walk.commands;
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    for pair in commands.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(format!(
                "command \"{}\" is declared more than once",
                pair[0].name
            ));
        }
    }
    Ok(commands)
}

/// The `use` items of one module scope. Rust does not share imports with child modules, so each
/// module (and the file root) gets its own scope.
#[derive(Default)]
struct Scope {
    /// Local name -> full path it was imported from, e.g. `S` -> `["tauri", "State"]`.
    imports: BTreeMap<String, Vec<String>>,
}

impl Scope {
    /// Scope of a module's items. `use super::*` brings the parent's imports in (private `use`
    /// items are glob-importable by children); the module's own imports shadow them.
    fn from_items(items: &[Item], parent: &Scope) -> Self {
        let mut scope = Scope::default();
        if items.iter().any(is_super_glob) {
            scope.imports = parent.imports.clone();
        }
        for item in items {
            if let Item::Use(item_use) = item {
                scope.add_use_tree(&item_use.tree, &[]);
            }
        }
        scope
    }

    /// Glob imports bind no names here; a keyless name they might supply is rejected in
    /// [`is_keyless_path`] instead of guessed.
    fn add_use_tree(&mut self, tree: &UseTree, prefix: &[String]) {
        match tree {
            UseTree::Path(path) => {
                let mut next = prefix.to_vec();
                next.push(path.ident.to_string());
                self.add_use_tree(&path.tree, &next);
            }
            UseTree::Name(name) => self.bind(&name.ident.to_string(), prefix),
            UseTree::Rename(rename) => {
                let mut full = prefix.to_vec();
                full.push(rename.ident.to_string());
                self.imports.insert(rename.rename.to_string(), full);
            }
            UseTree::Group(group) => {
                for item in &group.items {
                    self.add_use_tree(item, prefix);
                }
            }
            UseTree::Glob(_) => {}
        }
    }

    /// `use a::b::{self}` imports `b`; `use a::b` imports `b` from `a`.
    fn bind(&mut self, name: &str, prefix: &[String]) {
        if name == "self" {
            if let Some((last, head)) = prefix.split_last() {
                self.imports.insert(last.clone(), head.to_vec());
            }
            return;
        }
        let mut full = prefix.to_vec();
        full.push(name.to_string());
        self.imports.insert(name.to_string(), full);
    }

    /// The full path a written path refers to, following an import of its first segment.
    fn resolve(&self, written: &[String]) -> Vec<String> {
        match written.split_first() {
            Some((first, rest)) if first != "tauri" => match self.imports.get(first) {
                Some(full) => full.iter().chain(rest).cloned().collect(),
                None => written.to_vec(),
            },
            _ => written.to_vec(),
        }
    }
}

fn is_super_glob(item: &Item) -> bool {
    matches!(item, Item::Use(item_use)
        if matches!(&item_use.tree, UseTree::Path(path)
            if path.ident == "super" && matches!(&*path.tree, UseTree::Glob(_))))
}

fn segment_names(path: &syn::Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}

/// Walks the module tree from the crate roots. Each file is parsed once, and each module sees the
/// scope of the module that declares it, so `use super::*` and `#[cfg]` gating are judged on the
/// real parents.
struct ModuleWalk {
    repo_root: PathBuf,
    visited: BTreeSet<PathBuf>,
    commands: Vec<Command>,
}

impl ModuleWalk {
    fn label(&self, path: &Path) -> String {
        path.strip_prefix(&self.repo_root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// `module_dir` is the directory of the file's child modules; `None` derives it from the path.
    fn visit_file(
        &mut self,
        path: &Path,
        parent: &Scope,
        module_dir: Option<PathBuf>,
    ) -> Result<(), String> {
        if !self.visited.insert(path.to_path_buf()) {
            return Ok(());
        }
        let label = self.label(path);
        let source = fs::read_to_string(path).map_err(|error| format!("{label}: {error}"))?;
        let file = syn::parse_file(&source).map_err(|error| format!("{label}: {error}"))?;
        let scope = Scope::from_items(&file.items, parent);
        let module_dir = module_dir.unwrap_or_else(|| module_dir_of(path));
        let file_dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        self.collect(&label, &file.items, &scope, &module_dir, Some(&file_dir))
    }

    /// `file_dir` is the directory of the declaring file; `None` inside an inline module, where
    /// `#[path]` is not supported.
    fn collect(
        &mut self,
        label: &str,
        items: &[Item],
        scope: &Scope,
        module_dir: &Path,
        file_dir: Option<&Path>,
    ) -> Result<(), String> {
        for item in items {
            match item {
                Item::Fn(function) => self.collect_fn(label, function, scope)?,
                Item::Mod(module) => {
                    let name = module.ident.unraw().to_string();
                    let before = self.commands.len();
                    match &module.content {
                        Some((_, inner)) => {
                            if module.attrs.iter().any(|attr| attr.path().is_ident("path")) {
                                return Err(fail_span(
                                    label,
                                    module.ident.span(),
                                    &name,
                                    "#[path] on an inline module is not supported by this tool",
                                ));
                            }
                            let child = Scope::from_items(inner, scope);
                            let child_dir = module_dir.join(&name);
                            self.collect(label, inner, &child, &child_dir, None)?;
                        }
                        None => {
                            self.visit_module_file(label, module, scope, module_dir, file_dir)?
                        }
                    }
                    if self.commands.len() > before && module.attrs.iter().any(is_cfg_attr) {
                        return Err(fail_span(
                            label,
                            module.ident.span(),
                            &name,
                            "a module gated by #[cfg] contains tauri commands that cannot be listed unconditionally",
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn collect_fn(&mut self, label: &str, function: &ItemFn, scope: &Scope) -> Result<(), String> {
        let command = command_attr(label, function, scope)?;
        let cfg_gated = function
            .attrs
            .iter()
            .any(|attr| is_cfg_attr(attr) && (command.is_some() || mentions_command(attr)));
        if cfg_gated {
            return Err(fail(
                label,
                function,
                "a tauri command gated by #[cfg] cannot be listed unconditionally",
            ));
        }
        if let Some(attr) = command {
            let command = command_from_fn(label, function, &attr, scope)?;
            self.commands.push(command);
        }
        Ok(())
    }

    fn visit_module_file(
        &mut self,
        label: &str,
        module: &syn::ItemMod,
        scope: &Scope,
        module_dir: &Path,
        file_dir: Option<&Path>,
    ) -> Result<(), String> {
        let name = module.ident.unraw().to_string();
        let explicit = path_attribute(label, module)?;
        if let Some(relative) = explicit {
            let Some(dir) = file_dir else {
                return Err(fail_span(
                    label,
                    module.ident.span(),
                    &name,
                    "#[path] on a module inside an inline module is not supported by this tool",
                ));
            };
            let file = dir.join(&relative);
            if !file.is_file() {
                return Err(fail_span(
                    label,
                    module.ident.span(),
                    &name,
                    "#[path] module file not found",
                ));
            }
            // A file loaded through #[path] owns its own directory, like a mod.rs.
            let owned = file.parent().map(Path::to_path_buf);
            return self.visit_file(&file, scope, owned);
        }
        let candidates: Vec<PathBuf> = [
            module_dir.join(format!("{name}.rs")),
            module_dir.join(&name).join("mod.rs"),
        ]
        .into_iter()
        .filter(|path| path.is_file())
        .collect();
        match candidates.as_slice() {
            [file] => self.visit_file(file, scope, None),
            [] => Err(fail_span(
                label,
                module.ident.span(),
                &name,
                "module file not found",
            )),
            _ => Err(fail_span(
                label,
                module.ident.span(),
                &name,
                "module file is ambiguous (both name.rs and name/mod.rs exist)",
            )),
        }
    }
}

/// Directory that holds a module's child files: `mod.rs` and crate roots own their directory;
/// `name.rs` owns `name/`.
fn module_dir_of(path: &Path) -> PathBuf {
    match path.file_name().and_then(|name| name.to_str()) {
        Some("lib.rs" | "main.rs" | "mod.rs") => {
            path.parent().unwrap_or(Path::new("")).to_path_buf()
        }
        _ => path.with_extension(""),
    }
}

/// The `#[path = "..."]` value of a module, if it has one.
fn path_attribute(label: &str, module: &syn::ItemMod) -> Result<Option<String>, String> {
    let Some(attr) = module
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("path"))
    else {
        return Ok(None);
    };
    let name = module.ident.unraw().to_string();
    let value = match &attr.meta {
        Meta::NameValue(pair) => string_value(&pair.value),
        _ => None,
    };
    value.map(Some).ok_or_else(|| {
        fail_span(
            label,
            module.ident.span(),
            &name,
            "#[path] must be written as #[path = \"...\"]",
        )
    })
}

fn is_cfg_attr(attr: &Attribute) -> bool {
    attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr")
}

/// A `cfg_attr(...)` whose arguments mention `command` may enable a command this tool cannot see.
fn mentions_command(attr: &Attribute) -> bool {
    match &attr.meta {
        Meta::List(list) => list.tokens.to_string().contains("command"),
        _ => false,
    }
}

fn command_attr(
    label: &str,
    function: &ItemFn,
    scope: &Scope,
) -> Result<Option<CommandAttr>, String> {
    let mut found = None;
    for attr in &function.attrs {
        if !is_command_attr(attr, scope) {
            continue;
        }
        if found.is_some() {
            return Err(fail(
                label,
                function,
                "more than one tauri::command attribute",
            ));
        }
        let parsed = parse_command_attr(attr).map_err(|reason| fail(label, function, &reason))?;
        found = Some(parsed);
    }
    Ok(found)
}

/// A bare `#[command]` counts only when its scope imports `tauri::command`.
fn is_command_attr(attr: &Attribute, scope: &Scope) -> bool {
    let written = segment_names(attr.path());
    scope.resolve(&written) == ["tauri", "command"]
}

fn parse_command_attr(attr: &Attribute) -> Result<CommandAttr, String> {
    let mut parsed = CommandAttr {
        rename: None,
        arg_case: ArgCase::Camel,
    };
    match &attr.meta {
        Meta::Path(_) => {}
        Meta::List(list) => {
            let arguments = list
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .map_err(|error| error.to_string())?;
            for argument in arguments {
                let Meta::NameValue(pair) = argument else {
                    return Err(UNSUPPORTED_ARGUMENT.into());
                };
                let value = string_value(&pair.value)
                    .ok_or("tauri::command argument values must be string literals")?;
                if pair.path.is_ident("rename") {
                    parsed.rename = Some(value);
                } else if pair.path.is_ident("rename_all") {
                    parsed.arg_case = match value.as_str() {
                        "camelCase" => ArgCase::Camel,
                        "snake_case" => ArgCase::Snake,
                        other => return Err(format!("unsupported rename_all \"{other}\"")),
                    };
                } else {
                    return Err(UNSUPPORTED_ARGUMENT.into());
                }
            }
        }
        Meta::NameValue(_) => return Err("unsupported tauri::command attribute form".into()),
    }
    Ok(parsed)
}

fn string_value(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }) => Some(value.value()),
        _ => None,
    }
}

fn command_from_fn(
    label: &str,
    function: &ItemFn,
    attr: &CommandAttr,
    scope: &Scope,
) -> Result<Command, String> {
    let ident = function.sig.ident.unraw().to_string();
    let name = attr.rename.clone().unwrap_or_else(|| ident.clone());
    let type_params: Vec<String> = function
        .sig
        .generics
        .type_params()
        .map(|p| p.ident.to_string())
        .collect();

    let mut args = Vec::new();
    for input in &function.sig.inputs {
        let typed = match input {
            FnArg::Receiver(receiver) => {
                return Err(fail_at(
                    label,
                    receiver.span(),
                    function,
                    "commands cannot take self",
                ));
            }
            FnArg::Typed(typed) => typed,
        };
        let pat = match &*typed.pat {
            Pat::Wild(_) => continue,
            Pat::Ident(pat) if pat.subpat.is_none() => pat,
            other => {
                return Err(fail_at(
                    label,
                    other.span(),
                    function,
                    "parameter pattern must be a plain identifier",
                ));
            }
        };
        let keyless = is_keyless(&typed.ty, &type_params, scope)
            .map_err(|reason| fail_at(label, typed.ty.span(), function, &reason))?;
        if keyless {
            continue;
        }
        args.push(argument_key(&pat.ident.unraw().to_string(), attr.arg_case));
    }

    args.sort();
    if args.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(fail(
            label,
            function,
            "two arguments map to the same payload key",
        ));
    }
    Ok(Command { name, ident, args })
}

fn is_keyless(ty: &Type, type_params: &[String], scope: &Scope) -> Result<bool, String> {
    let mut inner: &Type = ty;
    loop {
        inner = match inner {
            Type::Reference(reference) => &reference.elem,
            Type::Paren(paren) => &paren.elem,
            Type::Group(group) => &group.elem,
            _ => break,
        };
    }
    match inner {
        Type::ImplTrait(_) => Err("impl Trait parameter types cannot be classified".into()),
        Type::Path(path) if path.qself.is_none() => {
            let written = segment_names(&path.path);
            let Some(last) = written.last() else {
                return Ok(false);
            };
            if written.len() == 1 && type_params.contains(last) {
                return Err("generic parameter types cannot be classified".into());
            }
            is_keyless_path(&written, scope)
        }
        _ => Ok(false),
    }
}

/// Tauri keys every parameter except the runtime types that implement `CommandArg` without a
/// payload. A path counts as keyless only when it resolves exactly to one of [`KEYLESS_PATHS`].
/// A path that merely ends in a keyless name (a local DTO, `tauri::http::Request`, an alias of
/// another crate's type) fails closed.
fn is_keyless_path(written: &[String], scope: &Scope) -> Result<bool, String> {
    let full = scope.resolve(written);
    let Some(last) = full.last() else {
        return Ok(false);
    };
    if !KEYLESS_PATHS
        .iter()
        .any(|path| path.last() == Some(&last.as_str()))
    {
        return Ok(false);
    }
    if KEYLESS_PATHS
        .iter()
        .any(|path| full.iter().map(String::as_str).eq(path.iter().copied()))
    {
        return Ok(true);
    }
    Err(format!(
        "`{}` has a keyless Tauri type name but does not resolve to a keyless tauri type (tauri::State, tauri::AppHandle, tauri::Window, tauri::Webview, tauri::WebviewWindow, tauri::ipc::Request, tauri::ipc::CommandScope, tauri::ipc::GlobalScope); import it from tauri or qualify it",
        written.join("::")
    ))
}

/// Mirrors tauri-macros 2.7.0 (`src/command/wrapper.rs`): camelCase by default, heck conversions.
fn argument_key(name: &str, arg_case: ArgCase) -> String {
    match arg_case {
        ArgCase::Camel => name.to_lower_camel_case(),
        ArgCase::Snake => name.to_snake_case(),
    }
}

fn fail(label: &str, function: &ItemFn, reason: &str) -> String {
    fail_at(label, function.sig.ident.span(), function, reason)
}

fn fail_at(label: &str, span: Span, function: &ItemFn, reason: &str) -> String {
    fail_span(label, span, &function.sig.ident.to_string(), reason)
}

/// `file:line name: reason`, the fail-closed format for every classification error.
fn fail_span(label: &str, span: Span, name: &str, reason: &str) -> String {
    format!("{label}:{} {name}: {reason}", span.start().line)
}

/// Command names listed in `COMMANDS` (the app ACL manifest in build.rs).
fn build_command_names(build_rs: &Path) -> Result<BTreeSet<String>, String> {
    let label = "build.rs";
    let source = fs::read_to_string(build_rs).map_err(|error| format!("{label}: {error}"))?;
    let file = syn::parse_file(&source).map_err(|error| format!("{label}: {error}"))?;
    for item in &file.items {
        let expr = match item {
            Item::Const(item) if item.ident == "COMMANDS" => &item.expr,
            Item::Static(item) if item.ident == "COMMANDS" => &item.expr,
            _ => continue,
        };
        let Expr::Reference(reference) = &**expr else {
            return Err(format!("{label}: COMMANDS must be a slice literal"));
        };
        let Expr::Array(array) = &*reference.expr else {
            return Err(format!("{label}: COMMANDS must be a slice literal"));
        };
        let mut names = Vec::new();
        for element in &array.elems {
            names.push(
                string_value(element)
                    .ok_or_else(|| format!("{label}: COMMANDS entries must be string literals"))?,
            );
        }
        let unique: BTreeSet<String> = names.iter().cloned().collect();
        if unique.len() != names.len() {
            return Err(format!("{label}: COMMANDS lists a name more than once"));
        }
        return Ok(unique);
    }
    Err(format!("{label}: no COMMANDS constant found"))
}

/// Function identifiers passed to `tauri::generate_handler!` (directly or via `command_handler!`).
fn handler_registrations(lib_rs: &Path) -> Result<BTreeSet<String>, String> {
    let label = "src/lib.rs";
    let source = fs::read_to_string(lib_rs).map_err(|error| format!("{label}: {error}"))?;
    handler_idents(&handler_paths_in(&source)?)
}

/// Full paths of every registration in a source file, in order.
fn handler_paths_in(source: &str) -> Result<Vec<String>, String> {
    let tokens = TokenStream::from_str(source).map_err(|error| format!("src/lib.rs: {error}"))?;
    let mut paths = Vec::new();
    collect_handler_paths(tokens, &mut paths)?;
    if paths.is_empty() {
        return Err("src/lib.rs: no generate_handler registrations found".into());
    }
    Ok(paths)
}

/// Function identifiers of the registrations. The handler keys by function path, so two
/// registrations of one path, or two different paths ending in one identifier, are errors.
fn handler_idents(paths: &[String]) -> Result<BTreeSet<String>, String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for path in paths {
        let ident = path.rsplit("::").next().unwrap_or(path);
        match seen.get(ident) {
            Some(previous) if *previous == path.as_str() => {
                return Err(format!(
                    "src/lib.rs: handler registers {path} more than once"
                ));
            }
            Some(previous) => {
                return Err(format!(
                    "src/lib.rs: handler paths {previous} and {path} share the last segment `{ident}`"
                ));
            }
            None => {
                seen.insert(ident, path);
            }
        }
    }
    Ok(seen.keys().map(|ident| (*ident).to_string()).collect())
}

fn collect_handler_paths(stream: TokenStream, out: &mut Vec<String>) -> Result<(), String> {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    for (index, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Group(group) => collect_handler_paths(group.stream(), out)?,
            TokenTree::Ident(ident) if HANDLER_MACROS.contains(&ident.to_string().as_str()) => {
                if let (Some(TokenTree::Punct(bang)), Some(TokenTree::Group(args))) =
                    (tokens.get(index + 1), tokens.get(index + 2))
                {
                    if bang.as_char() == '!' {
                        collect_registered_paths(args.stream(), out)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn collect_registered_paths(args: TokenStream, out: &mut Vec<String>) -> Result<(), String> {
    for entry in split_top_level_commas(args) {
        // Entries containing `$` are macro-variable pass-through inside command_handler!.
        let is_macro_variable = entry
            .clone()
            .into_iter()
            .any(|token| matches!(&token, TokenTree::Punct(punct) if punct.as_char() == '$'));
        if is_macro_variable {
            continue;
        }
        let path: syn::Path = syn::parse2(entry).map_err(|error| format!("src/lib.rs: {error}"))?;
        let written = segment_names(&path);
        if written.is_empty() {
            return Err("src/lib.rs: empty handler entry".into());
        }
        out.push(written.join("::"));
    }
    Ok(())
}

fn split_top_level_commas(stream: TokenStream) -> Vec<TokenStream> {
    let mut entries = vec![TokenStream::new()];
    for token in stream {
        if matches!(&token, TokenTree::Punct(punct) if punct.as_char() == ',') {
            entries.push(TokenStream::new());
        } else if let Some(current) = entries.last_mut() {
            current.extend(std::iter::once(token));
        }
    }
    entries
        .into_iter()
        .filter(|entry| !entry.is_empty())
        .collect()
}

fn cross_check(
    commands: &[Command],
    build_names: &BTreeSet<String>,
    handler_idents: &BTreeSet<String>,
) -> Result<(), String> {
    let names: BTreeSet<String> = commands.iter().map(|c| c.name.clone()).collect();
    let idents: BTreeSet<String> = commands.iter().map(|c| c.ident.clone()).collect();
    let mut problems = Vec::new();
    if names != *build_names {
        problems.push(describe_difference(
            "build.rs COMMANDS",
            &names,
            build_names,
        ));
    }
    if idents != *handler_idents {
        problems.push(describe_difference(
            "generate_handler registrations",
            &idents,
            handler_idents,
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn describe_difference(
    list: &str,
    discovered: &BTreeSet<String>,
    listed: &BTreeSet<String>,
) -> String {
    let not_listed: Vec<&str> = discovered.difference(listed).map(String::as_str).collect();
    let not_discovered: Vec<&str> = listed.difference(discovered).map(String::as_str).collect();
    format!(
        "{list} differs from discovered commands; discovered but not in {list}: [{}]; in {list} but not discovered: [{}]",
        not_listed.join(", "),
        not_discovered.join(", ")
    )
}

fn render(commands: &[Command]) -> String {
    let mut out = String::from("[\n");
    for (index, command) in commands.iter().enumerate() {
        let args: Vec<String> = command.args.iter().map(|arg| json_string(arg)).collect();
        out.push_str("  {\n");
        out.push_str(&format!(
            "    \"command\": {},\n",
            json_string(&command.name)
        ));
        out.push_str(&format!("    \"args\": [{}]\n", args.join(", ")));
        out.push_str(if index + 1 == commands.len() {
            "  }\n"
        } else {
            "  },\n"
        });
    }
    out.push_str("]\n");
    out
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Command names whose lines differ between the committed and freshly rendered manifests.
fn stale_commands(committed: &str, rendered: &str) -> Vec<String> {
    let committed_lines: Vec<&str> = committed.lines().collect();
    let rendered_lines: Vec<&str> = rendered.lines().collect();
    let mut names = BTreeSet::new();
    for index in 0..committed_lines.len().max(rendered_lines.len()) {
        if committed_lines.get(index) == rendered_lines.get(index) {
            continue;
        }
        let name = nearest_command(&rendered_lines, index)
            .or_else(|| nearest_command(&committed_lines, index));
        names.insert(name.unwrap_or_else(|| "(header)".to_string()));
    }
    names.into_iter().collect()
}

fn nearest_command(lines: &[&str], index: usize) -> Option<String> {
    let end = index.saturating_add(1).min(lines.len());
    lines[..end].iter().rev().find_map(|line| {
        line.trim()
            .strip_prefix("\"command\": \"")
            .and_then(|rest| rest.strip_suffix("\","))
            .map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(source: &str) -> Result<Vec<Command>, String> {
        let file = syn::parse_file(source).map_err(|error| error.to_string())?;
        let mut walk = ModuleWalk {
            repo_root: PathBuf::new(),
            visited: BTreeSet::new(),
            commands: Vec::new(),
        };
        let scope = Scope::from_items(&file.items, &Scope::default());
        walk.collect("test.rs", &file.items, &scope, Path::new("."), None)?;
        Ok(walk.commands)
    }

    fn only(source: &str) -> Command {
        let mut found = commands(source).expect("source should classify");
        assert_eq!(found.len(), 1, "expected exactly one command");
        found.remove(0)
    }

    #[test]
    fn camel_case_keys_follow_heck_conversion() {
        let command = only(
            "#[tauri::command] fn f(conversation_id: String, _hidden: u8, page2_size: u8, x: u8) {}",
        );
        assert_eq!(command.args, ["conversationId", "hidden", "page2Size", "x"]);
    }

    #[test]
    fn snake_case_keeps_parameter_names() {
        let command =
            only("#[tauri::command(rename_all = \"snake_case\")] fn f(conversation_id: String) {}");
        assert_eq!(command.args, ["conversation_id"]);
    }

    #[test]
    fn explicit_camel_case_matches_default() {
        let command =
            only("#[tauri::command(rename_all = \"camelCase\")] fn f(device_id: String) {}");
        assert_eq!(command.args, ["deviceId"]);
    }

    #[test]
    fn rename_changes_the_command_name_not_the_function_ident() {
        let command = only("#[tauri::command(rename = \"custom_name\")] fn f_x() {}");
        assert_eq!(command.name, "custom_name");
        assert_eq!(command.ident, "f_x");
    }

    #[test]
    fn keyless_runtime_types_are_skipped_when_qualified() {
        let command = only(
            "#[tauri::command] fn f(state: tauri::State<'_, AppState>, request: tauri::ipc::Request<'_>, value: String) {}",
        );
        assert_eq!(command.args, ["value"]);
    }

    #[test]
    fn keyless_runtime_types_are_skipped_when_referenced() {
        let command = only(
            "use tauri::{AppHandle, State}; #[tauri::command] fn f(app: &AppHandle, window: &tauri::WebviewWindow, state: &State<'_, X>, value: u8) {}",
        );
        assert_eq!(command.args, ["value"]);
    }

    #[test]
    fn channel_parameters_are_keyed() {
        let command = only("#[tauri::command] fn f(on_event: Channel<Event>) {}");
        assert_eq!(command.args, ["onEvent"]);
    }

    #[test]
    fn wildcard_parameters_are_skipped() {
        let command = only("#[tauri::command] fn f(_: String, b: u8) {}");
        assert_eq!(command.args, ["b"]);
    }

    #[test]
    fn tuple_pattern_fails_closed() {
        let error = commands("#[tauri::command] fn f((a, b): (u8, u8)) {}").unwrap_err();
        assert!(error.contains("plain identifier"), "{error}");
    }

    #[test]
    fn impl_trait_type_fails_closed() {
        let error = commands("#[tauri::command] fn f(value: impl Into<String>) {}").unwrap_err();
        assert!(error.contains("impl Trait"), "{error}");
    }

    #[test]
    fn generic_parameter_type_fails_closed() {
        let error = commands("#[tauri::command] fn f<T>(value: T) {}").unwrap_err();
        assert!(error.contains("generic parameter"), "{error}");
    }

    #[test]
    fn unknown_attribute_argument_fails_closed() {
        let error = commands("#[tauri::command(root = \"crate\")] fn f() {}").unwrap_err();
        assert!(
            error.contains("unsupported tauri::command argument"),
            "{error}"
        );
    }

    #[test]
    fn commands_inside_nested_modules_are_found() {
        let found =
            commands("mod outer { pub mod inner { #[tauri::command] pub fn f(a_b: u8) {} } }")
                .expect("source should classify");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].args, ["aB"]);
    }

    #[test]
    fn bare_command_attribute_requires_the_tauri_import() {
        assert!(commands("#[command] fn f() {}").unwrap().is_empty());
        let imported = commands("use tauri::command; #[command] fn f(a: u8) {}").unwrap();
        assert_eq!(imported[0].args, ["a"]);
    }

    #[test]
    fn handler_entries_keep_full_paths_and_skip_macro_variables() {
        let paths = handler_paths_in(
            "fn x() { tauri::generate_handler![a::b::first, second, $($extra,)*] }",
        )
        .unwrap();
        assert_eq!(paths, ["a::b::first", "second"]);
    }

    fn command(name: &str, ident: &str) -> Command {
        Command {
            name: name.into(),
            ident: ident.into(),
            args: Vec::new(),
        }
    }

    fn set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn keyless_name_from_another_tauri_module_fails_closed() {
        let error =
            commands("use tauri::http::Request;\n#[tauri::command] fn f(r: Request<'_>) {}")
                .unwrap_err();
        assert!(error.contains("`Request`"), "{error}");
        let qualified =
            commands("#[tauri::command] fn f(r: tauri::http::Request<'_>) {}").unwrap_err();
        assert!(qualified.contains("`tauri::http::Request`"), "{qualified}");
    }

    #[test]
    fn canonical_ipc_scope_types_are_keyless() {
        let command = only(
            "#[tauri::command] fn f(s: tauri::ipc::GlobalScope<X>, c: tauri::ipc::CommandScope<X>, v: u8) {}",
        );
        assert_eq!(command.args, ["v"]);
    }

    #[test]
    fn unimported_keyless_name_fails_closed() {
        let error = commands("#[tauri::command] fn f(app: &AppHandle) {}").unwrap_err();
        assert!(error.contains("`AppHandle`"), "{error}");
    }

    #[test]
    fn aliased_tauri_keyless_type_stays_keyless() {
        let command = only("use tauri::State as S; #[tauri::command] fn f(s: S<'_, X>, v: u8) {}");
        assert_eq!(command.args, ["v"]);
    }

    #[test]
    fn module_alias_of_tauri_resolves_qualified_keyless_types() {
        let command = only("use tauri::ipc; #[tauri::command] fn f(r: ipc::Request<'_>, v: u8) {}");
        assert_eq!(command.args, ["v"]);
    }

    #[test]
    fn aliased_tauri_keyed_type_keeps_its_key() {
        let command =
            only("use tauri::ipc::Channel as C; #[tauri::command] fn f(on_event: C<Event>) {}");
        assert_eq!(command.args, ["onEvent"]);
    }

    #[test]
    fn local_dto_named_like_a_keyless_type_fails_closed() {
        let error = commands("struct Request { id: u8 }\n#[tauri::command] fn f(r: Request) {}")
            .unwrap_err();
        assert!(
            error.contains("does not resolve to a keyless tauri type"),
            "{error}"
        );
        assert!(error.starts_with("test.rs:2 f:"), "{error}");
    }

    #[test]
    fn keyless_name_imported_from_another_crate_fails_closed() {
        let error = commands("use crate::dto::State;\n#[tauri::command] fn f(s: State<'_, X>) {}")
            .unwrap_err();
        assert!(error.contains("`State`"), "{error}");
    }

    #[test]
    fn alias_of_another_crates_keyless_name_fails_closed() {
        let error =
            commands("use other::Window as W;\n#[tauri::command] fn f(w: W) {}").unwrap_err();
        assert!(
            error.contains("does not resolve to a keyless tauri type"),
            "{error}"
        );
    }

    #[test]
    fn glob_import_does_not_classify_keyless_names() {
        let error =
            commands("use tauri::*;\n#[tauri::command] fn f(s: State<'_, X>) {}").unwrap_err();
        assert!(
            error.contains("does not resolve to a keyless tauri type"),
            "{error}"
        );
    }

    #[test]
    fn child_module_does_not_inherit_parent_imports() {
        let error = commands(
            "use tauri::State;\nmod inner { #[tauri::command] pub fn f(s: State<'_, X>) {} }",
        )
        .unwrap_err();
        assert!(
            error.contains("does not resolve to a keyless tauri type"),
            "{error}"
        );
    }

    #[test]
    fn child_module_inherits_parent_imports_through_super_glob() {
        let command = only(
            "use tauri::State;\nmod inner { use super::*; #[tauri::command] pub fn f(s: State<'_, X>, v: u8) {} }",
        );
        assert_eq!(command.args, ["v"]);
    }

    #[test]
    fn other_glob_imports_do_not_provide_keyless_names() {
        let error = commands(
            "mod inner { use crate::shared::*; #[tauri::command] pub fn f(s: State<'_, X>) {} }",
        )
        .unwrap_err();
        assert!(
            error.contains("does not resolve to a keyless tauri type"),
            "{error}"
        );
    }

    #[test]
    fn mutable_and_raw_identifier_parameters_are_keyed() {
        let command = only("#[tauri::command] fn f(mut conversation_id: String, r#type: u8) {}");
        assert_eq!(command.args, ["conversationId", "type"]);
    }

    #[test]
    fn invalid_rename_all_fails_closed() {
        let error =
            commands("#[tauri::command(rename_all = \"kebab-case\")] fn f() {}").unwrap_err();
        assert!(error.contains("unsupported rename_all"), "{error}");
    }

    #[test]
    fn self_receiver_fails_closed() {
        let error = commands("#[tauri::command] fn f(self) {}").unwrap_err();
        assert!(error.contains("cannot take self"), "{error}");
    }

    #[test]
    fn cfg_gated_command_fails_closed() {
        let error =
            commands("#[cfg(target_os = \"macos\")]\n#[tauri::command] fn f() {}").unwrap_err();
        assert!(error.contains("#[cfg]"), "{error}");
    }

    #[test]
    fn cfg_attr_naming_a_command_fails_closed() {
        let error = commands("#[cfg_attr(unix, tauri::command)] fn f() {}").unwrap_err();
        assert!(error.contains("#[cfg]"), "{error}");
    }

    #[test]
    fn cfg_gated_module_with_commands_fails_closed() {
        let error =
            commands("#[cfg(unix)] mod platform { #[tauri::command] pub fn f() {} }").unwrap_err();
        assert!(error.contains("platform"), "{error}");
    }

    #[test]
    fn cfg_gated_helper_without_commands_is_accepted() {
        let found = commands("#[cfg(unix)] mod platform { pub fn helper() {} }").unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn handler_registered_twice_fails_closed() {
        let paths =
            handler_paths_in("fn x() { tauri::generate_handler![a::foo, a::foo] }").unwrap();
        let error = handler_idents(&paths).unwrap_err();
        assert!(error.contains("more than once"), "{error}");
    }

    #[test]
    fn distinct_handler_paths_with_one_last_segment_fail_closed() {
        let paths =
            handler_paths_in("fn x() { tauri::generate_handler![a::foo, b::foo] }").unwrap();
        let error = handler_idents(&paths).unwrap_err();
        assert!(error.contains("share the last segment `foo`"), "{error}");
    }

    #[test]
    fn distinct_handler_paths_collect_their_identifiers() {
        let paths =
            handler_paths_in("fn x() { tauri::generate_handler![a::foo, b::bar] }").unwrap();
        assert_eq!(handler_idents(&paths).unwrap(), set(&["bar", "foo"]));
    }

    #[test]
    fn cross_check_accepts_matching_lists() {
        let commands = [command("load", "load")];
        assert!(cross_check(&commands, &set(&["load"]), &set(&["load"])).is_ok());
    }

    #[test]
    fn cross_check_reports_names_missing_from_build_rs() {
        let commands = [command("load", "load"), command("extra", "extra")];
        let error = cross_check(&commands, &set(&["load"]), &set(&["load", "extra"])).unwrap_err();
        assert!(
            error.contains("discovered but not in build.rs COMMANDS: [extra]"),
            "{error}"
        );
    }

    #[test]
    fn cross_check_reports_build_rs_names_without_a_command() {
        let commands = [command("load", "load")];
        let error = cross_check(&commands, &set(&["load", "gone"]), &set(&["load"])).unwrap_err();
        assert!(
            error.contains("in build.rs COMMANDS but not discovered: [gone]"),
            "{error}"
        );
    }

    #[test]
    fn cross_check_reports_handler_registration_differences_both_ways() {
        let commands = [command("load", "load")];
        let error = cross_check(&commands, &set(&["load"]), &set(&["other"])).unwrap_err();
        assert!(
            error.contains("discovered but not in generate_handler registrations: [load]"),
            "{error}"
        );
        assert!(
            error.contains("in generate_handler registrations but not discovered: [other]"),
            "{error}"
        );
    }

    #[test]
    fn golden_read_failure_is_an_error_not_staleness() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let error = compare_golden(Err(denied), "[]\n", Path::new("golden.json")).unwrap_err();
        assert!(error.contains("cannot read golden.json"), "{error}");
    }

    #[test]
    fn missing_golden_is_reported_as_missing() {
        let missing = io::Error::from(io::ErrorKind::NotFound);
        let state = compare_golden(Err(missing), "[]\n", Path::new("golden.json")).unwrap();
        assert_eq!(state, Golden::Missing);
    }

    #[test]
    fn golden_comparison_distinguishes_current_and_stale() {
        let current = compare_golden(Ok("[]\n".into()), "[]\n", Path::new("g")).unwrap();
        assert_eq!(current, Golden::Current);
        let stale = compare_golden(Ok("[\n]\n".into()), "[]\n", Path::new("g")).unwrap();
        assert!(matches!(stale, Golden::Stale(_)));
    }
}
