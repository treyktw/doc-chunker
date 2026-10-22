//! HTML.
//!
//! Web pages are mostly not the article: navigation, cookie banners, footers,
//! related-post rails. Indexing that noise means retrieval competes against
//! boilerplate that appears on every page of a site, so `<script>`, `<style>`
//! and `<nav>` are dropped outright rather than tidied afterwards.

use super::{BlockKind, DocumentBlock, DocumentFormat, DocumentMetadata, NormalizedDocument, tidy};
use crate::engine::{EngineError, EngineResult};
use quick_xml::Reader;
use quick_xml::events::Event;

/// Elements whose text is never content.
const DROPPED: &[&str] = &["script", "style", "nav", "noscript", "svg"];

/// Elements that end a block of text.
const BLOCK_LEVEL: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "li",
    "td",
    "th",
    "tr",
    "blockquote",
    "pre",
    "br",
    "hr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
];

pub(crate) fn extract(bytes: &[u8]) -> EngineResult<NormalizedDocument> {
    let source = std::str::from_utf8(bytes).map_err(|error| {
        EngineError::invalid_input("html_not_utf8", format!("HTML must be UTF-8: {error}"))
    })?;

    let mut reader = Reader::from_str(source);
    let config = reader.config_mut();
    config.trim_text(false);
    // Real-world HTML is not well-formed XML; unclosed tags are the norm.
    config.check_end_names = false;

    let mut blocks = Vec::new();
    let mut ordinal = 0usize;
    let mut buffer = String::new();
    let mut title: Option<String> = None;
    let mut trail: Vec<(u8, String)> = Vec::new();

    // Nesting depth inside dropped elements, so `<nav><div>…` stays dropped.
    let mut suppressed = 0usize;
    let mut pending_heading: Option<u8> = None;
    let mut in_title = false;

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
        let section = trail.iter().map(|(_, name)| name.clone()).collect();
        out.push(DocumentBlock::new(text, kind, *ordinal).with_section(section));
        *ordinal += 1;
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = tag_name(element.name().into_inner());

                if DROPPED.contains(&name.as_str()) {
                    suppressed += 1;
                    continue;
                }
                if suppressed > 0 {
                    continue;
                }

                if name == "title" {
                    in_title = true;
                    continue;
                }

                if let Some(depth) = heading_depth(&name) {
                    flush(
                        &mut buffer,
                        BlockKind::Paragraph,
                        &trail,
                        &mut blocks,
                        &mut ordinal,
                    );
                    pending_heading = Some(depth);
                } else if BLOCK_LEVEL.contains(&name.as_str()) {
                    flush(
                        &mut buffer,
                        BlockKind::Paragraph,
                        &trail,
                        &mut blocks,
                        &mut ordinal,
                    );
                }
            }

            Ok(Event::End(element)) => {
                let name = tag_name(element.name().into_inner());

                if DROPPED.contains(&name.as_str()) {
                    suppressed = suppressed.saturating_sub(1);
                    continue;
                }
                if suppressed > 0 {
                    continue;
                }

                if name == "title" {
                    in_title = false;
                    let value = tidy(&buffer);
                    if !value.is_empty() {
                        title = Some(value);
                    }
                    buffer.clear();
                    continue;
                }

                if let Some(depth) = heading_depth(&name) {
                    let heading = tidy(&buffer);
                    if !heading.is_empty() {
                        trail.retain(|(existing, _)| *existing < depth);
                        trail.push((depth, heading));
                    }
                    flush(
                        &mut buffer,
                        BlockKind::Heading,
                        &trail,
                        &mut blocks,
                        &mut ordinal,
                    );
                    pending_heading = None;
                } else if BLOCK_LEVEL.contains(&name.as_str()) {
                    flush(
                        &mut buffer,
                        BlockKind::Paragraph,
                        &trail,
                        &mut blocks,
                        &mut ordinal,
                    );
                }
            }

            Ok(Event::Empty(element)) => {
                let name = tag_name(element.name().into_inner());
                if suppressed == 0 && BLOCK_LEVEL.contains(&name.as_str()) {
                    flush(
                        &mut buffer,
                        BlockKind::Paragraph,
                        &trail,
                        &mut blocks,
                        &mut ordinal,
                    );
                }
            }

            Ok(Event::Text(text)) => {
                if suppressed > 0 {
                    continue;
                }
                // HTML entity rules, not XML ones: &nbsp; and friends only
                // resolve under the HTML set, and a page full of literal
                // "&amp;" is a page that reads wrong everywhere it is cited.
                let value = text.html_content();
                buffer.push_str(&value);
                if !in_title && pending_heading.is_none() {
                    // Keep words apart across inline element boundaries.
                    buffer.push(' ');
                }
            }

            // Entities arrive as their own events, not inside the text.
            // Dropping them silently deletes every &amp;, &lt; and &nbsp; in
            // the document — a corruption that shows up only in the citation.
            Ok(Event::GeneralRef(entity)) => {
                if suppressed == 0 {
                    buffer.push_str(&resolve_entity(&entity));
                }
            }

            Ok(Event::Eof) => break,
            // Recover whatever parsed; a broken page still has usable content.
            Err(_) => break,
            _ => {}
        }
    }

    flush(
        &mut buffer,
        BlockKind::Paragraph,
        &trail,
        &mut blocks,
        &mut ordinal,
    );

    Ok(NormalizedDocument {
        format: DocumentFormat::Html,
        metadata: DocumentMetadata {
            title,
            ..Default::default()
        },
        blocks,
        warnings: Vec::new(),
    })
}

