//! Generative robustness tests for the format-preserving MCP editors.
//!
//! Every test is deterministic. A fixed list of seeds drives the shared
//! `at_fs::testkit::generative::Rng`, and each assertion message carries its seed, so a
//! failing case is replayed by rerunning the test unchanged.
//!
//! The documents are built with sentinels in every unrelated value and comment. Each edit is
//! checked two ways: the common prefix/suffix may not cross the edited entry (only separators
//! — commas, spaces, tabs, newlines — may move), and every unrelated sentinel must survive
//! with the same count.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use at_adapters::claude_mcp::{
    ClaudeConfigScope, ClaudeMcpAdapter, ClaudeMcpConfig, ClaudeMcpError, ClaudeServer,
};
use at_adapters::codex_mcp::{CodexConfigFile, CodexError, CodexServerEntry};
use at_adapters::omp_mcp::{OmpMcpAdapter, OmpMcpConfig, OmpMcpError, OmpServer};
use at_fs::testkit::generative::Rng;
use at_model::mcp::{EnvValue, McpTransport, SecretRef};
use serde_json::{Map, Value};
use tempfile::TempDir;
use url::Url;

/// Seeds per property. Each seed also runs every operation and format, so the real case
/// counts are higher than this number.
const CASES: u64 = 320;

/// The part of a document one edit may change.
///
/// `start` is where byte identity must resume at the latest. `content_start..content_end` is
/// the edited entry itself; bytes between the entry and the surrounding untouched text may
/// only be separators (commas and whitespace).
#[derive(Debug, Clone, Copy)]
struct Aperture {
    content_start: usize,
    content_end: usize,
}

impl Aperture {
    fn insertion(anchor: usize) -> Self {
        Self {
            content_start: anchor,
            content_end: anchor,
        }
    }
}

/// Asserts that `after` differs from `before` only inside `aperture`, and that no sentinel
/// from unrelated content changed its occurrence count.
///
/// A line diff is used instead of a byte prefix/suffix: unrelated server tables share the
/// `[mcp_servers.` header text, so byte alignment can attribute that shared text to the edit
/// even though it is reconstructed unchanged. Lines are compared whole; a line outside the
/// entry may only change its separators (commas, whitespace), and a line may only be removed
/// when it is blank.
fn assert_confined(
    what: &str,
    seed: u64,
    before: &str,
    after: &str,
    aperture: Aperture,
    sentinels: &[String],
    separators: fn(u8) -> bool,
) {
    assert_ne!(
        before, after,
        "{what} seed={seed}: the edit changed nothing"
    );
    for sentinel in sentinels {
        assert_eq!(
            after.matches(sentinel).count(),
            before.matches(sentinel).count(),
            "{what} seed={seed}: unrelated sentinel {sentinel} changed count\nbefore: {before:?}\nafter: {after:?}"
        );
    }
    let line_index = |offset: usize| {
        before.as_bytes()[..offset.min(before.len())]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
    };
    let first = line_index(aperture.content_start);
    let last = line_index(aperture.content_end.saturating_sub(1));
    let old_lines: Vec<&str> = before.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = after.split_inclusive('\n').collect();
    let strip = |line: &str| -> String {
        line.bytes()
            .filter(|byte| !separators(*byte))
            .map(char::from)
            .collect()
    };
    let diagnostic = || {
        format!(
            "seed={seed} aperture={aperture:?} lines={first}..={last} before_lines={} after_lines={}",
            old_lines.len(),
            new_lines.len()
        )
    };
    let diff = similar::TextDiff::from_lines(before, after);
    for op in diff.ops() {
        match *op {
            similar::DiffOp::Equal { .. } => {}
            similar::DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => {
                assert!(new_len > 0);
                assert!(
                    new_lines[new_index..new_index + new_len]
                        .iter()
                        .any(|line| !line.trim().is_empty()),
                    "{what}: insertion added only blank lines\n{}",
                    diagnostic()
                );
                let is_insertion = aperture.content_start == aperture.content_end;
                if is_insertion {
                    // A new entry may follow the anchor line after its blank separator lines.
                    assert!(
                        old_index >= last,
                        "{what}: insertion at old line {old_index} is before the anchor line {last}\n{}",
                        diagnostic()
                    );
                    if old_index > last {
                        assert!(
                            old_lines[last + 1..old_index]
                                .iter()
                                .all(|line| line.trim().is_empty()),
                            "{what}: insertion at old line {old_index} skipped unrelated content after line {last}\n{}",
                            diagnostic()
                        );
                    }
                } else if old_index < first || old_index > last + 1 {
                    // Myers may re-emit a separator-only line change as delete + insert.
                    let inserted: String = new_lines[new_index..new_index + new_len]
                        .iter()
                        .map(|line| strip(line))
                        .collect();
                    let neighbour = |index: usize| {
                        old_lines
                            .get(index)
                            .map_or_else(String::new, |line| strip(line))
                    };
                    let previous = old_index.checked_sub(1).map_or_else(String::new, neighbour);
                    let at = neighbour(old_index);
                    assert!(
                        inserted == previous || inserted == at,
                        "{what}: insertion at old line {old_index} is outside the entry lines {first}..={last}\n{}",
                        diagnostic()
                    );
                }
            }
            similar::DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => {
                let reemitted = new_lines.get(new_index).map(|line| strip(line));
                for (index, line) in old_lines.iter().enumerate().skip(old_index).take(old_len) {
                    if index < first || index > last {
                        assert!(
                            line.trim().is_empty()
                                || reemitted.as_deref() == Some(strip(line).as_str()),
                            "{what}: deleted unrelated line {index}: {:?}\n{}",
                            line,
                            diagnostic()
                        );
                    }
                }
            }
            similar::DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                if old_len == new_len {
                    for offset in 0..old_len {
                        let index = old_index + offset;
                        if index < first || index > last {
                            assert_eq!(
                                strip(old_lines[index]),
                                strip(new_lines[new_index + offset]),
                                "{what}: unrelated line {index} changed beyond separators\n{}",
                                diagnostic()
                            );
                        }
                    }
                } else {
                    // A block resize may absorb an adjacent separator-only line (for example
                    // the previous entry losing its trailing comma). Every unrelated line in
                    // the window must still reappear, in order, inside the new window.
                    let old_outside: Vec<String> = old_lines[old_index..old_index + old_len]
                        .iter()
                        .enumerate()
                        .filter(|(offset, _)| {
                            let index = old_index + offset;
                            index < first || index > last
                        })
                        .map(|(_, line)| strip(line))
                        .filter(|line| !line.is_empty())
                        .collect();
                    let new_tokens: Vec<String> = new_lines[new_index..new_index + new_len]
                        .iter()
                        .map(|line| strip(line))
                        .filter(|line| !line.is_empty())
                        .collect();
                    let mut remaining = new_tokens.iter();
                    for token in &old_outside {
                        assert!(
                            remaining.any(|candidate| candidate == token),
                            "{what}: unrelated line content {token:?} disappeared in the rewritten window\n{}",
                            diagnostic()
                        );
                    }
                }
            }
        }
    }
}

