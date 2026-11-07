//! Guards the webview↔backend command contract.
//!
//! The React frontend reaches the engine only through typed Tauri commands. A name the frontend
//! passes to `invoke` that is absent from `tauri::generate_handler!` is not a compile error: the
//! call is rejected at runtime, so the screen shows a failed button. The frontend is not compiled
//! as part of this test, so both sides are read as source text and compared.
//!
//! The scan is deliberately textual rather than a TypeScript parse: it strips comments, finds
//! every `invoke` call (with optional generic arguments), and resolves the first argument when it
//! is a direct literal, a ternary over literals, or an identifier declared with a literal union
//! type or a local `const`. Anything else falls back to the literals inside the expression, which
//! errs towards reporting a name instead of silently skipping a call.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Commands registered for the webview that no `ui/src` module invokes.
///
/// Most entries are the no-dialog half of a pair whose sibling the webview uses
/// (`projects_choose_folder` registers the folder that `projects_add` would receive as a path),
/// or a shell path that runs before the webview renders. Keeping a registration is a
/// command-surface decision, so removal is never automatic; each entry records why it stays.
/// The list is asserted to stay honest: an entry that becomes invoked, or stops being
/// registered, fails the test instead of rotting quietly.
const FRONTEND_UNUSED_COMMANDS: &[(&str, &str)] = &[
    (
        "config_report",
        "release-only effective-config readout with no caller; the webview's Doctor flow \
         uses `diagnostics_report_copy`",
    ),
    (
        "global_store_overview",
        "no-dialog Global store query; the webview reads the same state from `overview_load`",
    ),
    (
        "global_enable_writes",
        "no-dialog Global write authorization; the webview uses `overview_enable_global_writes`",
    ),
    (
        "jobs_list",
        "job inventory with no caller; the webview tracks the ids of the jobs it started",
    ),
    (
        "migrate_scan",
        "synchronous scan with no caller; the webview uses `migrate_scan_start` and \
         `migrate_scan_result`",
    ),
    (
        "network_events",
        "the last events already travel inside `NetworkPolicyView`, so the webview never \
         reads this separate list",
    ),
    (
        "projects_add",
        "registration by path with no caller; the webview uses `projects_choose_folder`",
    ),
    (
        "window_state_load",
        "window geometry is read by the shell's setup restore, before the webview renders",
    ),
    (
        "window_state_save",
        "window geometry is written by the shell's own resize and move handlers",
    ),
];

