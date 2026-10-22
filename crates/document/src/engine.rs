//! The slice of the shared engine contract this crate uses.
//!
//! In the system this was extracted from, every engine shares one core crate
//! for metadata and errors. Only the parts the document engine touches are
//! inlined here, unchanged in shape, so the extractors read the same as they
//! do in production.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

pub type EngineResult<T> = Result<T, EngineError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EngineId(String);

impl EngineId {
    pub fn new(value: impl Into<String>) -> EngineResult<Self> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(EngineError::invalid_input(
                "engine_id_empty",
                "engine id must not be empty",
            ));
        }
        if !trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        }) {
            return Err(EngineError::invalid_input(
                "engine_id_invalid",
                "engine id may only contain ASCII letters, numbers, dashes, underscores, and dots",
            ));
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EngineId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Document,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineMetadata {
    pub id: EngineId,
    pub kind: EngineKind,
    pub name: String,
    pub version: String,
    pub capabilities: BTreeSet<String>,
}

impl EngineMetadata {
    pub fn new(id: EngineId, kind: EngineKind, name: impl Into<String>) -> Self {
        Self {
            id,
            kind,
            name: name.into(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            capabilities: BTreeSet::new(),
        }
    }

    pub fn with_capabilities(
        mut self,
        capabilities: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.capabilities
            .extend(capabilities.into_iter().map(Into::into));
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineErrorKind {
    InvalidInput,
    Dependency,
}

/// An error with a stable snake_case `code`, so callers branch on
/// `"pdf_unreadable"` rather than parsing a message.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct EngineError {
    pub code: String,
    pub kind: EngineErrorKind,
    pub message: String,
    pub retryable: bool,
}

impl EngineError {
    pub fn new(
        kind: EngineErrorKind,
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            kind,
            message: message.into(),
            retryable,
        }
    }

    pub fn invalid_input(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(EngineErrorKind::InvalidInput, code, message, false)
    }

    pub fn dependency(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(EngineErrorKind::Dependency, code, message, true)
    }
}
