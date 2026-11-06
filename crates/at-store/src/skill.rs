//! `SKILL.md` parsing, validation and format-preserving edits.
//!
//! A skill document is a YAML frontmatter block delimited by `---` lines followed by a markdown
//! body. Parsing never re-serialises: every value keeps its byte range in the original text, so an
//! edit rewrites only the bytes it targets.
//!
//! ```
//! use at_store::skill::{parse_skill_doc, set_description};
//!
//! let source = b"---\nname: my-skill\ndescription: Does one thing\nlicense: MIT\n---\n# My skill\n";
//! let doc = parse_skill_doc(source)?;
//! assert_eq!(doc.name, "my-skill");
//! assert_eq!(doc.unknown_keys.len(), 1);
//! let edited = set_description(&doc, "Does another thing")?;
//! assert!(edited.contains("description: Does another thing\n"));
//! # Ok::<(), at_model::skill::SkillError>(())
//! ```

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use at_model::skill::{
    ByteSpan, MAX_SKILL_DOC_BYTES, ParsedSkill, SkillDoc, SkillError, SkillNameMismatch,
    SkillOrigin, UnknownKey, check_folder_name, validate_skill_description, validate_skill_name,
};
use saphyr::{AnnotatedMapping, LoadableYamlNode, MarkedYaml, YamlData};

/// Opening and closing frontmatter delimiter.
const DELIMITER: &str = "---";

/// UTF-8 byte order mark; stripped only for scanning, never removed from [`SkillDoc::raw`].
const BOM: char = '\u{feff}';

/// Parses the bytes of a `SKILL.md` file.
///
/// # Errors
/// [`SkillError`] for a document that is too large or not UTF-8, missing or unclosed frontmatter,
/// frontmatter that is not a block-style YAML mapping, or a missing or invalid
/// `name`/`description`.
pub fn parse_skill_doc(bytes: &[u8]) -> Result<SkillDoc, SkillError> {
    if bytes.len() > MAX_SKILL_DOC_BYTES {
        return Err(SkillError::DocTooLarge {
            size: bytes.len(),
            limit: MAX_SKILL_DOC_BYTES,
        });
    }
    let text = core::str::from_utf8(bytes).map_err(|error| SkillError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;
    parse_text(text)
}

/// Parses a skill document found in `folder` and applies the folder/name rule: a mismatch is a
/// blocking error for [`SkillOrigin::Create`] and a warning for [`SkillOrigin::Import`].
///
/// # Errors
/// Any [`parse_skill_doc`] error, plus [`SkillError::NameMismatch`] when creating.
pub fn parse_skill_doc_in_folder(
    bytes: &[u8],
    folder: &str,
    origin: SkillOrigin,
) -> Result<ParsedSkill, SkillError> {
    let doc = parse_skill_doc(bytes)?;
    let warning = check_folder_name(&doc.name, folder, origin)?;
    Ok(ParsedSkill { doc, warning })
}

/// Rewrites only the description value of `doc`, returning the complete new document text.
///
/// `description` is trimmed before validation and writing. Every byte outside the description
/// value keeps its original position and content; the result is re-parsed and accepted only if
/// the name, unknown keys and body are unchanged.
///
/// # Errors
/// [`SkillError::EmptyDescription`], [`SkillError::DescriptionTooLong`], or
/// [`SkillError::DescriptionNotRepresentable`] when no YAML encoding round-trips.
pub fn set_description(doc: &SkillDoc, description: &str) -> Result<String, SkillError> {
    let trimmed = description.trim();
    validate_skill_description(trimmed)?;
    let span = doc.description_span;
    let (Some(head), Some(tail)) = (doc.raw.get(..span.start()), doc.raw.get(span.end()..)) else {
        return Err(SkillError::MissingDescription);
    };

    let rebuild = |value: &str| {
        let mut out = String::with_capacity(doc.raw.len() + value.len());
        out.push_str(head);
        out.push(' ');
        out.push_str(value);
        out.push_str(tail);
        out
    };

    if let Some(plain) = encode_plain(trimmed) {
        let candidate = rebuild(&plain);
        if round_trips(doc, &candidate, trimmed) {
            return Ok(candidate);
        }
    }
    let candidate = rebuild(&encode_quoted(trimmed));
    if round_trips(doc, &candidate, trimmed) {
        return Ok(candidate);
    }
    Err(SkillError::DescriptionNotRepresentable {
        value: trimmed.to_owned(),
    })
}

/// A `SKILL.md` read from disk together with the warning an import should surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSkill {
    /// Path the document was read from.
    pub path: PathBuf,
    /// The parsed document.
    pub doc: SkillDoc,
    /// Folder/name warning for imports, `None` when the names match.
    pub warning: Option<SkillNameMismatch>,
}

