//! Format-preserving renderer for AgenTuna-owned instruction regions.

use std::path::{Path, PathBuf};

/// Start marker for an AgenTuna-managed instruction block.
pub const MANAGED_BEGIN: &[u8] = b"<!-- agentuna:begin -->";
/// End marker for an AgenTuna-managed instruction block.
pub const MANAGED_END: &[u8] = b"<!-- agentuna:end -->";

/// Why rendering a managed block was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ManagedBlockError {
    /// Existing unmarked file needs explicit adoption.
    #[error("adoption is required before modifying {path}")]
    AdoptRequired {
        /// Existing file.
        path: PathBuf,
    },
    /// Markers are invalid or unterminated.
    #[error("malformed managed block in {path} at line {line}")]
    MalformedManagedBlock {
        /// Invalid file.
        path: PathBuf,
        /// One-based start line.
        line: usize,
    },
}

/// Replaces only the managed content. An absent file becomes a managed block.
///
/// # Errors
/// Returns [`ManagedBlockError::AdoptRequired`] for an existing unmarked file or
/// [`ManagedBlockError::MalformedManagedBlock`] for invalid marker placement.
pub fn render_managed_block(
    path: &Path,
    existing: Option<&[u8]>,
    content: &[u8],
) -> Result<Vec<u8>, ManagedBlockError> {
    let Some(existing) = existing else {
        let mut out =
            Vec::with_capacity(content.len() + MANAGED_BEGIN.len() + MANAGED_END.len() + 4);
        out.extend_from_slice(MANAGED_BEGIN);
        out.extend_from_slice(b"\n");
        out.extend_from_slice(content);
        if !content.ends_with(b"\n") {
            out.push(b'\n');
        }
        out.extend_from_slice(MANAGED_END);
        out.push(b'\n');
        return Ok(out);
    };
    let Some(start) = find(existing, MANAGED_BEGIN) else {
        return if find(existing, MANAGED_END).is_some() {
            Err(ManagedBlockError::MalformedManagedBlock {
                path: path.to_path_buf(),
                line: 1,
            })
        } else {
            Err(ManagedBlockError::AdoptRequired {
                path: path.to_path_buf(),
            })
        };
    };
    let body_start = start + MANAGED_BEGIN.len();
    let Some(offset) = find(&existing[body_start..], MANAGED_END) else {
        return Err(ManagedBlockError::MalformedManagedBlock {
            path: path.to_path_buf(),
            line: 1 + existing[..start].iter().filter(|&&b| b == b'\n').count(),
        });
    };
    let end = body_start + offset;
    if find(&existing[end + MANAGED_END.len()..], MANAGED_BEGIN).is_some() {
        return Err(ManagedBlockError::MalformedManagedBlock {
            path: path.to_path_buf(),
            line: 1 + existing[..end].iter().filter(|&&b| b == b'\n').count(),
        });
    }
    if find(&existing[end + MANAGED_END.len()..], MANAGED_END).is_some() {
        return Err(ManagedBlockError::MalformedManagedBlock {
            path: path.to_path_buf(),
            line: 1 + existing[..end].iter().filter(|&&b| b == b'\n').count(),
        });
    }
    let mut out = Vec::with_capacity(existing.len() + content.len());
    out.extend_from_slice(&existing[..body_start]);
    out.push(b'\n');
    out.extend_from_slice(content);
    if !content.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(&existing[end..]);
    Ok(out)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