#[test]
fn every_frontend_command_is_registered_in_both_handler_lists() {
    let lists = handler_lists();
    let invoked = frontend_commands();

    assert!(
        !invoked.is_empty(),
        "the frontend scan found no invoke targets; the scanner or the UI tree moved"
    );

    let mut missing = Vec::new();
    for name in &invoked {
        if !lists.debug.contains(name) {
            missing.push(format!(
                "`{name}` is invoked from ui/src but missing from the debug handler list"
            ));
        }
        if !lists.release.contains(name) {
            missing.push(format!(
                "`{name}` is invoked from ui/src but missing from the release handler list"
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "frontend commands that the backend does not register (the call is rejected at runtime):\n{}",
        missing.join("\n")
    );
}

#[test]
fn registered_commands_are_either_invoked_or_documented_as_frontend_unused() {
    let lists = handler_lists();
    let invoked = frontend_commands();
    let documented: BTreeMap<&str, &str> = FRONTEND_UNUSED_COMMANDS.iter().copied().collect();

    let mut undocumented = Vec::new();
    for name in &lists.all {
        if !invoked.contains(name) && !documented.contains_key(name.as_str()) {
            undocumented.push(format!(
                "`{name}` is registered but never invoked from ui/src; invoke it, remove the \
                 registration, or add it to FRONTEND_UNUSED_COMMANDS with a reason"
            ));
        }
    }
    assert!(
        undocumented.is_empty(),
        "registered commands with no webview caller:\n{}",
        undocumented.join("\n")
    );

    let mut stale = Vec::new();
    for (name, _reason) in FRONTEND_UNUSED_COMMANDS {
        if !lists.all.contains(*name) {
            stale.push(format!("`{name}` is no longer registered"));
        } else if invoked.contains(*name) {
            stale.push(format!(
                "`{name}` is invoked now, so it is not frontend-unused"
            ));
        }
    }
    assert!(
        stale.is_empty(),
        "FRONTEND_UNUSED_COMMANDS has entries that no longer describe the code:\n{}",
        stale.join("\n")
    );
}

/// Both `tauri::generate_handler!` lists, split by the `cfg` that guards them.
struct HandlerLists {
    debug: BTreeSet<String>,
    release: BTreeSet<String>,
    all: BTreeSet<String>,
}

fn handler_lists() -> HandlerLists {
    let path = source_root().join("crates/at-app/src/app/mod.rs");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));

    let mut debug = BTreeSet::new();
    let mut release = BTreeSet::new();
    let mut found = 0_usize;
    for start in find_all(&source, "tauri::generate_handler!") {
        let open = find_from(&source, start, '[').unwrap_or_else(|| {
            panic!("handler list at byte {start} has no `[` after generate_handler!")
        });
        let body = bracket_body(&source, open)
            .unwrap_or_else(|| panic!("handler list at byte {start} is not terminated"));

        let is_debug = match enclosing_cfg(&source, start) {
            Some(cfg) if cfg.contains("debug_assertions") && !cfg.contains("not(") => true,
            Some(cfg) if cfg.contains("not(debug_assertions)") => false,
            other => panic!(
                "handler list at byte {start} is not guarded by a debug/release cfg (found {other:?})"
            ),
        };

        let target = if is_debug { &mut debug } else { &mut release };
        for entry in body.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let name = entry.rsplit("::").next().unwrap_or(entry).trim().to_owned();
            assert!(
                is_identifier(&name),
                "handler entry `{entry}` does not end in a command name"
            );
            target.insert(name);
        }
        found += 1;
    }

    assert_eq!(
        found, 2,
        "expected a debug and a release handler list, found {found}"
    );
    let all = debug.union(&release).cloned().collect();
    HandlerLists {
        debug,
        release,
        all,
    }
}

/// Every command name the frontend passes to `invoke`, across the whole `ui/src` tree.
fn frontend_commands() -> BTreeSet<String> {
    let mut files = Vec::new();
    collect_sources(&source_root().join("ui/src"), &mut files);

    let mut commands = BTreeSet::new();
    for file in files {
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", file.display()));
        let stripped = strip_comments(&source);
        commands.extend(invoke_targets(&stripped));
    }
    commands
}

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("could not list {}: {error}", dir.display()));
    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_sources(&path, out);
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("ts" | "tsx")
        ) {
            out.push(path);
        }
    }
}

/// Removes `//` and `/* */` comments while leaving string and template contents untouched.
fn strip_comments(source: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Code,
        Line,
        Block,
        Double,
        Single,
        Template,
    }

    let mut out = String::with_capacity(source.len());
    let mut state = State::Code;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match state {
            State::Code => match c {
                '/' => match chars.peek() {
                    Some('/') => {
                        chars.next();
                        state = State::Line;
                    }
                    Some('*') => {
                        chars.next();
                        state = State::Block;
                    }
                    _ => out.push(c),
                },
                '"' => {
                    state = State::Double;
                    out.push(c);
                }
                '\'' => {
                    state = State::Single;
                    out.push(c);
                }
                '`' => {
                    state = State::Template;
                    out.push(c);
                }
                _ => out.push(c),
            },
            State::Line => {
                if c == '\n' {
                    out.push('\n');
                    state = State::Code;
                }
            }
            State::Block => {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    state = State::Code;
                } else if c == '\n' {
                    out.push('\n');
                }
            }
            State::Double | State::Single | State::Template => {
                out.push(c);
                if c == '\\' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if (state == State::Double && c == '"')
                    || (state == State::Single && c == '\'')
                    || (state == State::Template && c == '`')
                {
                    state = State::Code;
                }
            }
        }
    }
    out
}