/// Why a `SKILL.md` could not be read from disk.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SkillReadError {
    /// The file could not be opened or read.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The path is not a regular file.
    #[error("{}: not a regular file", .path.display())]
    NotRegularFile {
        /// Path involved.
        path: PathBuf,
    },
    /// The bytes are not a valid skill document.
    #[error("{}: {source}", .path.display())]
    Invalid {
        /// Path involved.
        path: PathBuf,
        /// Parse or validation error.
        #[source]
        source: SkillError,
    },
    /// The parent folder has no UTF-8 name to check the skill name against.
    #[error("{}: the skill folder has no usable name", .path.display())]
    FolderNameUnavailable {
        /// Path involved.
        path: PathBuf,
    },
}

/// Reads and parses a `SKILL.md` file, bounding the read by [`MAX_SKILL_DOC_BYTES`].
///
/// # Errors
/// [`SkillReadError`] for I/O failures, files over the limit, non-files and invalid documents.
pub fn read_skill_doc(path: &Path) -> Result<SkillDoc, SkillReadError> {
    let bytes = read_document(path)?;
    parse_skill_doc(&bytes).map_err(|source| SkillReadError::Invalid {
        path: path.to_path_buf(),
        source,
    })
}

/// Reads a `SKILL.md` from a skill folder and applies the folder/name rule for `origin`.
///
/// # Errors
/// [`read_skill_doc`] errors, [`SkillReadError::FolderNameUnavailable`], or
/// [`SkillError::NameMismatch`] wrapped in [`SkillReadError::Invalid`] when creating.
pub fn read_skill_doc_in_folder(
    path: &Path,
    origin: SkillOrigin,
) -> Result<LoadedSkill, SkillReadError> {
    let folder = folder_name(path)
        .ok_or_else(|| SkillReadError::FolderNameUnavailable {
            path: path.to_path_buf(),
        })?
        .to_owned();
    let bytes = read_document(path)?;
    let parsed = parse_skill_doc_in_folder(&bytes, &folder, origin).map_err(|source| {
        SkillReadError::Invalid {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(LoadedSkill {
        path: path.to_path_buf(),
        doc: parsed.doc,
        warning: parsed.warning,
    })
}

fn parse_text(text: &str) -> Result<SkillDoc, SkillError> {
    let bom_len = if text.starts_with(BOM) {
        BOM.len_utf8()
    } else {
        0
    };
    let (opener_end, frontmatter_start) = line_at(text, bom_len);
    if text[bom_len..opener_end].trim_end() != DELIMITER {
        return Err(SkillError::MissingFrontmatter);
    }
    let frontmatter_end =
        find_closing_delimiter(text, frontmatter_start).ok_or(SkillError::UnclosedFrontmatter)?;
    let (closer_end, body_start) = line_at(text, frontmatter_end);
    if text[frontmatter_end..closer_end].trim_end() != DELIMITER {
        return Err(SkillError::UnclosedFrontmatter);
    }

    let frontmatter = &text[frontmatter_start..frontmatter_end];
    let documents = MarkedYaml::load_from_str(frontmatter).map_err(|error| {
        invalid_yaml(
            error.marker().line() + 1,
            error.marker().col() + 1,
            error.info(),
        )
    })?;
    if documents.len() > 1 {
        let marker = documents[1].span.start;
        return Err(invalid_yaml(
            marker.line() + 1,
            marker.col() + 1,
            "frontmatter contains more than one YAML document",
        ));
    }
    let document = documents.first().ok_or(SkillError::MissingName)?;
    let mapping = match &document.data {
        YamlData::Mapping(mapping) => mapping,
        _ => return Err(SkillError::FrontmatterNotMapping),
    };

    let name = mapping_lookup(mapping, "name")
        .ok_or(SkillError::MissingName)?
        .data
        .as_str()
        .ok_or(SkillError::NameNotString)?;
    validate_skill_name(name)?;
    let description = mapping_lookup(mapping, "description")
        .ok_or(SkillError::MissingDescription)?
        .data
        .as_str()
        .ok_or(SkillError::DescriptionNotString)?
        .trim();
    validate_skill_description(description)?;

    let entries = scan_entries(text, frontmatter_start, frontmatter_end);
    let description_entry = entries
        .iter()
        .find(|entry| entry.key == "description")
        .ok_or(SkillError::FrontmatterNotMapping)?;
    // Block-style mappings are the only editable form; anything else (flow style, exotic keys)
    // would leave spans the writer cannot trust.
    if mapping.len() != entries.len() {
        return Err(SkillError::FrontmatterNotMapping);
    }

    let unknown_keys = entries
        .iter()
        .filter(|entry| entry.key != "name" && entry.key != "description")
        .map(|entry| UnknownKey {
            key: entry.key.clone(),
            span: entry.span(),
            raw: text[entry.key_start..entry.entry_end].to_owned(),
        })
        .collect();

    Ok(SkillDoc {
        name: name.to_owned(),
        description: description.to_owned(),
        raw: text.to_owned(),
        frontmatter_span: ByteSpan::new(frontmatter_start, frontmatter_end),
        body_span: ByteSpan::new(body_start, text.len()),
        description_span: ByteSpan::new(description_entry.colon + 1, description_entry.entry_end),
        unknown_keys,
    })
}

fn invalid_yaml(line: usize, column: usize, message: &str) -> SkillError {
    SkillError::InvalidFrontmatter {
        line,
        column,
        message: message.to_owned(),
    }
}

fn mapping_lookup<'a>(
    mapping: &'a AnnotatedMapping<'a, MarkedYaml<'a>>,
    key: &str,
) -> Option<&'a MarkedYaml<'a>> {
    mapping
        .iter()
        .find(|(candidate, _)| candidate.data.as_str() == Some(key))
        .map(|(_, value)| value)
}

/// Returns `(content_end, next_line_start)` for the line beginning at `start`.
///
/// `content_end` excludes the line break (including a preceding `\r`, so CRLF files work).
fn line_at(text: &str, start: usize) -> (usize, usize) {
    match text[start..].find('\n') {
        Some(offset) => {
            let newline = start + offset;
            let mut content_end = newline;
            if content_end > start && text.as_bytes()[content_end - 1] == b'\r' {
                content_end -= 1;
            }
            (content_end, newline + 1)
        }
        None => (text.len(), text.len()),
    }
}

fn find_closing_delimiter(text: &str, start: usize) -> Option<usize> {
    let mut line_start = start;
    while line_start < text.len() {
        let (content_end, after) = line_at(text, line_start);
        if text[line_start..content_end].trim_end() == DELIMITER {
            return Some(line_start);
        }
        if after <= line_start {
            return None;
        }
        line_start = after;
    }
    None
}

/// A top-level frontmatter entry located in the original text.
struct Entry {
    key: String,
    key_start: usize,
    colon: usize,
    entry_end: usize,
}

impl Entry {
    fn span(&self) -> ByteSpan {
        ByteSpan::new(self.key_start, self.entry_end)
    }
}

/// Scans the top-level block entries of the frontmatter, preserving source order.
fn scan_entries(text: &str, start: usize, end: usize) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut line_start = start;
    while line_start < end {
        let (content_end, after) = line_at(text, line_start);
        let content_end = content_end.min(end);
        if let Some((key_raw, colon_offset)) = top_level_key(&text[line_start..content_end]) {
            entries.push(Entry {
                key: normalize_key(key_raw),
                key_start: line_start,
                colon: line_start + colon_offset,
                entry_end: content_end,
            });
        }
        if after <= line_start {
            break;
        }
        line_start = after;
    }
    for index in 0..entries.len() {
        let limit = entries.get(index + 1).map_or(end, |next| next.key_start);
        let key_start = entries[index].key_start;
        entries[index].entry_end = entry_end(text, key_start, limit);
    }
    entries
}

/// End of an entry's value: the last line that is the key line or an indented continuation.
fn entry_end(text: &str, key_start: usize, limit: usize) -> usize {
    let (first_content_end, mut after) = line_at(text, key_start);
    let mut last = first_content_end.min(limit);
    while after < limit {
        let (content_end, next) = line_at(text, after);
        let content_end = content_end.min(limit);
        let line = &text[after..content_end];
        if line.trim().is_empty() {
            if next <= after {
                break;
            }
            after = next;
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            break;
        }
        last = content_end;
        if next <= after {
            break;
        }
        after = next;
    }
    last
}

/// Splits `line` into a top-level key and the byte offset of its `:`.
fn top_level_key(line: &str) -> Option<(&str, usize)> {
    let first = line.as_bytes().first()?;
    if matches!(
        first,
        b' ' | b'\t' | b'#' | b'{' | b'[' | b'%' | b'&' | b'*'
    ) {
        return None;
    }
    if line.starts_with("---") || line.starts_with("...") || line.starts_with("- ") {
        return None;
    }
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(quote_byte) => {
                if byte == b'\\' && quote_byte == b'"' {
                    index += 2;
                    continue;
                }
                if byte == quote_byte {
                    quote = None;
                }
            }
            None => {
                if byte == b'"' || byte == b'\'' {
                    quote = Some(byte);
                } else if byte == b':' {
                    let next = bytes.get(index + 1);
                    if next.is_none() || matches!(next, Some(b' ') | Some(b'\t')) {
                        let key = line[..index].trim_end();
                        if key.is_empty() {
                            return None;
                        }
                        return Some((key, index));
                    }
                }
            }
        }
        index += 1;
    }
    None
}

