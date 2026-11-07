//! Skill identity and the shape of a parsed `SKILL.md` document.
//!
//! The types here are IO-free and parser-free so the rest of the workspace can pass a parsed
//! skill around without pulling in a YAML parser. `at-store`'s `skill` module produces
//! [`SkillDoc`] values from bytes; every span it records is a byte range into [`SkillDoc::raw`].

/// Required file name of a skill definition.
pub const SKILL_FILE_NAME: &str = "SKILL.md";

/// Maximum number of characters in a skill name (Claude Code's current limit).
pub const SKILL_NAME_MAX_CHARS: usize = 64;

/// Maximum number of characters in a skill description (Claude Code's current limit).
pub const SKILL_DESCRIPTION_MAX_CHARS: usize = 1024;

/// Largest `SKILL.md` document the parser accepts (1 MiB).
pub const MAX_SKILL_DOC_BYTES: usize = 1024 * 1024;

/// A half-open byte range (`start..end`) in the original document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ByteSpan {
    start: usize,
    end: usize,
}

impl ByteSpan {
    /// Creates a span. The caller is responsible for `start <= end` and for both offsets being
    /// byte boundaries of the text the span refers to.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// First byte of the range.
    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    /// First byte after the range.
    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }

    /// Number of bytes in the range.
    #[must_use]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    /// Whether the range contains no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The range as a slice of `text`, or `None` when the range is out of bounds or not on a
    /// character boundary.
    #[must_use]
    pub fn slice(self, text: &str) -> Option<&str> {
        text.get(self.start..self.end)
    }

    /// The range as a slice of `bytes`, or `None` when the range is out of bounds.
    #[must_use]
    pub fn slice_bytes(self, bytes: &[u8]) -> Option<&[u8]> {
        bytes.get(self.start..self.end)
    }
}

/// A frontmatter key other than `name` or `description`, kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKey {
    /// The key as written, with the surrounding quotes removed when the key was quoted.
    pub key: String,
    /// Byte range of the whole entry: from the first byte of the key to the end of its value,
    /// excluding the line break that ends it. Blank lines after the entry are not included.
    pub span: ByteSpan,
    /// The entry's source text, verbatim.
    pub raw: String,
}

/// A parsed `SKILL.md` document: the two known fields plus everything else, untouched.
///
/// `raw` is the complete file, including a leading UTF-8 BOM and CRLF line endings. All spans
/// index into it, so `raw[span]` always yields the exact original bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDoc {
    /// Skill name from the frontmatter, already validated against [`validate_skill_name`].
    pub name: String,
    /// Skill description from the frontmatter, trimmed and validated.
    pub description: String,
    /// The complete document exactly as parsed.
    pub raw: String,
    /// Byte range of the YAML block between the opening and closing `---` lines, delimiters
    /// excluded.
    pub frontmatter_span: ByteSpan,
    /// Byte range of the markdown body, starting after the closing `---` line.
    pub body_span: ByteSpan,
    /// Byte range of the whole `name` entry: from the first byte of the key to the end of its
    /// value, excluding the line break that ends it. This is the location a trace reports for a
    /// store skill.
    pub name_span: ByteSpan,
    /// Byte range of the description value as written: from the byte after the `:` to the end of
    /// the scalar (block-scalar headers and indentation included, the final line break excluded).
    /// Rewriting exactly this range changes only the description.
    pub description_span: ByteSpan,
    /// Frontmatter keys other than `name` and `description`, in source order.
    pub unknown_keys: Vec<UnknownKey>,
}

impl SkillDoc {
    /// The YAML frontmatter text, delimiters excluded.
    #[must_use]
    pub fn frontmatter(&self) -> &str {
        self.frontmatter_span.slice(&self.raw).unwrap_or("")
    }

    /// The markdown body after the closing `---` line.
    #[must_use]
    pub fn body(&self) -> &str {
        self.body_span.slice(&self.raw).unwrap_or("")
    }

    /// The markdown body as raw bytes.
    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        self.body_span
            .slice_bytes(self.raw.as_bytes())
            .unwrap_or(&[])
    }
}