/// Command names passed as the first argument of every `invoke` call in one file.
fn invoke_targets(source: &str) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    let mut cursor = 0;
    while let Some(start) = find_identifier(source, "invoke", cursor) {
        cursor = start + "invoke".len();

        let mut next = skip_space(source, cursor);
        if source[next..].starts_with('<') {
            match skip_angle_group(source, next) {
                Some(end) => next = skip_space(source, end),
                None => continue,
            }
        }
        if !source[next..].starts_with('(') {
            continue;
        }
        let Some(argument) = first_argument(source, next) else {
            continue;
        };
        targets.extend(command_names(source, argument.trim()));
    }
    targets
}

/// The command names one `invoke` argument can carry.
fn command_names(source: &str, argument: &str) -> BTreeSet<String> {
    if argument.is_empty() {
        return BTreeSet::new();
    }
    if let Some(literal) = string_literal(argument) {
        return BTreeSet::from([literal]);
    }
    if let Some((_condition, then_branch, else_branch)) = split_ternary(argument) {
        let mut names: BTreeSet<String> = string_literals(then_branch).into_iter().collect();
        names.extend(string_literals(else_branch));
        return names;
    }
    if is_identifier(argument) {
        let resolved = resolve_identifier(source, argument);
        if !resolved.is_empty() {
            return resolved;
        }
    }
    string_literals(argument).into_iter().collect()
}

/// Literal-union type annotations, type aliases and `const` initializers bound to `name`.
fn resolve_identifier(source: &str, name: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let aliases = type_aliases(source);

    for start in find_all_identifier(source, name) {
        let mut cursor = skip_space(source, start + name.len());
        if !source[cursor..].starts_with(':') {
            continue;
        }
        cursor = skip_space(source, cursor + 1);
        let annotation = annotation_text(source, cursor);
        if let Some(union) = union_literals(annotation) {
            names.extend(union);
        } else if let Some(alias) = aliases.get(annotation.trim()) {
            names.extend(alias.iter().cloned());
        }
    }

    for start in find_all_identifier(source, "const") {
        let after_const = skip_space(source, start + "const".len());
        if find_identifier(source, name, after_const) != Some(after_const) {
            continue;
        }
        let mut cursor = skip_space(source, after_const + name.len());
        if source[cursor..].starts_with(':') {
            cursor = skip_space(
                source,
                skip_annotation(source, skip_space(source, cursor + 1)),
            );
        }
        if !source[cursor..].starts_with('=') {
            continue;
        }
        let initializer = statement_text(source, cursor + 1);
        if let Some((_condition, then_branch, else_branch)) = split_ternary(initializer) {
            names.extend(string_literals(then_branch));
            names.extend(string_literals(else_branch));
        } else if let Some(literal) = string_literal(initializer.trim()) {
            names.insert(literal);
        }
    }

    names
}

/// `type Name = "a" | "b";` declarations in one file.
fn type_aliases(source: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut aliases = BTreeMap::new();
    for start in find_all_identifier(source, "type") {
        let name_start = skip_space(source, start + "type".len());
        let Some(name_end) = identifier_end(source, name_start) else {
            continue;
        };
        let mut cursor = skip_space(source, name_end);
        if !source[cursor..].starts_with('=') {
            continue;
        }
        cursor = skip_space(source, cursor + 1);
        let body = statement_text(source, cursor);
        if let Some(union) = union_literals(body) {
            aliases.insert(source[name_start..name_end].to_owned(), union);
        }
    }
    aliases
}

/// Parses `"a" | "b" | "c"` into its literals, or `None` when the text is not such a union.
fn union_literals(text: &str) -> Option<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for part in split_top_level(text, '|') {
        names.insert(string_literal(part.trim())?);
    }
    (!names.is_empty()).then_some(names)
}

