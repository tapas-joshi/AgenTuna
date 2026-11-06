//! Skill templates, canonical slug generation and the live wizard preview.
//!
//! The add-skill wizard renders a complete `SKILL.md` on every keystroke
//! ([`render_preview`]) and saves the document it previewed. Everything here is a pure
//! function over its arguments: no IO, no clock, no global state, so the UI can debounce
//! calls without worrying about ordering or partial writes.
//!
//! The rendered document is a `---` frontmatter block carrying exactly `name` and
//! `description`, followed by an H1, one `Use when:` line and the template body. YAML scalars
//! are escaped so quotes, colons and newlines in a description round-trip through the
//! `SKILL.md` parser (TS-005).
//!
//! ```
//! use at_engine::skill_templates::{SkillDraft, SkillTemplate, render_preview, slugify};
//!
//! let draft = SkillDraft::new("My Great Skill!", "Reviews a change.", SkillTemplate::Checklist);
//! let preview = render_preview(&draft);
//! assert_eq!(preview.slug.as_deref(), Some("my-great-skill"));
//! assert!(preview.markdown.contains("- [ ]"));
//! assert!(preview.is_valid());
//! assert!(slugify("***").is_err());
//! ```

use at_model::skill::{SKILL_NAME_MAX_CHARS, SkillError, validate_skill_description};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Slug used in a preview when the display name has no usable characters yet.
///
/// The rendered document is still a valid `SKILL.md` shape, so the preview pane keeps
/// updating while the user types. Saving must require [`SkillPreview::is_valid`], which is
/// false while the name cannot produce a slug.
pub const PLACEHOLDER_SLUG: &str = "untitled";

/// One of the fixed starting points offered by the add-skill wizard.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillTemplate {
    /// Frontmatter, a `Usage` section and nothing else.
    Blank,
    /// A `Checklist` section of markdown checkboxes.
    Checklist,
    /// Numbered `Workflow` steps plus `Inputs` and `Outputs` sections.
    Workflow,
}

impl SkillTemplate {
    /// Every template, in the order the wizard offers them.
    pub const ALL: [Self; 3] = [Self::Blank, Self::Checklist, Self::Workflow];

    /// Stable identifier for the CLI, logs and generated UI types.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blank => "blank",
            Self::Checklist => "checklist",
            Self::Workflow => "workflow",
        }
    }

    /// The markdown body that follows the H1 and `Use when:` line.
    #[must_use]
    pub const fn body(self) -> &'static str {
        match self {
            Self::Blank => BLANK_BODY,
            Self::Checklist => CHECKLIST_BODY,
            Self::Workflow => WORKFLOW_BODY,
        }
    }
}

const BLANK_BODY: &str = "\
## Usage

Describe what this skill does and when to apply it.

1. First action.
2. Second action.
";

const CHECKLIST_BODY: &str = "\
## Checklist

- [ ] First item
- [ ] Second item
- [ ] Third item
";

const WORKFLOW_BODY: &str = "\
## Workflow

1. First step.
2. Second step.
3. Third step.

## Inputs

- What the skill needs before it starts.

## Outputs

- What the skill produces when it finishes.
";

/// The wizard inputs the preview is rendered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SkillDraft<'a> {
    /// Human display name, used for the H1. The frontmatter `name` comes from [`slugify`].
    pub display_name: &'a str,
    /// Frontmatter `description`.
    pub description: &'a str,
    /// Template body to render.
    pub template: SkillTemplate,
}

impl<'a> SkillDraft<'a> {
    /// A draft over borrowed wizard buffers.
    #[must_use]
    pub const fn new(display_name: &'a str, description: &'a str, template: SkillTemplate) -> Self {
        Self {
            display_name,
            description,
            template,
        }
    }
}

/// Why a display name cannot produce a canonical slug.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SlugError {
    /// Nothing survived lowercasing and replacing punctuation: no letters or digits.
    #[error("the name has no letters or digits to build a slug from")]
    Empty,
    /// The slug is longer than [`SKILL_NAME_MAX_CHARS`].
    #[error("slug `{slug}` is {len} characters; the limit is {limit}")]
    TooLong {
        /// The slug that was produced.
        slug: String,
        /// Its length in characters.
        len: usize,
        /// The limit.
        limit: usize,
    },
}

/// Why a description is not usable, independent of the preview.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DescriptionIssue {
    /// Nothing but whitespace.
    Empty,
    /// Longer than the parser's limit.
    TooLong {
        /// Length in characters.
        len: usize,
        /// The limit.
        limit: usize,
    },
}

/// Everything the wizard needs from one render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPreview {
    /// Canonical slug, `None` when the display name has no usable characters.
    pub slug: Option<String>,
    /// The complete `SKILL.md`; uses [`PLACEHOLDER_SLUG`] while `slug` is `None`.
    pub markdown: String,
    /// Why the display name cannot be saved yet.
    pub name_error: Option<SlugError>,
    /// Why the description cannot be saved yet.
    pub description_error: Option<DescriptionIssue>,
}

impl SkillPreview {
    /// Whether the draft can be saved as rendered.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.name_error.is_none() && self.description_error.is_none()
    }
}

