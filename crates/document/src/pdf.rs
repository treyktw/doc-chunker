//! PDF extraction.
//!
//! Page numbers are the reason this file exists. A flat string for a 400-page
//! book retrieves fine and cites uselessly, so text is kept per page and the
//! number is attached before it goes anywhere else.

use super::{BlockKind, DocumentBlock, DocumentFormat, DocumentMetadata, NormalizedDocument, tidy};
use crate::engine::{EngineError, EngineResult};

pub(crate) fn extract(bytes: &[u8]) -> EngineResult<NormalizedDocument> {
    // Parsed once. Extracting per page inside a loop re-parses the whole file
    // every iteration, which turns a linear job into a quadratic one — on a
    // long document that is the difference between seconds and minutes.
    let pages = pdf_extract::extract_text_from_mem_by_pages(bytes).map_err(|error| {
        EngineError::invalid_input("pdf_unreadable", format!("could not read the PDF: {error}"))
    })?;

    let page_count = pages.len() as u32;
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut ordinal = 0usize;
    let mut section: Vec<String> = Vec::new();
    let mut empty_pages = 0u32;

    for (index, raw) in pages.iter().enumerate() {
        // Pages are 1-indexed everywhere a human will see them.
        let page = index as u32 + 1;
        let cleaned = tidy(raw);

        if cleaned.trim().is_empty() {
            empty_pages += 1;
            continue;
        }

        for paragraph in split_paragraphs(&cleaned) {
            let kind = if super::looks_like_heading(&paragraph) {
                // A heading opens a section. Deeper structure is not
                // recoverable from a text layer, so the trail stays one deep
                // rather than guessing at nesting.
                section = vec![paragraph.clone()];
                BlockKind::Heading
            } else {
                BlockKind::Paragraph
            };

            blocks.push(
                DocumentBlock::new(paragraph, kind, ordinal)
                    .with_page(page)
                    .with_section(section.clone()),
            );
            ordinal += 1;
        }
    }

    if empty_pages > 0 {
        warnings.push(format!(
            "{empty_pages} of {page_count} pages carried no text layer; they are probably scans and need OCR"
        ));
    }

    Ok(NormalizedDocument {
        format: DocumentFormat::Pdf,
        metadata: DocumentMetadata {
            page_count: Some(page_count),
            ..Default::default()
        },
        blocks,
        warnings,
    })
}

/// Split a page into paragraphs.
///
/// PDF text layers store lines, not paragraphs. Splitting on them directly
/// turns every wrapped sentence into two blocks, and the second is a fragment
/// like "mns." — retrievable, uncitable, and actively misleading when it comes
/// back as a search result. So lines are rejoined unless something says the
/// paragraph actually ended.
fn split_paragraphs(text: &str) -> Vec<String> {
    let by_blank: Vec<String> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .flat_map(join_wrapped_lines)
        .collect();

    if !by_blank.is_empty() {
        return by_blank;
    }

    join_wrapped_lines(text)
}

/// Rejoin wrapped lines into paragraphs.
fn join_wrapped_lines(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for (index, line) in lines.iter().enumerate() {
        // A line ending in a hyphen is a word broken by wrapping, and a line
        // continuing one is mid-word. Neither can be a heading, however short
        // it looks — "the eigen-" is not a section title.
        let continues_previous = current.ends_with('-');
        let is_broken_word = line.ends_with('-');

        // The same goes for a line wrapped out of a sentence that has not
        // finished. On a narrow page most wrapped lines are short and lack a
        // full stop, so on its own the heading test reads ordinary prose as a
        // stack of section titles. A line inside an unfinished sentence, or
        // one whose next line carries on in lower case, is part of a paragraph.
        let mid_sentence = !current.is_empty()
            && !(current.ends_with('.') || current.ends_with('?') || current.ends_with('!'));
        let next_continues = lines
            .get(index + 1)
            .and_then(|next| next.chars().next())
            .is_some_and(char::is_lowercase);

        // A heading stands alone: it is the boundary the chunker will use.
        if !continues_previous
            && !is_broken_word
            && !mid_sentence
            && !next_continues
            && super::looks_like_heading(line)
        {
            if !current.is_empty() {
                out.push(tidy(&current));
                current.clear();
            }
            out.push(tidy(line));
            continue;
        }

        if !current.is_empty() {
            // A hyphen at a line end is a word broken by wrapping, not a word
            // that contains a hyphen — rejoin without the space or the term
            // becomes unsearchable.
            if current.ends_with('-') {
                current.pop();
            } else {
                current.push(' ');
            }
        }
        current.push_str(line);

        let ends_sentence = line.ends_with('.') || line.ends_with('?') || line.ends_with('!');
        let next_starts_new = lines.get(index + 1).is_some_and(|next| {
            super::looks_like_heading(next) || next.chars().next().is_some_and(|c| c.is_uppercase())
        });

        if ends_sentence && next_starts_new {
            out.push(tidy(&current));
            current.clear();
        }
    }

    if !current.trim().is_empty() {
        out.push(tidy(&current));
    }

    out.into_iter().filter(|part| !part.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_blank_line_paragraphs() {
        let parts = split_paragraphs("First para.\nstill first.\n\nSecond para.");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "First para. still first.");
    }

    #[test]
    fn rejoins_a_sentence_broken_by_line_wrapping() {
        // The exact shape a PDF text layer produces for a wrapped sentence.
        let parts = split_paragraphs(
            "The column space of A is the set of all linear combinations of its\ncolumns.",
        );
        assert_eq!(parts.len(), 1, "a wrapped sentence is one paragraph");
        assert!(parts[0].ends_with("its columns."));
    }

    #[test]
    fn rejoins_a_word_split_across_lines_by_a_hyphen() {
        let parts = split_paragraphs("the eigen-\nvalue is real.");
        assert_eq!(parts[0], "the eigenvalue is real.");
    }

    #[test]
    fn a_heading_still_stands_on_its_own() {
        let parts = split_paragraphs("Column Space\nThe set of reachable vectors.");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "Column Space");
    }

    #[test]
    fn short_wrapped_lines_on_a_narrow_page_are_not_headings() {
        // A5 page, ~10 words a line: every line here passes the heading test
        // when looked at alone.
        let parts = split_paragraphs(
            "Watering\nMost houseplants want the top few centimetres of soil to\ndry out between waterings. Pushing a finger into the pot is\na better guide than a calendar, because light, temperature\nand pot size all change how fast soil dries.",
        );
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[0], "Watering");
        assert!(parts[1].starts_with("Most houseplants"));
        assert!(parts[1].ends_with("how fast soil dries."));
    }

    #[test]
    fn separate_sentences_stay_separate() {
        let parts = split_paragraphs("First thought here.\nSecond thought here.");
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn rejects_a_file_that_is_not_a_pdf() {
        assert!(extract(b"definitely not a pdf").is_err());
    }
}