/// The type annotation that starts at `start`, stopping at a parameter or statement boundary.
fn annotation_text(source: &str, start: usize) -> &str {
    let mut depth = 0_i32;
    let mut index = start;
    let mut chars = source[start..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' | '\'' | '`' => skip_string(&mut chars, c),
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => {
                if depth == 0 {
                    index = start + offset;
                    break;
                }
                depth -= 1;
            }
            ',' | ';' | '=' | '\n' if depth == 0 => {
                index = start + offset;
                break;
            }
            _ => {}
        }
        index = start + offset + c.len_utf8();
    }
    &source[start..index]
}

/// Byte offset after a type annotation that starts at `start`.
fn skip_annotation(source: &str, start: usize) -> usize {
    start + annotation_text(source, start).len()
}

/// The statement or initializer that starts at `start`, stopping at a top-level `;` or newline.
fn statement_text(source: &str, start: usize) -> &str {
    let mut depth = 0_i32;
    let mut index = source.len();
    let mut chars = source[start..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' | '\'' | '`' => skip_string(&mut chars, c),
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ';' if depth == 0 => {
                index = start + offset;
                break;
            }
            _ => {}
        }
        index = start + offset + c.len_utf8();
    }
    &source[start..index]
}

fn skip_string(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>, quote: char) {
    while let Some((_, c)) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if c == quote {
            return;
        }
    }
}

/// Splits `condition ? a : b` at its top-level `?` and `:`.
fn split_ternary(expression: &str) -> Option<(&str, &str, &str)> {
    let mut depth = 0_i32;
    let mut question = None;
    let mut colon = None;
    let mut chars = expression.char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' | '\'' | '`' => skip_string(&mut chars, c),
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            '?' if depth == 0 && question.is_none() => question = Some(offset),
            ':' if depth == 0 && question.is_some() && colon.is_none() => colon = Some(offset),
            _ => {}
        }
    }
    let (question, colon) = (question?, colon?);
    Some((
        &expression[..question],
        &expression[question + 1..colon],
        &expression[colon + 1..],
    ))
}

fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' | '\'' | '`' => skip_string(&mut chars, c),
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            c if c == separator && depth == 0 => {
                parts.push(&text[start..offset]);
                start = offset + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// The source of the first argument of the call whose `(` is at `open`.
fn first_argument(source: &str, open: usize) -> Option<&str> {
    let start = skip_space(source, open + 1);
    let mut depth = 0_i32;
    let mut chars = source[start..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' | '\'' | '`' => skip_string(&mut chars, c),
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => {
                if depth == 0 {
                    return Some(&source[start..start + offset]);
                }
                depth -= 1;
            }
            ',' if depth == 0 => return Some(&source[start..start + offset]),
            _ => {}
        }
    }
    Some(&source[start..])
}

/// The literal contents when `text` is exactly one quoted string.
fn string_literal(text: &str) -> Option<String> {
    let mut chars = text.char_indices();
    let (_, quote) = chars.next()?;
    if !matches!(quote, '"' | '\'' | '`') {
        return None;
    }
    let body = &text[quote.len_utf8()..];
    let closing = body.find(quote)?;
    if closing + quote.len_utf8() != body.len() {
        return None;
    }
    Some(body[..closing].to_owned())
}

fn string_literals(text: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        if !matches!(c, '"' | '\'' | '`') {
            continue;
        }
        let body_start = offset + c.len_utf8();
        let mut end = None;
        while let Some((inner_offset, inner)) = chars.next() {
            if inner == '\\' {
                chars.next();
            } else if inner == c {
                end = Some(inner_offset);
                break;
            }
        }
        if let Some(end) = end {
            literals.push(text[body_start..end].to_owned());
        }
    }
    literals
}

fn is_identifier(text: &str) -> bool {
    identifier_end(text, 0) == Some(text.len())
}

fn identifier_end(text: &str, start: usize) -> Option<usize> {
    let mut end = None;
    for (offset, c) in text[start..].char_indices() {
        let boundary = !(c.is_alphanumeric() || c == '_' || c == '$');
        if boundary {
            end = Some(start + offset);
            break;
        }
    }
    let end = end.unwrap_or(text.len());
    (end > start).then_some(end)
}