/// Where a skill document comes from, which decides how a folder/name mismatch is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SkillOrigin {
    /// The user is creating a new skill; the name must match the folder.
    Create,
    /// A skill is being imported; the name may differ from the folder it was found in.
    Import,
}

/// A skill name that differs from the folder holding the document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("skill name `{name}` does not match its folder `{folder}`")]
pub struct SkillNameMismatch {
    /// Name declared in the frontmatter.
    pub name: String,
    /// Folder name the document was found in.
    pub folder: String,
}

impl SkillNameMismatch {
    /// Returns the mismatch when `name` and `folder` differ, `None` when they match.
    #[must_use]
    pub fn new(name: &str, folder: &str) -> Option<Self> {
        (name != folder).then(|| Self {
            name: name.to_owned(),
            folder: folder.to_owned(),
        })
    }
}

/// A parsed document plus the folder/name warning an import should surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSkill {
    /// The parsed document.
    pub doc: SkillDoc,
    /// Set when the document was imported from a folder whose name differs from the declared
    /// name. Creation turns the same condition into an error instead.
    pub warning: Option<SkillNameMismatch>,
}

/// Why a `SKILL.md` document is not a usable skill.
///
/// Each malformed case has its own variant so callers can point at the exact field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SkillError {
    /// The document is larger than [`MAX_SKILL_DOC_BYTES`].
    #[error("skill document is {size} bytes; the limit is {limit}")]
    DocTooLarge {
        /// Observed size.
        size: usize,
        /// Limit.
        limit: usize,
    },
    /// The document is not valid UTF-8.
    #[error("skill document is not valid UTF-8 at byte {valid_up_to}")]
    InvalidUtf8 {
        /// Byte offset of the first invalid sequence.
        valid_up_to: usize,
    },
    /// The document does not open with a `---` line.
    #[error("skill document does not start with a `---` frontmatter block")]
    MissingFrontmatter,
    /// The document opens a frontmatter block that is never closed.
    #[error("skill document opens frontmatter with `---` but never closes it")]
    UnclosedFrontmatter,
    /// The frontmatter is not valid YAML.
    #[error("frontmatter is not valid YAML at line {line}, column {column}: {message}")]
    InvalidFrontmatter {
        /// 1-based line in the document.
        line: usize,
        /// 1-based column.
        column: usize,
        /// Parser message.
        message: String,
    },
    /// The frontmatter is valid YAML but must be a block-style mapping.
    #[error("frontmatter must be a block-style YAML mapping")]
    FrontmatterNotMapping,
    /// No `name` key.
    #[error("frontmatter has no `name` field")]
    MissingName,
    /// `name` exists but is not a string.
    #[error("frontmatter `name` must be a string")]
    NameNotString,
    /// `name` is empty.
    #[error("skill name is empty")]
    NameEmpty,
    /// `name` is longer than [`SKILL_NAME_MAX_CHARS`].
    #[error("skill name `{name}` is {len} characters; the limit is {limit}")]
    NameTooLong {
        /// The name.
        name: String,
        /// Observed length in characters.
        len: usize,
        /// Limit.
        limit: usize,
    },
    /// `name` contains an uppercase character.
    #[error("skill name `{name}` contains an uppercase character")]
    NameUppercase {
        /// The name.
        name: String,
    },
    /// `name` contains whitespace.
    #[error("skill name `{name}` contains whitespace")]
    NameWhitespace {
        /// The name.
        name: String,
    },
    /// `name` contains a character other than a lowercase letter, digit or hyphen.
    #[error("skill name `{name}` contains `{character}`")]
    NameInvalidCharacter {
        /// The name.
        name: String,
        /// Offending character.
        character: char,
    },
    /// `name` starts with a hyphen.
    #[error("skill name `{name}` starts with a hyphen")]
    NameLeadingHyphen {
        /// The name.
        name: String,
    },
    /// `name` ends with a hyphen.
    #[error("skill name `{name}` ends with a hyphen")]
    NameTrailingHyphen {
        /// The name.
        name: String,
    },
    /// `name` contains two hyphens in a row.
    #[error("skill name `{name}` contains consecutive hyphens")]
    NameDoubleHyphen {
        /// The name.
        name: String,
    },
    /// No `description` key.
    #[error("frontmatter has no `description` field")]
    MissingDescription,
    /// `description` exists but is not a string.
    #[error("frontmatter `description` must be a string")]
    DescriptionNotString,
    /// `description` is empty after trimming.
    #[error("skill description is empty")]
    EmptyDescription,
    /// `description` is longer than [`SKILL_DESCRIPTION_MAX_CHARS`].
    #[error("skill description is {len} characters; the limit is {limit}")]
    DescriptionTooLong {
        /// Observed length in characters.
        len: usize,
        /// Limit.
        limit: usize,
    },
    /// The description could not be written back without changing its meaning.
    #[error("skill description `{value}` cannot be represented in YAML")]
    DescriptionNotRepresentable {
        /// The description that failed to round-trip.
        value: String,
    },
    /// The declared name differs from the folder (blocking when creating).
    #[error("skill name `{name}` does not match its folder `{folder}`")]
    NameMismatch {
        /// Declared name.
        name: String,
        /// Folder name.
        folder: String,
    },
}

