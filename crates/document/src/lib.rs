//! The document engine.
//!
//! Turns a file into positioned blocks: text that knows which page, slide or
//! section it came from.
//!
//! Position is the whole point. A chunk without a page number can be retrieved
//! but not cited, and a citation that cannot be opened is barely better than no
//! citation — the user has to take the answer on faith, which is exactly what
//! source grounding exists to avoid. So every extractor here carries position
//! through, and the ones that cannot say so rather than inventing it.

#![forbid(unsafe_code)]

pub mod engine;
mod html;
mod markdown;
mod ooxml;
mod pdf;

use async_trait::async_trait;
pub use engine::{
    EngineError, EngineErrorKind, EngineId, EngineKind, EngineMetadata, EngineResult,
};
use serde::{Deserialize, Serialize};

/// What kind of thing a block is.
///
/// Retrieval treats these differently: a heading starts a new chunk, a caption
/// belongs to whatever it captions, and a table should not be split mid-row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Heading,
    Paragraph,
    ListItem,
    Table,
    Caption,
    Code,
}

/// A unit of extracted text, with wherever it came from attached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentBlock {
    pub text: String,
    pub kind: BlockKind,
    /// 1-indexed page for PDFs, slide number for PPTX. `None` for formats with
    /// no pagination, which is honest rather than defaulting to 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Heading trail leading to this block, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub section: Vec<String>,
    /// Position within the document, for stable ordering across extractors.
    pub ordinal: usize,
}

impl DocumentBlock {
    pub fn new(text: impl Into<String>, kind: BlockKind, ordinal: usize) -> Self {
        Self {
            text: text.into(),
            kind,
            page: None,
            section: Vec::new(),
            ordinal,
        }
    }

    pub fn with_page(mut self, page: u32) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_section(mut self, section: Vec<String>) -> Self {
        self.section = section;
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedDocument {
    pub format: DocumentFormat,
    pub metadata: DocumentMetadata,
    pub blocks: Vec<DocumentBlock>,
    /// Extraction problems worth surfacing rather than swallowing — a scanned
    /// page with no text layer, a slide with only images. The caller decides
    /// whether that means "needs OCR" or "this document is unusable".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl NormalizedDocument {
    pub fn total_characters(&self) -> usize {
        self.blocks.iter().map(|block| block.text.len()).sum()
    }

    /// True when the file parsed but produced almost nothing.
    ///
    /// Usually a scanned PDF: structurally valid, no text layer. Treating that
    /// as a successful extraction produces a source that is "ready" and
    /// answers nothing, which is worse than a visible failure.
    pub fn looks_empty(&self) -> bool {
        self.total_characters() < 32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    Pdf,
    Docx,
    Pptx,
    Html,
    Markdown,
    PlainText,
}

impl DocumentFormat {
    /// Identify by content first, extension second.
    ///
    /// Extensions lie — downloads get renamed, exports keep the wrong suffix —
    /// and parsing a PPTX as a DOCX fails confusingly rather than cleanly.
    pub fn detect(bytes: &[u8], filename: Option<&str>) -> Option<Self> {
        if bytes.starts_with(b"%PDF-") {
            return Some(Self::Pdf);
        }

        // Both DOCX and PPTX are zip archives; the parts inside distinguish them.
        if bytes.starts_with(b"PK\x03\x04") {
            // A zip we cannot classify is not a document we can read.
            return ooxml::classify(bytes);
        }

        let extension = filename
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, ext)| ext.to_ascii_lowercase());

        match extension.as_deref() {
            Some("pdf") => Some(Self::Pdf),
            Some("docx") => Some(Self::Docx),
            Some("pptx") => Some(Self::Pptx),
            Some("html" | "htm") => Some(Self::Html),
            Some("md" | "markdown") => Some(Self::Markdown),
            Some("txt" | "text" | "log") => Some(Self::PlainText),
            _ => {
                let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_lowercase();
                if head.contains("<!doctype html") || head.contains("<html") {
                    Some(Self::Html)
                } else if bytes.is_empty() {
                    None
                } else {
                    // Valid UTF-8 with no other signal is text worth indexing.
                    std::str::from_utf8(bytes).ok().map(|_| Self::PlainText)
                }
            }
        }
    }
}

/// Normalizes a file into positioned blocks.
#[async_trait]
pub trait DocumentEngine: Send + Sync {
    fn metadata(&self) -> EngineMetadata;