/// Byte offset of the next whole-word `needle` at or after `from`.
fn find_identifier(text: &str, needle: &str, from: usize) -> Option<usize> {
    let mut cursor = from;
    while let Some(offset) = text[cursor..].find(needle).map(|found| cursor + found) {
        let before_ok = text[..offset]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'));
        let after = offset + needle.len();
        let after_ok = text[after..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'));
        if before_ok && after_ok {
            return Some(offset);
        }
        cursor = after;
    }
    None
}

fn find_all_identifier(text: &str, needle: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = find_identifier(text, needle, cursor) {
        found.push(offset);
        cursor = offset + needle.len();
    }
    found
}

fn find_all(text: &str, needle: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find(needle).map(|hit| cursor + hit) {
        found.push(offset);
        cursor = offset + needle.len();
    }
    found
}

fn find_from(text: &str, from: usize, needle: char) -> Option<usize> {
    text[from..]
        .char_indices()
        .find(|(_, c)| *c == needle)
        .map(|(offset, _)| from + offset)
}

/// The body of the `[...]` whose opening bracket is at `open`.
fn bracket_body(source: &str, open: usize) -> Option<&str> {
    let mut depth = 0_i32;
    let mut chars = source[open..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' | '\'' | '`' => skip_string(&mut chars, c),
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The `cfg` predicate text of the nearest `#[cfg(...)]` attribute before `before`.
fn enclosing_cfg(source: &str, before: usize) -> Option<&str> {
    let prefix = &source[..before];
    let start = prefix.rfind("#[cfg(")?;
    let body_start = start + "#[cfg(".len();
    let mut depth = 1_i32;
    for (offset, c) in source[body_start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[body_start..body_start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn skip_space(source: &str, from: usize) -> usize {
    from + source[from..]
        .len()
        .saturating_sub(source[from..].trim_start().len())
}

/// Byte offset just after the `>` that closes the generic argument starting at `open`.
fn skip_angle_group(source: &str, open: usize) -> Option<usize> {
    let mut depth = 0_i32;
    let mut chars = source[open..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' | '\'' | '`' => skip_string(&mut chars, c),
            '=' if chars.peek().is_some_and(|(_, next)| *next == '>') => {
                chars.next();
            }
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset + c.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn every_frontend_invoke_uses_the_commands_parameter_names() {
    // Tauri 2 renames a command's Rust parameters to camelCase on the wire. Passing the Rust
    // spelling (`job_id` instead of `jobId`) is not a compile error: the command rejects with
    // "invalid args" at runtime, and a caller that treats every rejection as "not ready yet"
    // reports its own invented failure. The migration apply did exactly that: the files were
    // written while the screen claimed the apply had timed out.
    let parameters = command_parameters();
    let mut files = Vec::new();
    collect_sources(&source_root().join("ui/src"), &mut files);

    let mut problems = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", file.display()));
        let stripped = strip_comments(&source);
        let mut cursor = 0;
        while let Some(start) = find_identifier(&stripped, "invoke", cursor) {
            cursor = start + "invoke".len();
            let mut next = skip_space(&stripped, cursor);
            if stripped[next..].starts_with('<') {
                match skip_angle_group(&stripped, next) {
                    Some(end) => next = skip_space(&stripped, end),
                    None => continue,
                }
            }
            if !stripped[next..].starts_with('(') {
                continue;
            }
            let Some(arguments) = first_argument(&stripped, next) else {
                continue;
            };
            let parts = split_top_level(arguments, ',');
            if parts.len() < 2 {
                continue;
            }
            let Some(keys) = object_keys(parts[1]) else {
                continue;
            };
            for name in command_names(&stripped, parts[0].trim()) {
                let Some(expected) = parameters.get(&name) else {
                    continue;
                };
                for key in &keys {
                    if !expected.iter().any(|candidate| candidate == key) {
                        problems.push(format!(
                            "{}: `invoke(\"{name}\")` passes `{key}`, but Tauri expects {}",
                            file.display(),
                            expected.join(", ")
                        ));
                    }
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "frontend command arguments the backend cannot deserialize:\n{}",
        problems.join("\n")
    );
}

/// Command name → the camelCase parameter names the webview must send.
///
/// Commands that opt out with `rename_all` are omitted: their wire spelling is their own.
fn command_parameters() -> BTreeMap<String, Vec<String>> {
    let mut files = Vec::new();
    collect_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );

    let mut parameters = BTreeMap::new();
    for file in files {
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", file.display()));
        let stripped = strip_comments(&source);
        let mut search = 0;
        while let Some(attribute) = find_from(&stripped, search, '#') {
            if !stripped[attribute..].starts_with("#[tauri::command") {
                search = attribute + 1;
                continue;
            }
            let Some(attribute_end) = find_from(&stripped, attribute, ']') else {
                break;
            };
            search = attribute_end + 1;
            let attribute_text = &stripped[attribute..=attribute_end];
            let Some(fn_at) = find_identifier(&stripped, "fn", attribute_end) else {
                break;
            };
            let name_start = skip_space(&stripped, fn_at + "fn".len());
            let Some(name_end) = identifier_end(&stripped, name_start) else {
                continue;
            };
            let name = stripped[name_start..name_end].to_owned();
            let Some(open) = find_from(&stripped, name_end, '(') else {
                continue;
            };
            let Some(declared) = first_argument(&stripped, open) else {
                continue;
            };
            if attribute_text.contains("rename_all") {
                continue;
            }
            let mut names = Vec::new();
            for parameter in split_top_level(declared, ',') {
                let Some((parameter_name, parameter_type)) = parameter.split_once(':') else {
                    continue;
                };
                let parameter_name = parameter_name.trim();
                if parameter_name.is_empty() || parameter_name == "self" {
                    continue;
                }
                if ["State", "AppHandle", "Window", "CommandScope"]
                    .iter()
                    .any(|marker| parameter_type.contains(marker))
                {
                    continue;
                }
                names.push(camel_case(parameter_name));
            }
            parameters.insert(name, names);
        }
    }
    parameters
}

/// `job_id` → `jobId`.
fn camel_case(name: &str) -> String {
    let mut parts = name.split('_');
    let mut camel = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let mut characters = part.chars();
        if let Some(first) = characters.next() {
            camel.extend(first.to_uppercase());
            camel.extend(characters);
        }
    }
    camel
}

/// Every top-level key of a `{...}` literal.
///
/// A shorthand property (`{ previewId }`) sends the variable's own name as the key, so it is
/// checked like `{ previewId: previewId }`; that is how the bulk discard passed the Rust spelling
/// `preview_id` unnoticed. `None` when the value is not a plain object literal this check can
/// judge (a spread, a quoted key, a call or a non-object expression), so the test never guesses.
fn object_keys(text: &str) -> Option<Vec<String>> {
    let text = text.trim();
    let body = text.strip_prefix('{')?.strip_suffix('}')?;
    let mut keys = Vec::new();
    for part in split_top_level(body, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part.starts_with("...") || part.contains("=>") {
            return None;
        }
        let key = match part.split_once(':') {
            Some((key, _)) => key.trim(),
            None => part,
        };
        if !is_identifier(key) {
            return None;
        }
        keys.push(key.to_owned());
    }
    Some(keys)
}

#[test]
fn shorthand_properties_are_checked_as_argument_keys() {
    // `invoke("x", { preview_id })` sends `preview_id`, not `previewId`; before this was handled
    // the bulk discard passed the Rust spelling and the guard skipped the call entirely.
    assert_eq!(
        object_keys("{ preview_id }"),
        Some(vec!["preview_id".to_owned()])
    );
    assert_eq!(
        object_keys("{ previewId, confirmed }"),
        Some(vec!["previewId".to_owned(), "confirmed".to_owned()])
    );
    assert_eq!(object_keys("{ ...rest }"), None);
    assert_eq!(object_keys("{ \"quoted-key\": 1 }"), None);
}