/// Removes quotes from a key so `"name"` and `name` compare equal.
fn normalize_key(raw: &str) -> String {
    let len = raw.len();
    if len >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        let mut out = String::with_capacity(len - 2);
        let mut chars = raw[1..len - 1].chars();
        while let Some(character) = chars.next() {
            if character == '\\' {
                match chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(character);
            }
        }
        return out;
    }
    if len >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        return raw[1..len - 1].replace("''", "'");
    }
    raw.to_owned()
}

/// Encodes a value as a plain scalar when that is unambiguous, otherwise `None`.
fn encode_plain(value: &str) -> Option<String> {
    if value.is_empty() || value.trim() != value {
        return None;
    }
    if value.chars().any(char::is_control) {
        return None;
    }
    let first = value.chars().next()?;
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        return None;
    }
    if value.contains(": ") || value.contains(" #") || value.ends_with(':') {
        return None;
    }
    // Values YAML would resolve to a non-string need quoting.
    const NON_STRINGS: &[&str] = &[
        "true", "false", "null", "~", "yes", "no", "on", "off", "True", "False", "Null", "TRUE",
        "FALSE", "NULL", "Yes", "No", "On", "Off",
    ];
    if NON_STRINGS.contains(&value) {
        return None;
    }
    if value.parse::<i64>().is_ok() || value.parse::<u64>().is_ok() || value.parse::<f64>().is_ok()
    {
        return None;
    }
    Some(value.to_owned())
}

