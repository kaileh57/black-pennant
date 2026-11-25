//! Contract-owned (no work unit). Error type for every content loader and validator.
use crate::ids::ContentHash;

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("RON parse error in {file}: {message}")]
    Parse { file: String, message: String },
    /// Untrusted-input limits checked before parsing (L2081).
    #[error("input exceeds limit: {0}")]
    Limit(&'static str),
    /// File format newer than this build; the message names the release it needs (L2083).
    #[error("format {found} is newer than supported {supported}; needs release {needs_release}")]
    FormatTooNew { found: u32, supported: u32, needs_release: String },
    #[error("share string damaged: content does not match its hash")]
    DamagedShareString,
    #[error("not a share string (missing BPN1: prefix or bad base64url)")]
    NotAShareString,
    #[error("unknown reference: {kind} `{key}`")]
    UnknownRef { kind: &'static str, key: String },
    #[error("migration {from} -> {to} failed: {message}")]
    Migration { from: u32, to: u32, message: String },
    #[error("save chain mismatch: expected previous hash {expected:?}")]
    SaveChain { expected: ContentHash },
    #[error("validation failed with {0} issue(s)")]
    Invalid(usize),
    #[error("io: {0}")]
    Io(String),
}
pub type ContentResult<T> = Result<T, ContentError>;
