//! Markdown and plain text.
//!
//! Markdown is the one format that already says where its boundaries are, so
//! the extractor's job is to preserve that structure rather than flatten it
//! into prose the chunker then has to guess at.

use super::{BlockKind, DocumentBlock, DocumentFormat, DocumentMetadata, NormalizedDocument, tidy};
use crate::engine::{EngineError, EngineResult};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

pub(crate) fn extract(bytes: &[u8]) -> EngineResult<NormalizedDocument> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        EngineError::invalid_input(
            "markdown_not_utf8",
            format!("markdown must be UTF-8: {error}"),
        )
    })?;

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);

    let mut blocks = Vec::new();
    let mut ordinal = 0usize;
    let mut buffer = String::new();
    let mut kind = BlockKind::Paragraph;
    // Heading trail, indexed by level, so a block under `## B` inside `# A`
    // reports both rather than only the nearest.
    let mut trail: Vec<(u8, String)> = Vec::new();
    let mut title: Option<String> = None;
    let mut in_code = false;

    let flush = |buffer: &mut String,
                 kind: BlockKind,
                 trail: &[(u8, String)],
                 out: &mut Vec<DocumentBlock>,
                 ordinal: &mut usize| {
        let text = tidy(buffer);
        buffer.clear();
        if text.is_empty() {
            return;
        }

        let section: Vec<String> = trail.iter().map(|(_, name)| name.clone()).collect();
        out.push(DocumentBlock::new(text, kind, *ordinal).with_section(section));
        *ordinal += 1;
    };

    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut buffer, kind, &trail, &mut blocks, &mut ordinal);
                kind = BlockKind::Heading;
                // Drop any trail entries at or below this level before pushing.
                let depth = heading_depth(level);
                trail.retain(|(existing, _)| *existing < depth);
            }
            Event::End(TagEnd::Heading(level)) => {
                let heading = tidy(&buffer);
                if !heading.is_empty() {
                    if title.is_none() && level == HeadingLevel::H1 {
                        title = Some(heading.clone());
                    }
                    trail.push((heading_depth(level), heading.clone()));
                }
                // The heading is emitted with the trail *including* itself, so
                // it reads as the start of its own section.
                flush(
                    &mut buffer,
                    BlockKind::Heading,
                    &trail,
                    &mut blocks,
                    &mut ordinal,
                );
                kind = BlockKind::Paragraph;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut buffer, kind, &trail, &mut blocks, &mut ordinal);
                in_code = true;
                kind = BlockKind::Code;
            }
            Event::End(TagEnd::CodeBlock) => {
                flush(
                    &mut buffer,
                    BlockKind::Code,
                    &trail,
                    &mut blocks,
                    &mut ordinal,
                );
                in_code = false;
                kind = BlockKind::Paragraph;
            }
            Event::Start(Tag::Item) => {
                flush(&mut buffer, kind, &trail, &mut blocks, &mut ordinal);
                kind = BlockKind::ListItem;
            }
            Event::End(TagEnd::Item) => {
                flush(
                    &mut buffer,
                    BlockKind::ListItem,
                    &trail,
                    &mut blocks,
                    &mut ordinal,
                );
                kind = BlockKind::Paragraph;
            }
            Event::End(TagEnd::Paragraph) => {
                flush(&mut buffer, kind, &trail, &mut blocks, &mut ordinal);
                kind = BlockKind::Paragraph;
            }
            Event::Text(value) | Event::Code(value) => buffer.push_str(&value),
            Event::SoftBreak => buffer.push(' '),
            Event::HardBreak => buffer.push('\n'),
            _ => {
                if in_code {
                    // Keep code layout; collapsing it destroys the thing.
                    buffer.push('\n');
                }
            }
        }
    }

    flush(&mut buffer, kind, &trail, &mut blocks, &mut ordinal);

    Ok(NormalizedDocument {
        format: DocumentFormat::Markdown,
        metadata: DocumentMetadata {
            title,
            ..Default::default()
        },
        blocks,
        warnings: Vec::new(),
    })
}

fn heading_depth(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Plain text: split on blank lines and keep whatever structure exists.
pub(crate) fn extract_plain(bytes: &[u8]) -> NormalizedDocument {
    let text = String::from_utf8_lossy(bytes);

    let blocks: Vec<DocumentBlock> = text
        .split("\n\n")
        .map(tidy)
        .filter(|part| !part.is_empty())
        .enumerate()
        .map(|(ordinal, part)| {
            let kind = if super::looks_like_heading(&part) {
                BlockKind::Heading
            } else {
                BlockKind::Paragraph
            };
            DocumentBlock::new(part, kind, ordinal)
        })
        .collect();

    NormalizedDocument {
        format: DocumentFormat::PlainText,
        metadata: DocumentMetadata::default(),
        blocks,
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_full_heading_trail() {
        let md = b"# Linear Algebra\n\n## Column space\n\nThe set of reachable vectors.\n";
        let document = extract(md).unwrap();

        let body = document
            .blocks
            .iter()
            .find(|block| block.text.starts_with("The set"))
            .expect("body block");

        assert_eq!(
            body.section,
            vec!["Linear Algebra".to_string(), "Column space".to_string()],
            "a block should report every heading above it, not just the nearest"
        );
    }

    #[test]
    fn a_sibling_heading_replaces_rather_than_nests() {
        let md = b"# Top\n\n## A\n\ntext a\n\n## B\n\ntext b\n";
        let document = extract(md).unwrap();

        let b = document
            .blocks
            .iter()
            .find(|block| block.text == "text b")
            .unwrap();
        assert_eq!(b.section, vec!["Top".to_string(), "B".to_string()]);
        assert!(
            !b.section.contains(&"A".to_string()),
            "siblings must not accumulate"
        );
    }

    #[test]
    fn code_blocks_are_marked_as_code() {
        let md = b"# T\n\n```rust\nfn main() {}\n```\n";
        let document = extract(md).unwrap();
        assert!(
            document
                .blocks
                .iter()
                .any(|block| block.kind == BlockKind::Code)
        );
    }

    #[test]
    fn list_items_become_their_own_blocks() {
        let md = b"- first\n- second\n- third\n";
        let document = extract(md).unwrap();
        let items = document
            .blocks
            .iter()
            .filter(|b| b.kind == BlockKind::ListItem)
            .count();
        assert_eq!(items, 3);
    }

    #[test]
    fn the_first_h1_becomes_the_title() {
        let document = extract(b"# Strang Chapter 3\n\nbody\n").unwrap();
        assert_eq!(document.metadata.title.as_deref(), Some("Strang Chapter 3"));
    }

    #[test]
    fn plain_text_splits_on_blank_lines() {
        let document = extract_plain(b"First para.\n\nSecond para.\n\nThird.");
        assert_eq!(document.blocks.len(), 3);
    }
}