/// Produces the canonical skill slug for a display name.
///
/// The name is compatibility-decomposed (NFKD), combining marks are dropped, ASCII
/// letters are lowercased, every other character becomes a separator, runs of separators
/// collapse to one `-` and leading/trailing separators disappear. The result therefore
/// always satisfies [`at_model::skill::validate_skill_name`].
///
/// `"My Great Skill!"` → `my-great-skill`; `"Étude"` → `etude`.
///
/// # Errors
/// [`SlugError::Empty`] when no letter or digit survives, [`SlugError::TooLong`] when the
/// slug exceeds [`SKILL_NAME_MAX_CHARS`].
pub fn slugify(display_name: &str) -> Result<String, SlugError> {
    let mut slug = String::with_capacity(display_name.len());
    let mut pending_separator = false;
    for character in display_name.nfkd().filter(|c| !is_combining_mark(*c)) {
        for lowered in character.to_lowercase() {
            if is_combining_mark(lowered) {
                continue;
            }
            if lowered.is_ascii_lowercase() || lowered.is_ascii_digit() {
                if pending_separator && !slug.is_empty() {
                    slug.push('-');
                }
                pending_separator = false;
                slug.push(lowered);
            } else {
                // Punctuation and letters NFKD cannot decompose into ASCII act as separators.
                pending_separator = true;
            }
        }
    }
    if slug.is_empty() {
        return Err(SlugError::Empty);
    }
    let len = slug.chars().count();
    if len > SKILL_NAME_MAX_CHARS {
        return Err(SlugError::TooLong {
            slug,
            len,
            limit: SKILL_NAME_MAX_CHARS,
        });
    }
    Ok(slug)
}

/// Renders a complete `SKILL.md`.
///
/// `title` is the H1 and may differ from `slug`; `description` is emitted as an escaped YAML
/// scalar. No validation happens here: callers that need a savable document use
/// [`render_preview`] or validate the slug and description themselves.
#[must_use]
pub fn render_document(
    template: SkillTemplate,
    slug: &str,
    title: &str,
    description: &str,
) -> String {
    let description = description.trim();
    let mut out = String::with_capacity(description.len() + template.body().len() + 256);
    out.push_str("---\nname: ");
    out.push_str(slug);
    out.push('\n');
    out.push_str("description: ");
    out.push_str(&yaml_scalar(description));
    out.push_str("\n---\n\n# ");
    out.push_str(title);
    out.push_str("\n\nUse when: ");
    out.push_str(&one_line(description));
    out.push_str("\n\n");
    out.push_str(template.body());
    out
}

/// Renders the wizard preview for `draft`.
///
/// The function never fails: an unusable name or description is reported in
/// [`SkillPreview::name_error`] / [`SkillPreview::description_error`] while the markdown
/// keeps rendering, so the preview pane never goes dark mid-typing.
#[must_use]
pub fn render_preview(draft: &SkillDraft<'_>) -> SkillPreview {
    let display_name = draft.display_name.trim();
    let (slug, name_error) = match slugify(display_name) {
        Ok(slug) => (Some(slug), None),
        Err(error) => (None, Some(error)),
    };
    let description = draft.description.trim();
    let description_error = match validate_skill_description(description) {
        Ok(()) => None,
        Err(SkillError::EmptyDescription) => Some(DescriptionIssue::Empty),
        Err(SkillError::DescriptionTooLong { len, limit }) => {
            Some(DescriptionIssue::TooLong { len, limit })
        }
        Err(_) => None,
    };
    let effective_slug = slug.as_deref().unwrap_or(PLACEHOLDER_SLUG);
    let title = if display_name.is_empty() {
        effective_slug
    } else {
        display_name
    };
    SkillPreview {
        markdown: render_document(draft.template, effective_slug, title, description),
        slug,
        name_error,
        description_error,
    }
}

/// A single-line rendering of a description for the `Use when:` sentence.
fn one_line(description: &str) -> String {
    let mut out = String::with_capacity(description.len());
    for word in description.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Emits `value` as a YAML scalar that parses back to the same string.
fn yaml_scalar(value: &str) -> String {
    if plain_scalar_is_safe(value) {
        value.to_owned()
    } else {
        quoted_scalar(value)
    }
}

/// Whether `value` can be written unquoted without changing its meaning.
fn plain_scalar_is_safe(value: &str) -> bool {
    if value.is_empty() || value.trim() != value {
        return false;
    }
    if value.chars().any(char::is_control) {
        return false;
    }
    let Some(first) = value.chars().next() else {
        return false;
    };
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        return false;
    }
    if value.contains(": ") || value.contains(" #") || value.ends_with(':') {
        return false;
    }
    const NON_STRING_SCALARS: &[&str] = &[
        "true", "false", "null", "~", "yes", "no", "on", "off", "True", "False", "Null", "TRUE",
        "FALSE", "NULL", "Yes", "No", "On", "Off",
    ];
    if NON_STRING_SCALARS.contains(&value) {
        return false;
    }
    if value.parse::<i64>().is_ok() || value.parse::<u64>().is_ok() || value.parse::<f64>().is_ok()
    {
        return false;
    }
    true
}

/// Emits `value` as a double-quoted YAML scalar with every special byte escaped.
fn quoted_scalar(value: &str) -> String {
    use core::fmt::Write as _;

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