/// A cheaper check for the multi-megabyte case: `after` inserts at `anchor` and nowhere else.
fn assert_insertion_only(
    what: &str,
    before: &str,
    after: &str,
    anchor: usize,
    sentinels: &[String],
) {
    assert!(
        after.starts_with(&before[..anchor]),
        "{what}: bytes before the insertion anchor changed"
    );
    assert!(
        after.ends_with(&before[anchor..]),
        "{what}: bytes after the insertion anchor changed"
    );
    assert!(after.len() > before.len(), "{what}: nothing was inserted");
    for sentinel in sentinels {
        assert_eq!(
            after.matches(sentinel).count(),
            before.matches(sentinel).count(),
            "{what}: sentinel {sentinel} changed count"
        );
    }
}

fn is_json_separator(byte: u8) -> bool {
    matches!(byte, b',' | b' ' | b'\t' | b'\r' | b'\n')
}

fn is_toml_separator(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("JSON string")
}

fn line_ending(rng: &mut Rng) -> &'static str {
    if rng.chance(4) { "\r\n" } else { "\n" }
}

fn indent_unit(rng: &mut Rng) -> &'static str {
    ["  ", "    ", "\t"][rng.below(3)]
}

/// Renders a JSON value with caller-chosen indentation instead of serde_json's fixed layout.
fn render_json_value(value: &Value, unit: &str, nl: &str, level: usize) -> String {
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                return String::from("{}");
            }
            let inner = unit.repeat(level + 1);
            let close = unit.repeat(level);
            let mut text = String::from("{");
            text.push_str(nl);
            let last = map.len() - 1;
            for (index, (key, item)) in map.iter().enumerate() {
                text.push_str(&inner);
                text.push_str(&json_string(key));
                text.push_str(": ");
                text.push_str(&render_json_value(item, unit, nl, level + 1));
                if index < last {
                    text.push(',');
                }
                text.push_str(nl);
            }
            text.push_str(&close);
            text.push('}');
            text
        }
        Value::Array(items) => {
            if items.is_empty() {
                return String::from("[]");
            }
            let inner = unit.repeat(level + 1);
            let close = unit.repeat(level);
            let mut text = String::from("[");
            text.push_str(nl);
            let last = items.len() - 1;
            for (index, item) in items.iter().enumerate() {
                text.push_str(&inner);
                text.push_str(&render_json_value(item, unit, nl, level + 1));
                if index < last {
                    text.push(',');
                }
                text.push_str(nl);
            }
            text.push_str(&close);
            text.push(']');
            text
        }
        scalar => scalar.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Oper {
    Add,
    Replace,
    Remove,
}