    async fn normalize(
        &self,
        bytes: &[u8],
        filename: Option<&str>,
    ) -> EngineResult<NormalizedDocument>;
}

/// The built-in extractor set.
#[derive(Debug, Default, Clone, Copy)]
pub struct StandardDocumentEngine;

impl StandardDocumentEngine {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DocumentEngine for StandardDocumentEngine {
    fn metadata(&self) -> EngineMetadata {
        EngineMetadata::new(
            EngineId::new("document.standard").expect("valid engine id"),
            EngineKind::Document,
            "Standard document engine",
        )
        .with_capabilities(["pdf", "docx", "pptx", "html", "markdown", "text"])
    }

    async fn normalize(
        &self,
        bytes: &[u8],
        filename: Option<&str>,
    ) -> EngineResult<NormalizedDocument> {
        let format = DocumentFormat::detect(bytes, filename).ok_or_else(|| {
            EngineError::invalid_input(
                "document_format_unknown",
                "could not identify the document format from its contents or filename",
            )
        })?;

        // PDF extraction is CPU-bound and synchronous; keeping it on the async
        // runtime would stall every other task on the worker for the duration
        // of a large file.
        let owned = bytes.to_vec();
        let extracted = tokio_blocking(move || match format {
            DocumentFormat::Pdf => pdf::extract(&owned),
            DocumentFormat::Docx => ooxml::extract_docx(&owned),
            DocumentFormat::Pptx => ooxml::extract_pptx(&owned),
            DocumentFormat::Html => html::extract(&owned),
            DocumentFormat::Markdown => markdown::extract(&owned),
            DocumentFormat::PlainText => Ok(markdown::extract_plain(&owned)),
        })
        .await?;

        let mut document = extracted;
        document.format = format;

        if document.looks_empty() {
            document.warnings.push(
                "the file parsed but produced almost no text; if it is a scan it needs OCR"
                    .to_string(),
            );
        }

        Ok(document)
    }
}

/// Run blocking work off the async runtime, mapping the join failure.
async fn tokio_blocking<F, T>(work: F) -> EngineResult<T>
where
    F: FnOnce() -> EngineResult<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(EngineError::dependency(
            "document_extraction_failed",
            format!("extraction task did not complete: {error}"),
        )),
    }
}

/// Shared helper: collapse runs of whitespace without destroying paragraphing.
pub(crate) fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;

    for character in text.chars() {
        if character.is_whitespace() && character != '\n' {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(character);
            last_was_space = false;
        }
    }

    out.trim().to_string()
}

/// Whether a line reads as a heading rather than prose.
///
/// Shared with the chunker's notion so a heading detected here produces the
/// same boundary there.
pub(crate) fn looks_like_heading(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 80 {
        return false;
    }
    if trimmed.starts_with('#') {
        return true;
    }
    let ends_like_prose =
        trimmed.ends_with('.') || trimmed.ends_with('?') || trimmed.ends_with('!');
    !ends_like_prose && trimmed.split_whitespace().count() <= 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_pdf_by_magic_bytes_not_extension() {
        assert_eq!(
            DocumentFormat::detect(b"%PDF-1.7\nrest", Some("notes.txt")),
            Some(DocumentFormat::Pdf),
            "content must beat a wrong extension"
        );
    }

    #[test]
    fn detects_html_without_an_extension() {
        let html = b"<!doctype html><html><body><p>hi</p></body></html>";
        assert_eq!(
            DocumentFormat::detect(html, None),
            Some(DocumentFormat::Html)
        );
    }

    #[test]
    fn falls_back_to_plain_text_for_utf8() {
        assert_eq!(
            DocumentFormat::detect("just some notes".as_bytes(), None),
            Some(DocumentFormat::PlainText)
        );
    }

    #[test]
    fn refuses_binary_it_cannot_identify() {
        assert_eq!(
            DocumentFormat::detect(&[0xff, 0xfe, 0x00, 0x01], None),
            None
        );
        assert_eq!(DocumentFormat::detect(b"", None), None);
    }

    #[test]
    fn tidy_collapses_spaces_but_keeps_line_structure() {
        assert_eq!(tidy("a   b\n  c  "), "a b\n c");
    }

    #[test]
    fn empty_extraction_is_detectable() {
        let document = NormalizedDocument {
            format: DocumentFormat::Pdf,
            metadata: DocumentMetadata::default(),
            blocks: vec![DocumentBlock::new("  ", BlockKind::Paragraph, 0)],
            warnings: Vec::new(),
        };
        assert!(
            document.looks_empty(),
            "a scan with no text layer must not read as success"
        );
    }
}