/// Encodes a value as a double-quoted YAML scalar.
fn encode_quoted(value: &str) -> String {
    use core::fmt::Write;

    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Accepts a rewritten document only when nothing but the description changed.
fn round_trips(original: &SkillDoc, candidate: &str, description: &str) -> bool {
    match parse_skill_doc(candidate.as_bytes()) {
        Ok(parsed) => {
            parsed.name == original.name
                && parsed.description == description
                && parsed.body() == original.body()
                && parsed.unknown_keys.len() == original.unknown_keys.len()
                && parsed
                    .unknown_keys
                    .iter()
                    .zip(&original.unknown_keys)
                    .all(|(new, old)| new.key == old.key && new.raw == old.raw)
        }
        Err(_) => false,
    }
}

fn folder_name(path: &Path) -> Option<&str> {
    path.parent()?.file_name().and_then(OsStr::to_str)
}

fn read_document(path: &Path) -> Result<Vec<u8>, SkillReadError> {
    let file = File::open(path).map_err(|source| SkillReadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| SkillReadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(SkillReadError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    let limit = MAX_SKILL_DOC_BYTES;
    if metadata.len() > limit as u64 {
        return Err(SkillReadError::Invalid {
            path: path.to_path_buf(),
            source: SkillError::DocTooLarge {
                size: usize::try_from(metadata.len()).unwrap_or(usize::MAX),
                limit,
            },
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| SkillReadError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > limit {
        return Err(SkillReadError::Invalid {
            path: path.to_path_buf(),
            source: SkillError::DocTooLarge {
                size: bytes.len(),
                limit,
            },
        });
    }
    Ok(bytes)
}