impl Oper {
    fn label(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Replace => "replace",
            Self::Remove => "remove",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonFlavor {
    Claude,
    Omp,
}

/// One MCP server definition, shared by the JSON and TOML builders.
#[derive(Debug, Clone)]
struct Spec {
    name: String,
    stdio: bool,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    url: Option<String>,
    headers: BTreeMap<String, String>,
    enabled: bool,
    extra: BTreeMap<String, Value>,
}

impl Spec {
    fn transport(&self) -> McpTransport {
        if self.stdio {
            McpTransport::Stdio {
                command: self.command.clone(),
                args: self.args.clone(),
                cwd: self.cwd.clone(),
                env: self
                    .env
                    .iter()
                    .map(|(key, value)| (key.clone(), EnvValue::Literal(value.clone())))
                    .collect(),
            }
        } else {
            McpTransport::Http {
                url: Url::parse(self.url.as_deref().unwrap_or("https://example.invalid/"))
                    .expect("generated URL"),
                headers: self
                    .headers
                    .iter()
                    .map(|(key, value)| (key.clone(), EnvValue::Literal(value.clone())))
                    .collect(),
                oauth: None,
            }
        }
    }

    fn to_claude(&self) -> ClaudeServer {
        ClaudeServer {
            name: self.name.clone(),
            transport: self.transport(),
            extra: self.extra.clone(),
        }
    }

    fn to_omp(&self) -> OmpServer {
        OmpServer {
            name: self.name.clone(),
            transport: self.transport(),
            enabled: self.enabled,
            extra: self.extra.clone(),
        }
    }

    fn desired(&self) -> Self {
        let mut spec = self.clone();
        if spec.stdio {
            spec.command.push_str("-patched");
            spec.args.push(format!("--changed-{}", spec.args.len()));
        } else {
            let url = spec
                .url
                .take()
                .unwrap_or_else(|| String::from("https://example.invalid/"));
            spec.url = Some(format!("{url}patched"));
        }
        spec.enabled = !spec.enabled;
        spec
    }

    fn envelope(&self, flavor: JsonFlavor) -> Value {
        let mut map = Map::new();
        map.insert(
            String::from("type"),
            Value::String(if self.stdio { "stdio" } else { "http" }.to_owned()),
        );
        if self.stdio {
            map.insert(String::from("command"), Value::String(self.command.clone()));
            if !self.args.is_empty() {
                map.insert(
                    String::from("args"),
                    Value::Array(self.args.iter().cloned().map(Value::String).collect()),
                );
            }
            if !self.env.is_empty() {
                let mut env = Map::new();
                for (key, value) in &self.env {
                    env.insert(key.clone(), Value::String(value.clone()));
                }
                map.insert(String::from("env"), Value::Object(env));
            }
            if let Some(cwd) = &self.cwd {
                map.insert(
                    String::from("cwd"),
                    Value::String(cwd.to_string_lossy().into_owned()),
                );
            }
        } else {
            map.insert(
                String::from("url"),
                Value::String(self.url.clone().unwrap_or_default()),
            );
            if !self.headers.is_empty() {
                let mut headers = Map::new();
                for (key, value) in &self.headers {
                    headers.insert(key.clone(), Value::String(value.clone()));
                }
                map.insert(String::from("headers"), Value::Object(headers));
            }
        }
        if flavor == JsonFlavor::Omp {
            map.insert(String::from("enabled"), Value::Bool(self.enabled));
        }
        for (key, value) in &self.extra {
            map.insert(key.clone(), value.clone());
        }
        Value::Object(map)
    }
}

fn random_spec(rng: &mut Rng, name: String, sentinel: &str) -> Spec {
    let stdio = !rng.chance(4);
    let mut env = BTreeMap::new();
    if stdio && rng.chance(2) {
        env.insert(rng.identifier(6), format!("{sentinel}-env"));
        if rng.chance(3) {
            env.insert(format!("UNICODE_{}", rng.name(3)), rng.unicode_word());
        }
    }
    let mut headers = BTreeMap::new();
    if !stdio && rng.chance(2) {
        headers.insert(String::from("Authorization"), format!("Bearer {sentinel}"));
    }
    let mut extra = BTreeMap::new();
    if rng.chance(3) {
        let mut meta = Map::new();
        meta.insert(String::from("note"), Value::String(sentinel.to_owned()));
        meta.insert(String::from("unicode"), Value::String(rng.unicode_word()));
        extra.insert(format!("meta_{}", rng.name(3)), Value::Object(meta));
    }
    if rng.chance(4) {
        extra.insert(
            format!("timeout_{}", rng.name(2)),
            Value::Number(serde_json::Number::from(rng.span(1, 600))),
        );
    }
    Spec {
        name,
        stdio,
        command: format!("run-{}", rng.name(5)),
        args: if rng.chance(2) {
            vec![rng.name(4), format!("--{sentinel}")]
        } else {
            Vec::new()
        },
        env,
        cwd: if stdio && rng.chance(5) {
            Some(PathBuf::from(format!("/tmp/{sentinel}")))
        } else {
            None
        },
        url: if stdio {
            None
        } else {
            Some(format!("https://example.invalid/{sentinel}/"))
        },
        headers,
        enabled: rng.chance(2),
        extra,
    }
}

/// One generated JSON config plus the byte range a single edit may touch.
struct JsonFixture {
    text: String,
    aperture: Aperture,
    sentinels: Vec<String>,
    name: String,
    desired: Spec,
    scope: ClaudeConfigScope,
    /// Exact `mcpServers` object text when the fixture starts with an empty container.
    empty_container_markup: Option<String>,
    /// Byte range of the `mcpServers` line through its closing brace, when present.
    container: Option<(usize, usize)>,
}

fn build_json(flavor: JsonFlavor, op: Oper, seed: u64) -> JsonFixture {
    build_json_inner(flavor, op, seed, None)
}

fn build_json_with_container(flavor: JsonFlavor, op: Oper, seed: u64) -> JsonFixture {
    build_json_inner(flavor, op, seed, Some(true))
}

fn build_json_inner(
    flavor: JsonFlavor,
    op: Oper,
    seed: u64,
    container: Option<bool>,
) -> JsonFixture {
    let mut rng = Rng::new(seed ^ 0x5EED_5EED_5EED);
    let nl = line_ending(&mut rng);
    let unit = indent_unit(&mut rng);
    let mut sentinels = Vec::new();
    let target_name = format!("srv{}{}", seed, rng.name(3));

    let unrelated = if op == Oper::Add {
        rng.below(4)
    } else {
        rng.below(3)
    };
    let mut specs = Vec::with_capacity(unrelated + 1);
    for index in 0..unrelated {
        let sentinel = format!("sent{seed}x{index}");
        sentinels.push(sentinel.clone());
        let spec_name = format!("other{}{}", index, rng.name(2));
        specs.push(random_spec(&mut rng, spec_name, &sentinel));
    }
    let target_spec = random_spec(&mut rng, target_name.clone(), &format!("target{seed}"));
    let target_index = if op == Oper::Add {
        None
    } else {
        let index = rng.below(specs.len() + 1);
        specs.insert(index, target_spec.clone());
        Some(index)
    };
    let desired = target_spec.desired();

    let container_present = container.unwrap_or_else(|| op != Oper::Add || rng.chance(2));
    let empty_container_markup =
        (container_present && specs.is_empty()).then(|| format!("\"mcpServers\": {{{nl}{unit}}}"));

    let before_count = rng.below(3);
    let after_count = rng.below(3);
    let total_root = before_count + after_count + usize::from(container_present);

    let mut text = String::from("{");
    text.push_str(nl);
    let mut written = 0;
    let mut last_value_end = 1;
    for index in 0..before_count {
        let sentinel = format!("root{seed}b{index}");
        sentinels.push(sentinel.clone());
        let key = format!("notes_{sentinel}");
        let value = random_plain_value(&mut rng, &sentinel);
        text.push_str(unit);
        text.push_str(&json_string(&key));
        text.push_str(": ");
        text.push_str(&render_json_value(&value, unit, nl, 1));
        last_value_end = text.len();
        written += 1;
        if written < total_root {
            text.push(',');
        }
        text.push_str(nl);
    }

    let mut aperture = Aperture::insertion(1);
    let mut container = None;
    if container_present {
        let container_start = text.len();
        text.push_str(unit);
        text.push_str("\"mcpServers\": {");
        let container_open = text.len();
        text.push_str(nl);
        let level_two = unit.repeat(2);
        let mut previous_content_end = container_open;
        for (index, spec) in specs.iter().enumerate() {
            if Some(index) != target_index && rng.chance(3) {
                let sentinel = format!("cmt{seed}x{index}");
                sentinels.push(sentinel.clone());
                text.push_str(&level_two);
                text.push_str("// ");
                text.push_str(&sentinel);
                text.push_str(nl);
            }
            let content_start = text.len() + level_two.len();
            text.push_str(&level_two);
            text.push_str(&json_string(&spec.name));
            text.push_str(": ");
            let envelope = spec.envelope(flavor);
            if rng.chance(4) {
                text.push_str(&envelope.to_string());
            } else {
                text.push_str(&render_json_value(&envelope, unit, nl, 2));
            }
            let content_end = text.len();
            let last = index + 1 == specs.len();
            if !last {
                text.push(',');
            }
            text.push_str(nl);
            if Some(index) == target_index {
                aperture = Aperture {
                    content_start,
                    content_end,
                };
            }
            previous_content_end = content_end;
        }
        if op == Oper::Add {
            aperture = Aperture::insertion(previous_content_end);
        }
        let container_close = text.len() + unit.len();
        text.push_str(unit);
        text.push('}');
        container = Some((container_start, text.len()));
        if op != Oper::Add && specs.len() == 1 {
            // When the container holds exactly this entry, removing or rewriting the entry
            // legitimately reshapes the empty container, so the container body is the block.
            aperture.content_start = container_open;
            aperture.content_end = container_close;
        }
        written += 1;
        if written < total_root {
            text.push(',');
        }
        text.push_str(nl);
    }

    for index in 0..after_count {
        let sentinel = format!("root{seed}a{index}");
        sentinels.push(sentinel.clone());
        let key = format!("later_{sentinel}");
        let value = random_plain_value(&mut rng, &sentinel);
        text.push_str(unit);
        text.push_str(&json_string(&key));
        text.push_str(": ");
        text.push_str(&render_json_value(&value, unit, nl, 1));
        last_value_end = text.len();
        written += 1;
        if written < total_root {
            text.push(',');
        }
        text.push_str(nl);
    }
    text.push('}');
    if !container_present {
        aperture = Aperture::insertion(last_value_end);
    }
    if rng.chance(2) {
        text.push_str(nl);
    }

    let scope = if flavor == JsonFlavor::Claude && rng.chance(2) {
        ClaudeConfigScope::ProjectFile
    } else {
        ClaudeConfigScope::User
    };

    JsonFixture {
        text,
        aperture,
        sentinels,
        name: target_name,
        desired,
        scope,
        empty_container_markup,
        container,
    }
}

fn random_plain_value(rng: &mut Rng, sentinel: &str) -> Value {
    match rng.below(4) {
        0 => Value::String(format!("{sentinel} {}", rng.unicode_word())),
        1 => {
            let mut object = Map::new();
            object.insert(
                format!("nested_{sentinel}_{}", rng.unicode_word()),
                Value::Array(vec![
                    Value::Number(serde_json::Number::from(rng.below(1000))),
                    Value::Object(Map::from_iter([(
                        String::from("unicode"),
                        Value::String(rng.unicode_word()),
                    )])),
                    Value::Bool(true),
                ]),
            );
            Value::Object(object)
        }
        2 => Value::String("x".repeat(rng.span(200, 4_000))),
        _ => Value::Array(vec![
            Value::Number(serde_json::Number::from(rng.below(10_000))),
            Value::Null,
            Value::String(rng.unicode_word()),
        ]),
    }
}

fn apply_json(fixture: &JsonFixture, op: Oper) -> String {
    let config =
        ClaudeMcpConfig::parse(&fixture.text, fixture.scope.clone()).unwrap_or_else(|error| {
            panic!(
                "generated Claude document does not parse: {error}\n{}",
                fixture.text
            )
        });
    match op {
        Oper::Add | Oper::Replace => config.upsert(&fixture.desired.to_claude()),
        Oper::Remove => config.remove(&fixture.name),
    }
    .unwrap_or_else(|error| {
        panic!(
            "generated Claude {} failed: {error}\n{}",
            op.label(),
            fixture.text
        )
    })
}

fn apply_omp(fixture: &JsonFixture, op: Oper) -> String {
    let config = OmpMcpConfig::parse(&fixture.text).unwrap_or_else(|error| {
        panic!(
            "generated omp document does not parse: {error}\n{}",
            fixture.text
        )
    });
    match op {
        Oper::Add | Oper::Replace => config
            .upsert_server(&fixture.desired.to_omp())
            .unwrap_or_else(|error| {
                panic!(
                    "generated omp {} failed: {error}\n{}",
                    op.label(),
                    fixture.text
                )
            }),
        Oper::Remove => config.remove_server(&fixture.name).unwrap_or_else(|error| {
            panic!("generated omp remove failed: {error}\n{}", fixture.text)
        }),
    }
}

fn assert_json_semantics(fixture: &JsonFixture, flavor: JsonFlavor, op: Oper, output: &str) {
    match flavor {
        JsonFlavor::Claude => {
            let config =
                ClaudeMcpConfig::parse(output, fixture.scope.clone()).unwrap_or_else(|error| {
                    panic!("edited Claude document does not parse: {error}\n{output}")
                });
            match op {
                Oper::Add | Oper::Replace => {
                    let server = config.servers().get(&fixture.name).unwrap_or_else(|| {
                        panic!("edited document lost `{}`\n{output}", fixture.name)
                    });
                    match &server.transport {
                        McpTransport::Stdio { command, .. } => {
                            assert_eq!(
                                command, &fixture.desired.command,
                                "seed output did not apply the command"
                            );
                        }
                        McpTransport::Http { url, .. } => {
                            assert_eq!(
                                url.as_str(),
                                fixture.desired.url.as_deref().unwrap_or_default()
                            );
                        }
                    }
                }
                Oper::Remove => assert!(
                    config.servers().get(&fixture.name).is_none(),
                    "removed server is still present\n{output}"
                ),
            }
        }
        JsonFlavor::Omp => {
            let config = OmpMcpConfig::parse(output).unwrap_or_else(|error| {
                panic!("edited omp document does not parse: {error}\n{output}")
            });
            match op {
                Oper::Add | Oper::Replace => {
                    let server = config.server(&fixture.name).unwrap_or_else(|| {
                        panic!("edited document lost `{}`\n{output}", fixture.name)
                    });
                    match &server.transport {
                        McpTransport::Stdio { command, .. } => {
                            assert_eq!(command, &fixture.desired.command)
                        }
                        McpTransport::Http { url, .. } => assert_eq!(
                            url.as_str(),
                            fixture.desired.url.as_deref().unwrap_or_default()
                        ),
                    }
                }
                Oper::Remove => assert!(
                    config.server(&fixture.name).is_none(),
                    "removed server is still present\n{output}"
                ),
            }
        }
    }
}

#[test]
fn json_edits_touch_only_the_edited_entry() {
    for seed in 0..CASES {
        for flavor in [JsonFlavor::Claude, JsonFlavor::Omp] {
            for op in [Oper::Add, Oper::Replace, Oper::Remove] {
                let fixture = build_json(flavor, op, seed);
                let output = match flavor {
                    JsonFlavor::Claude => apply_json(&fixture, op),
                    JsonFlavor::Omp => apply_omp(&fixture, op),
                };
                assert_confined(
                    &format!("json {} {:?}", op.label(), flavor),
                    seed,
                    &fixture.text,
                    &output,
                    fixture.aperture,
                    &fixture.sentinels,
                    is_json_separator,
                );
                assert_json_semantics(&fixture, flavor, op, &output);
            }
        }
    }
}

/// One generated Codex TOML document plus the byte range a single edit may touch.
struct TomlFixture {
    text: String,
    aperture: Aperture,
    sentinels: Vec<String>,
    name: String,
    desired: CodexServerEntry,
}

fn random_toml_spec(rng: &mut Rng, id: String, sentinel: &str) -> Spec {
    let mut spec = random_spec(rng, id, sentinel);
    // Codex has no header table; keep env as a string map, which it does support.
    spec.headers.clear();
    spec.stdio = true;
    spec.url = None;
    spec.enabled = rng.chance(2);
    spec
}

fn render_toml_table(spec: &Spec, nl: &str, rng: &mut Rng) -> String {
    let key = if spec
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        spec.name.clone()
    } else {
        format!("\"{}\"", spec.name.replace('"', "\\\""))
    };
    let mut text = format!("[mcp_servers.{key}]{nl}");
    text.push_str(&format!("command = \"{}\"{nl}", spec.command));
    if !spec.args.is_empty() {
        if rng.chance(2) {
            let items: Vec<String> = spec.args.iter().map(|arg| format!("\"{arg}\"")).collect();
            text.push_str(&format!("args = [{}]{nl}", items.join(", ")));
        } else {
            text.push_str(&format!("args = [{nl}"));
            for arg in &spec.args {
                text.push_str(&format!("  \"{arg}\",{nl}"));
            }
            text.push_str(&format!("]{nl}"));
        }
    }
    if !spec.env.is_empty() {
        let items: Vec<String> = spec
            .env
            .iter()
            .map(|(name, value)| format!("{name} = \"{value}\""))
            .collect();
        text.push_str(&format!("env = {{ {} }}{nl}", items.join(", ")));
    }
    if rng.chance(2) {
        text.push_str(&format!("enabled = {}{nl}", spec.enabled));
    }
    text
}

fn toml_entry(spec: &Spec) -> CodexServerEntry {
    CodexServerEntry {
        command: Some(spec.command.clone()),
        args: Some(spec.args.clone()),
        env: Some(
            spec.env
                .iter()
                .map(|(key, value)| (key.clone(), EnvValue::Literal(value.clone())))
                .collect(),
        ),
        cwd: spec.cwd.clone(),
        enabled: Some(spec.enabled),
        ..CodexServerEntry::default()
    }
}

fn build_toml(op: Oper, seed: u64) -> TomlFixture {
    build_toml_inner(op, seed, false)
}

fn build_toml_with_server(op: Oper, seed: u64) -> TomlFixture {
    build_toml_inner(op, seed, true)
}

fn build_toml_inner(op: Oper, seed: u64, require_existing: bool) -> TomlFixture {
    let mut rng = Rng::new(seed ^ 0xB0B0_1234_5678);
    let nl = line_ending(&mut rng);
    let mut sentinels = Vec::new();
    let mut text = String::new();
    if rng.chance(2) {
        let sentinel = format!("head{seed}");
        text.push_str(&format!("# header {sentinel} {}{nl}", rng.unicode_word()));
        sentinels.push(sentinel);
    }
    let before_keys = rng.below(3);
    for index in 0..before_keys {
        text.push_str(&format!(
            "key_{index} = \"{}{}\"{nl}",
            rng.name(4),
            rng.unicode_word()
        ));
    }
    if before_keys > 0 {
        text.push_str(nl);
    }
    if rng.chance(3) {
        let sentinel = format!("tbl{seed}");
        text.push_str(&format!(
            "# unrelated {sentinel}{nl}[telemetry]{nl}value = \"{sentinel}\"{nl}{nl}"
        ));
        sentinels.push(sentinel);
    }
    if rng.chance(3) {
        let sentinel = format!("dotted{seed}");
        text.push_str(&format!("tool.settings.mode = \"{sentinel}\"{nl}{nl}"));
        sentinels.push(sentinel);
    }
    if rng.chance(3) {
        let sentinel = format!("aot{seed}");
        text.push_str(&format!(
            "[[events]]{nl}name = \"{sentinel}\"{nl}{nl}[[events]]{nl}name = \"second\"{nl}{nl}"
        ));
        sentinels.push(sentinel);
    }

    let unrelated = if op == Oper::Add {
        if require_existing {
            rng.span(1, 3)
        } else {
            rng.below(3)
        }
    } else {
        rng.below(2)
    };
    let target_name = format!("srv{}{}", seed, rng.name(3));
    let mut specs = Vec::with_capacity(unrelated + 1);
    for index in 0..unrelated {
        let sentinel = format!("sent{seed}t{index}");
        sentinels.push(sentinel.clone());
        let spec_name = format!("other{}{}", index, rng.name(2));
        specs.push(random_toml_spec(&mut rng, spec_name, &sentinel));
    }
    let target_spec = random_toml_spec(&mut rng, target_name.clone(), &format!("target{seed}"));
    let target_index = if op == Oper::Add {
        None
    } else {
        let index = rng.below(specs.len() + 1);
        specs.insert(index, target_spec.clone());
        Some(index)
    };
    let desired_spec = target_spec.desired();
    let desired = toml_entry(&desired_spec);

    let mut aperture = Aperture::insertion(text.len());
    let mut last_mcp_end = text.trim_end_matches(['\r', '\n']).len();
    for (index, spec) in specs.iter().enumerate() {
        // A comment directly above the edited table belongs to that table; TOML removes it
        // with the table, so only unrelated tables get sentinel comments.
        if Some(index) != target_index && rng.chance(3) {
            let sentinel = format!("cmt{seed}t{index}");
            sentinels.push(sentinel.clone());
            text.push_str(&format!("# server {sentinel}{nl}"));
        }
        let content_start = text.len();
        let table = if index == 0 && Some(index) != target_index && rng.chance(3) {
            format!(
                "[mcp_servers]{nl}{}.command = \"{}\"{nl}",
                spec.name, spec.command
            )
        } else {
            render_toml_table(spec, nl, &mut rng)
        };
        text.push_str(&table);
        let content_end = text.trim_end_matches(['\r', '\n']).len();
        if Some(index) == target_index {
            aperture = Aperture {
                content_start,
                content_end,
            };
        }
        last_mcp_end = content_end;
        text.push_str(nl);
    }

    let mut last_content_end = if specs.is_empty() {
        text.trim_end_matches(['\r', '\n']).len()
    } else {
        last_mcp_end
    };
    if rng.chance(2) {
        let sentinel = format!("after{seed}");
        sentinels.push(sentinel.clone());
        text.push_str(&format!(
            "# features {sentinel}{nl}[features]{nl}web_search = true{nl}"
        ));
        last_content_end = text.trim_end_matches(['\r', '\n']).len();
    }
    if rng.chance(2) {
        text.push_str(nl);
    }
    if op == Oper::Add {
        aperture = Aperture::insertion(if specs.is_empty() {
            last_content_end
        } else {
            last_mcp_end
        });
    }

    TomlFixture {
        text,
        aperture,
        sentinels,
        name: target_name,
        desired,
    }
}

#[test]
fn project_scope_edits_touch_only_the_project_entry() {
    for seed in 0..CASES {
        for op in [Oper::Add, Oper::Replace, Oper::Remove] {
            let fixture = build_json_with_container(JsonFlavor::Claude, op, seed);
            let (start, end) = fixture.container.expect("fixture has a container");
            let project_path = format!("/workspace/project-{seed}");
            let prefix = format!("{{\n  \"projects\": {{\n    {project_path:?}: {{\n");
            let mut text = String::new();
            text.push_str(&prefix);
            text.push_str(&fixture.text[start..end]);
            text.push_str("\n    }\n  }\n}\n");
            let shift = prefix.len() as isize - start as isize;
            let aperture = Aperture {
                content_start: (fixture.aperture.content_start as isize + shift) as usize,
                content_end: (fixture.aperture.content_end as isize + shift) as usize,
            };
            let scope = ClaudeConfigScope::Project {
                project_path: project_path.clone(),
            };
            let config = ClaudeMcpConfig::parse(&text, scope.clone()).unwrap_or_else(|error| {
                panic!("generated project document does not parse: {error}\n{text}")
            });
            let output = match op {
                Oper::Add | Oper::Replace => config.upsert(&fixture.desired.to_claude()),
                Oper::Remove => config.remove(&fixture.name),
            }
            .unwrap_or_else(|error| panic!("project {} failed: {error}\n{text}", op.label()));
            assert_confined(
                &format!("project json {}", op.label()),
                seed,
                &text,
                &output,
                aperture,
                &fixture.sentinels,
                is_json_separator,
            );
            let parsed = ClaudeMcpConfig::parse(&output, scope).unwrap_or_else(|error| {
                panic!("edited project document does not parse: {error}\n{output}")
            });
            match op {
                Oper::Add | Oper::Replace => {
                    assert!(parsed.servers().contains_key(&fixture.name))
                }
                Oper::Remove => assert!(!parsed.servers().contains_key(&fixture.name)),
            }
        }
    }
}

#[test]
fn toml_edits_touch_only_the_edited_table() {
    for seed in 0..CASES {
        for op in [Oper::Add, Oper::Replace, Oper::Remove] {
            let fixture = build_toml(op, seed);
            let mut config = CodexConfigFile::parse(&fixture.text).unwrap_or_else(|error| {
                panic!("generated TOML does not parse: {error}\n{}", fixture.text)
            });
            match op {
                Oper::Add | Oper::Replace => {
                    config
                        .set_server(&fixture.name, &fixture.desired)
                        .unwrap_or_else(|error| {
                            panic!(
                                "generated TOML {} failed: {error}\n{}",
                                op.label(),
                                fixture.text
                            )
                        });
                }
                Oper::Remove => {
                    assert!(
                        config
                            .remove_server(&fixture.name)
                            .unwrap_or_else(|error| panic!("remove failed: {error}")),
                        "generated target table was absent"
                    );
                }
            }
            let output = config.text();
            assert_confined(
                &format!("toml {}", op.label()),
                seed,
                &fixture.text,
                &output,
                fixture.aperture,
                &fixture.sentinels,
                is_toml_separator,
            );
            let parsed = CodexConfigFile::parse(&output)
                .unwrap_or_else(|error| panic!("edited TOML does not parse: {error}\n{output}"));
            match op {
                Oper::Add | Oper::Replace => {
                    let server = parsed
                        .server(&fixture.name)
                        .expect("server lookup")
                        .unwrap_or_else(|| panic!("edited TOML lost `{}`\n{output}", fixture.name));
                    assert_eq!(server.entry.command, fixture.desired.command);
                }
                Oper::Remove => assert!(
                    parsed
                        .server(&fixture.name)
                        .expect("server lookup")
                        .is_none(),
                    "removed table is still present\n{output}"
                ),
            }
        }
    }
}

/// Empty `mcpServers` objects are allowed to render either as `{}` or as `{<newline><indent>}`.
fn normalize_empty_container(text: &str, markup: Option<&str>) -> String {
    match markup {
        Some(markup) => text.replace(markup, "\"mcpServers\": {}"),
        None => text.to_owned(),
    }
}

#[test]
fn add_then_remove_restores_the_original_document() {
    for seed in 0..CASES {
        let json = build_json_with_container(JsonFlavor::Claude, Oper::Add, seed);
        let added = apply_json(&json, Oper::Add);
        let removed = ClaudeMcpConfig::parse(&added, json.scope.clone())
            .expect("added document parses")
            .remove(&json.name)
            .expect("removal of the added entry");
        assert_eq!(
            normalize_empty_container(&removed, json.empty_container_markup.as_deref()),
            normalize_empty_container(&json.text, json.empty_container_markup.as_deref()),
            "claude add/remove seed={seed} did not restore the document\nadded: {added:?}"
        );

        let omp = build_json_with_container(JsonFlavor::Omp, Oper::Add, seed);
        let added = apply_omp(&omp, Oper::Add);
        let removed = OmpMcpConfig::parse(&added)
            .expect("added document parses")
            .remove_server(&omp.name)
            .expect("removal of the added entry");
        assert_eq!(
            normalize_empty_container(&removed, omp.empty_container_markup.as_deref()),
            normalize_empty_container(&omp.text, omp.empty_container_markup.as_deref()),
            "omp add/remove seed={seed} did not restore the document\nadded: {added:?}"
        );

        let toml = build_toml_with_server(Oper::Add, seed);
        let mut config = CodexConfigFile::parse(&toml.text).expect("generated TOML parses");
        config
            .set_server(&toml.name, &toml.desired)
            .expect("generated TOML add");
        let mut reparse = CodexConfigFile::parse(&config.text()).expect("added TOML parses");
        assert!(
            reparse
                .remove_server(&toml.name)
                .expect("generated TOML remove")
        );
        let restored = reparse.text();
        // Documented whitespace rule: a document without a final newline gains one, because a
        // table appended at EOF must start on its own line; everything else is byte-identical.
        let expected = if toml.text.ends_with('\n') {
            toml.text.clone()
        } else if toml.text.contains("\r\n") {
            format!("{}\r\n", toml.text)
        } else {
            format!("{}\n", toml.text)
        };
        assert_eq!(
            restored,
            expected,
            "codex add/remove seed={seed} did not restore the document\nadded: {:?}",
            config.text()
        );
    }
}

#[test]
fn repeated_edits_are_idempotent() {
    for seed in 0..CASES {
        for flavor in [JsonFlavor::Claude, JsonFlavor::Omp] {
            for op in [Oper::Add, Oper::Replace] {
                let fixture = build_json(flavor, op, seed);
                let first = match flavor {
                    JsonFlavor::Claude => apply_json(&fixture, op),
                    JsonFlavor::Omp => apply_omp(&fixture, op),
                };
                let second = match flavor {
                    JsonFlavor::Claude => {
                        let config = ClaudeMcpConfig::parse(&first, fixture.scope.clone())
                            .expect("edited document parses");
                        config
                            .upsert(&fixture.desired.to_claude())
                            .expect("second identical upsert")
                    }
                    JsonFlavor::Omp => {
                        let config = OmpMcpConfig::parse(&first).expect("edited document parses");
                        config
                            .upsert_server(&fixture.desired.to_omp())
                            .expect("second identical upsert")
                    }
                };
                assert_eq!(
                    first,
                    second,
                    "{} {:?} seed={seed}: the same edit applied twice changed bytes",
                    op.label(),
                    flavor
                );
            }
        }
        for op in [Oper::Add, Oper::Replace] {
            let fixture = build_toml(op, seed);
            let mut config = CodexConfigFile::parse(&fixture.text).expect("generated TOML parses");
            config
                .set_server(&fixture.name, &fixture.desired)
                .expect("first set");
            let first = config.text();
            config
                .set_server(&fixture.name, &fixture.desired)
                .expect("second set");
            assert_eq!(
                first,
                config.text(),
                "toml {} seed={seed}: the same edit applied twice changed bytes",
                op.label()
            );
        }
    }

    // Removal is idempotent in the sense that the second attempt is a typed refusal that
    // leaves the document untouched.
    let remove_doc =
        "{\n  \"mcpServers\": {\n    \"gone\": {\"type\": \"stdio\", \"command\": \"x\"}\n  }\n}\n";
    let config = ClaudeMcpConfig::parse(remove_doc, ClaudeConfigScope::User).expect("parse");
    let removed = config.remove("gone").expect("first removal");
    let error = ClaudeMcpConfig::parse(&removed, ClaudeConfigScope::User)
        .expect("reparse")
        .remove("gone")
        .expect_err("second removal must be refused");
    assert!(matches!(error, ClaudeMcpError::UnknownServer { .. }));
    let omp_error = OmpMcpConfig::parse(&removed)
        .expect("reparse")
        .remove_server("gone");
    assert!(matches!(omp_error, Err(OmpMcpError::UnknownServer { .. })));
    let mut codex = CodexConfigFile::parse("[mcp_servers.gone]\ncommand = \"x\"\n").expect("parse");
    assert!(codex.remove_server("gone").expect("first removal"));
    let after = codex.text();
    assert!(!codex.remove_server("gone").expect("second removal"));
    assert_eq!(codex.text(), after);
}

#[test]
fn malformed_documents_return_typed_errors_without_writes() {
    // Truncated JSON and TOML never panic and never produce output through the editor.
    for seed in 0..150 {
        let mut rng = Rng::new(seed ^ 0xDEAD_BEEF);
        let fixture = build_json(JsonFlavor::Claude, Oper::Replace, seed);
        let mut cut = rng.span(1, fixture.text.len() - 1);
        while cut < fixture.text.len()
            && (!fixture.text.is_char_boundary(cut)
                || ClaudeMcpConfig::parse(&fixture.text[..cut], ClaudeConfigScope::User).is_ok())
        {
            cut += 1;
        }
        if cut < fixture.text.len() {
            let truncated = &fixture.text[..cut];
            let error = ClaudeMcpConfig::parse(truncated, ClaudeConfigScope::User)
                .expect_err("truncated JSON must be refused");
            assert!(
                matches!(error, ClaudeMcpError::Parse { .. }),
                "seed={seed}: {error}"
            );
            let omp_error =
                OmpMcpConfig::parse(truncated).expect_err("truncated JSON must be refused");
            assert!(
                matches!(omp_error, OmpMcpError::Parse { .. }),
                "seed={seed}"
            );
        }

        let toml = build_toml(Oper::Replace, seed);
        let mut cut = rng.span(1, toml.text.len() - 1);
        while cut < toml.text.len()
            && (!toml.text.is_char_boundary(cut)
                || CodexConfigFile::parse(&toml.text[..cut]).is_ok())
        {
            cut += 1;
        }
        if cut < toml.text.len() {
            let error = CodexConfigFile::parse(&toml.text[..cut])
                .expect_err("truncated TOML must be refused");
            assert!(
                matches!(error, CodexError::InvalidToml { .. }),
                "seed={seed}: {error}"
            );
        }
    }

    // Duplicate server names are ambiguous in JSON/JSONC.
    let duplicate = "{\n  \"mcpServers\": {\n    \"a\": {\"type\": \"stdio\", \"command\": \"x\"},\n    \"a\": {\"type\": \"stdio\", \"command\": \"y\"}\n  }\n}\n";
    let error = ClaudeMcpConfig::parse(duplicate, ClaudeConfigScope::User).expect_err("duplicates");
    assert!(
        matches!(error, ClaudeMcpError::DuplicateServer { .. }),
        "{error}"
    );
    let error = OmpMcpConfig::parse(duplicate).expect_err("duplicates");
    assert!(
        matches!(error, OmpMcpError::DuplicateServer { .. }),
        "{error}"
    );
    assert!(matches!(
        CodexConfigFile::parse("[mcp_servers.a]\ncommand = \"x\"\ncommand = \"y\"\n"),
        Err(CodexError::InvalidToml { .. })
    ));

    // A byte-order mark is not valid JSONC or TOML here: typed refusal, no editor output.
    let bom_json = "\u{feff}{\n  \"mcpServers\": {}\n}\n";
    assert!(matches!(
        ClaudeMcpConfig::parse(bom_json, ClaudeConfigScope::User),
        Err(ClaudeMcpError::Parse { .. })
    ));
    assert!(matches!(
        OmpMcpConfig::parse(bom_json),
        Err(OmpMcpError::Parse { .. })
    ));
    assert!(matches!(
        CodexConfigFile::parse("\u{feff}[mcp_servers.a]\ncommand = \"x\"\n"),
        Err(CodexError::ByteOrderMark)
    ));

    // Parser depth bound: above 512 levels is a typed error, not a stack overflow.
    for depth in [513_usize, 2_000] {
        let mut text = String::from(
            "{\"mcpServers\": {\"a\": {\"type\": \"stdio\", \"command\": \"x\", \"deep\": ",
        );
        text.push_str(&"[".repeat(depth));
        text.push_str(&"]".repeat(depth));
        text.push_str("}}}");
        let error = ClaudeMcpConfig::parse(&text, ClaudeConfigScope::User)
            .expect_err("deep nesting must be refused");
        assert!(matches!(error, ClaudeMcpError::Parse { .. }), "{error}");
        let error = OmpMcpConfig::parse(&text).expect_err("deep nesting must be refused");
        assert!(matches!(error, OmpMcpError::Parse { .. }), "{error}");
    }

    // Below the jsonc-parser bound (512 levels), a deeply nested unrelated value still
    // parses and an edit keeps it intact.
    let depth = 400;
    let deep = format!(
        "{{\"mcpServers\": {{\"alpha\": {{\"type\": \"stdio\", \"command\": \"a\"}}}}, \"deep\": {}0{}}}",
        "[".repeat(depth),
        "]".repeat(depth)
    );
    let config = ClaudeMcpConfig::parse(&deep, ClaudeConfigScope::User).expect("400 levels parse");
    let edited = config
        .upsert(&ClaudeServer {
            name: String::from("beta"),
            transport: McpTransport::Stdio {
                command: String::from("b"),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        })
        .expect("edit below the depth bound");
    assert_eq!(
        edited.matches('[').count(),
        deep.matches('[').count(),
        "the deep value was rewritten"
    );

    // TOML has its own recursion bound: a typed error, never a stack overflow.
    for depth in [600_usize, 2_000] {
        let mut nested = String::from("0");
        for _ in 0..depth {
            nested = format!("{{ a = {nested} }}");
        }
        let text = format!("[mcp_servers.deep]\ncommand = {nested}\n");
        let error = CodexConfigFile::parse(&text).expect_err("deep TOML must be refused");
        assert!(matches!(error, CodexError::InvalidToml { .. }), "{error}");
    }
    let segments: Vec<&str> = (0..600).map(|_| "a").collect();
    let deep_header = format!("[{}]\nv = 1\n", segments.join("."));
    assert!(matches!(
        CodexConfigFile::parse(&deep_header),
        Err(CodexError::InvalidToml { .. })
    ));

    // File-level failures never write: invalid UTF-8, a BOM and a too-large Codex config.
    let dir = TempDir::new().expect("temp dir");
    let codex_path = dir.path().join("config.toml");
    let omp_path = dir.path().join("mcp.json");
    let claude_path = dir.path().join(".claude.json");
    let invalid = [0xff_u8, 0xfe, 0xfd, 0x00, 0x80];
    for path in [&codex_path, &omp_path, &claude_path] {
        fs::write(path, invalid).expect("write invalid UTF-8");
        let before = fs::read(path).expect("read before");
        let is_codex = path == &codex_path;
        let result_codex = CodexConfigFile::load(path);
        let result_omp = OmpMcpAdapter::new().read(path);
        let result_claude = ClaudeMcpAdapter::read(path, ClaudeConfigScope::User);
        assert!(
            result_codex.is_err(),
            "Codex accepted invalid UTF-8 at {path:?}"
        );
        assert!(
            result_omp.is_err(),
            "omp accepted invalid UTF-8 at {path:?}"
        );
        assert!(
            result_claude.is_err(),
            "Claude accepted invalid UTF-8 at {path:?}"
        );
        if is_codex {
            assert!(matches!(result_codex, Err(CodexError::NotUtf8 { .. })));
            assert!(matches!(result_omp, Err(OmpMcpError::NotUtf8 { .. })));
        }
        assert_eq!(
            fs::read(path).expect("read after"),
            before,
            "a failed load wrote {path:?}"
        );
    }

    for path in [&codex_path, &omp_path, &claude_path] {
        fs::write(path, "\u{feff}[mcp_servers.a]\ncommand = \"x\"\n").expect("write BOM");
        let before = fs::read(path).expect("read before");
        assert!(CodexConfigFile::load(path).is_err());
        assert!(OmpMcpAdapter::new().read(path).is_err());
        assert!(ClaudeMcpAdapter::read(path, ClaudeConfigScope::User).is_err());
        assert_eq!(
            fs::read(path).expect("read after"),
            before,
            "a failed load wrote {path:?}"
        );
    }

    let oversized = vec![b'#'; (at_adapters::codex_mcp::MAX_CONFIG_BYTES + 1) as usize];
    fs::write(&codex_path, &oversized).expect("write oversized config");
    match CodexConfigFile::load(&codex_path) {
        Err(CodexError::TooLarge { .. }) => {}
        other => panic!("oversized Codex config was not refused: {other:?}"),
    }
    assert_eq!(fs::read(&codex_path).expect("read after"), oversized);
}

#[test]
fn multi_megabyte_documents_stay_precise_and_bounded() {
    // One 5 MiB document per JSON editor: parsing and editing is bounded, and the edit is
    // still confined to the edited entry.
    let mut big = String::from("{\n  \"unrelated\": [");
    let mut index = 0;
    while big.len() < 5 * 1024 * 1024 {
        if index > 0 {
            big.push(',');
        }
        big.push_str(&format!(
            "{{\"k\":{index},\"note\":\"sentinel-big-{index}\"}}"
        ));
        index += 1;
    }
    big.push_str("],\n  \"mcpServers\": {\n    \"alpha\": {\"type\": \"stdio\", \"command\": \"a\"}\n  }\n}\n");
    let anchor = big.rfind("\n  }\n}\n").expect("container close");
    let server = ClaudeServer {
        name: String::from("beta"),
        transport: McpTransport::Stdio {
            command: String::from("b"),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        },
        extra: BTreeMap::new(),
    };
    let claude =
        ClaudeMcpConfig::parse(&big, ClaudeConfigScope::User).expect("large document parses");
    let edited = claude.upsert(&server).expect("large document edit");
    assert_insertion_only(
        "large claude add",
        &big,
        &edited,
        anchor,
        &[
            String::from("sentinel-big-0"),
            format!("sentinel-big-{}", index - 1),
        ],
    );

    let omp_server = OmpServer::new(
        "beta",
        McpTransport::Stdio {
            command: String::from("b"),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        },
        true,
    );
    let omp = OmpMcpConfig::parse(&big).expect("large document parses");
    let edited = omp.upsert_server(&omp_server).expect("large document edit");
    assert_insertion_only(
        "large omp add",
        &big,
        &edited,
        anchor,
        &[
            String::from("sentinel-big-0"),
            format!("sentinel-big-{}", index - 1),
        ],
    );
}

#[test]
fn secret_references_never_leak_values_or_get_echoed() {
    for seed in 0..200 {
        let mut rng = Rng::new(seed ^ 0x005E_C2E7_5EC2);
        let token = rng.token();
        let variable = format!("TOKEN_{}", rng.identifier(6));
        let transport = McpTransport::Stdio {
            command: String::from("run"),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::from([(
                variable.clone(),
                EnvValue::Secret(SecretRef {
                    env_name: variable.clone(),
                    keychain_account: token.clone(),
                }),
            )]),
        };

        // Claude renders the reference, never the Keychain account value.
        let claude_doc = "{\n  \"mcpServers\": {}\n}\n";
        let config =
            ClaudeMcpConfig::parse(claude_doc, ClaudeConfigScope::User).expect("empty document");
        let server = ClaudeServer {
            name: String::from("secret"),
            transport: transport.clone(),
            extra: BTreeMap::new(),
        };
        let rendered = config.upsert(&server).expect("reference-form upsert");
        assert!(
            rendered.contains(&format!("${{{variable}}}")),
            "seed={seed}: the reference form was not written: {rendered}"
        );
        assert!(
            !rendered.contains(&token),
            "seed={seed}: the Keychain account value reached the document: {rendered}"
        );
        let refs = BTreeMap::from([(variable.clone(), token.clone())]);
        let reparsed =
            ClaudeMcpConfig::parse_with_secret_refs(&rendered, ClaudeConfigScope::User, &refs)
                .expect("reference round trip");
        let McpTransport::Stdio { env, .. } = &reparsed.servers()["secret"].transport else {
            panic!("stdio was rewritten");
        };
        assert!(matches!(env.get(&variable), Some(EnvValue::Secret(_))));

        // omp and Codex refuse the reference form instead of writing an unverified variable.
        let omp = OmpMcpConfig::parse("{\"mcpServers\": {}}\n").expect("empty omp");
        let error = omp
            .upsert_server(&OmpServer::new("secret", transport.clone(), true))
            .expect_err("omp must refuse ${VAR}");
        assert!(
            error.to_string().contains(&variable),
            "seed={seed}: {error}"
        );
        assert!(!error.to_string().contains(&token), "seed={seed}: {error}");

        let mut codex = CodexConfigFile::parse("").expect("empty codex");
        let (entry, _) = CodexServerEntry::from_transport(&transport);
        let error = codex
            .set_server("secret", &entry)
            .expect_err("Codex must refuse ${VAR}");
        assert!(
            error.to_string().contains(&variable),
            "seed={seed}: {error}"
        );
        assert!(!error.to_string().contains(&token), "seed={seed}: {error}");

        // Parser errors never quote a token-shaped value from the offending line.
        let malformed_toml = format!("model = {token}\n[mcp_servers.a]\ncommand = \"x\"\n");
        let error = CodexConfigFile::parse(&malformed_toml).expect_err("malformed TOML");
        assert!(!error.to_string().contains(&token), "seed={seed}: {error}");
        let malformed_json =
            format!("{{\"mcpServers\": {{\"a\": {{\"type\": \"http\", \"url\": \"{token}\"}}}}}}");
        let error = ClaudeMcpConfig::parse(&malformed_json, ClaudeConfigScope::User)
            .expect_err("invalid URL");
        assert!(!error.to_string().contains(&token), "seed={seed}: {error}");
        let error = OmpMcpConfig::parse(&malformed_json).expect_err("invalid URL");
        assert!(!error.to_string().contains(&token), "seed={seed}: {error}");
    }
}