/// Validates a skill name: 1 to [`SKILL_NAME_MAX_CHARS`] characters, lowercase letters, digits
/// and single hyphens, with no leading or trailing hyphen.
///
/// # Errors
/// Returns the first rule the name breaks, in this order: empty, length, uppercase, whitespace,
/// invalid character, leading hyphen, trailing hyphen, consecutive hyphens.
pub fn validate_skill_name(name: &str) -> Result<(), SkillError> {
    if name.is_empty() {
        return Err(SkillError::NameEmpty);
    }
    let len = name.chars().count();
    if len > SKILL_NAME_MAX_CHARS {
        return Err(SkillError::NameTooLong {
            name: name.to_owned(),
            len,
            limit: SKILL_NAME_MAX_CHARS,
        });
    }
    if name.chars().any(|c| c.is_uppercase()) {
        return Err(SkillError::NameUppercase {
            name: name.to_owned(),
        });
    }
    if name.chars().any(|c| c.is_whitespace()) {
        return Err(SkillError::NameWhitespace {
            name: name.to_owned(),
        });
    }
    if let Some(character) = name
        .chars()
        .find(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && *c != '-')
    {
        return Err(SkillError::NameInvalidCharacter {
            name: name.to_owned(),
            character,
        });
    }
    if name.starts_with('-') {
        return Err(SkillError::NameLeadingHyphen {
            name: name.to_owned(),
        });
    }
    if name.ends_with('-') {
        return Err(SkillError::NameTrailingHyphen {
            name: name.to_owned(),
        });
    }
    if name.contains("--") {
        return Err(SkillError::NameDoubleHyphen {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Validates a trimmed description: 1 to [`SKILL_DESCRIPTION_MAX_CHARS`] characters.
///
/// # Errors
/// [`SkillError::EmptyDescription`] or [`SkillError::DescriptionTooLong`].
pub fn validate_skill_description(description: &str) -> Result<(), SkillError> {
    let len = description.chars().count();
    if len == 0 {
        return Err(SkillError::EmptyDescription);
    }
    if len > SKILL_DESCRIPTION_MAX_CHARS {
        return Err(SkillError::DescriptionTooLong {
            len,
            limit: SKILL_DESCRIPTION_MAX_CHARS,
        });
    }
    Ok(())
}

/// Applies the folder/name rule: a mismatch blocks [`SkillOrigin::Create`] and merely warns for
/// [`SkillOrigin::Import`].
///
/// # Errors
/// [`SkillError::NameMismatch`] when the origin is [`SkillOrigin::Create`] and the names differ.
pub fn check_folder_name(
    name: &str,
    folder: &str,
    origin: SkillOrigin,
) -> Result<Option<SkillNameMismatch>, SkillError> {
    let mismatch = SkillNameMismatch::new(name, folder);
    match (origin, mismatch) {
        (SkillOrigin::Create, Some(mismatch)) => Err(SkillError::NameMismatch {
            name: mismatch.name,
            folder: mismatch.folder,
        }),
        (_, mismatch) => Ok(mismatch),
    }
}