/// Resolve one entity reference to the text it stands for.
///
/// `html_content()` returns the reference's *name*, not its value, so using it
/// directly turns `&amp;` into the literal word "amp" — a corruption that is
/// invisible until someone reads a citation.
fn resolve_entity(entity: &quick_xml::events::BytesRef<'_>) -> String {
    // Numeric references: &#8212; and &#x2014;
    if let Ok(Some(character)) = entity.resolve_char_ref() {
        return character.to_string();
    }

    let name = entity.html_content();
    match quick_xml::escape::resolve_predefined_entity(&name) {
        Some(value) => value.to_string(),
        // A few HTML entities are not XML-predefined but appear constantly.
        None => match name.as_ref() {
            "nbsp" => " ".to_string(),
            "mdash" => "\u{2014}".to_string(),
            "ndash" => "\u{2013}".to_string(),
            "hellip" => "\u{2026}".to_string(),
            "rsquo" | "apos" => "\u{2019}".to_string(),
            "lsquo" => "\u{2018}".to_string(),
            "ldquo" => "\u{201C}".to_string(),
            "rdquo" => "\u{201D}".to_string(),
            // Unknown: keep the source form rather than inventing text.
            other => format!("&{other};"),
        },
    }
}

fn tag_name(raw: &str) -> String {
    raw.to_ascii_lowercase()
}

fn heading_depth(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_navigation_and_scripts() {
        let html = br#"<html><head><title>Notes</title></head><body>
            <nav><a href="/">Home</a><a href="/about">About</a></nav>
            <script>var tracking = 1;</script>
            <p>The column space is the set of reachable vectors.</p>
        </body></html>"#;

        let document = extract(html).unwrap();
        let all: String = document.blocks.iter().map(|b| b.text.clone()).collect();

        assert!(all.contains("column space"));
        assert!(!all.contains("Home"), "navigation is not content");
        assert!(!all.contains("tracking"), "scripts are never content");
    }

    #[test]
    fn takes_the_title_from_the_head() {
        let html = b"<html><head><title>Strang Ch.3</title></head><body><p>x</p></body></html>";
        assert_eq!(
            extract(html).unwrap().metadata.title.as_deref(),
            Some("Strang Ch.3")
        );
    }

    #[test]
    fn headings_build_a_section_trail() {
        let html = b"<body><h1>Top</h1><h2>Inner</h2><p>body text here</p></body>";
        let document = extract(html).unwrap();
        let body = document
            .blocks
            .iter()
            .find(|b| b.text.contains("body text"))
            .unwrap();
        assert_eq!(body.section, vec!["Top".to_string(), "Inner".to_string()]);
    }

    #[test]
    fn survives_unclosed_tags() {
        // Browsers accept this; so must we.
        let html = b"<body><p>first<p>second<div>third</body>";
        let document = extract(html).unwrap();
        let all: String = document.blocks.iter().map(|b| b.text.clone()).collect();
        assert!(all.contains("first") && all.contains("second") && all.contains("third"));
    }

    #[test]
    fn entities_are_decoded() {
        let document = extract(b"<body><p>a &amp; b &lt; c</p></body>").unwrap();
        assert!(document.blocks[0].text.contains("a & b < c"));
    }
}
