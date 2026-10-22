//! DOCX and PPTX.
//!
//! Both are zip archives of XML. Rather than a document-model library for each,
//! the parts are read directly: the text lives in a small, stable set of
//! elements, and going through the raw XML keeps the dependency surface to a
//! zip reader and a pull parser.
//!
//! For PPTX the slide number is the citation target, and slides are numbered by
//! their filename — `slide12.xml` is slide 12 — because archive order is not
//! guaranteed to match presentation order.

use super::{BlockKind, DocumentBlock, DocumentFormat, DocumentMetadata, NormalizedDocument, tidy};
use crate::engine::{EngineError, EngineResult};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::io::{Cursor, Read};

/// Tell DOCX and PPTX apart by what is inside the archive.
pub(crate) fn classify(bytes: &[u8]) -> Option<DocumentFormat> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;

    let mut has_document = false;
    let mut has_slides = false;

    for index in 0..archive.len() {
        let Ok(entry) = archive.by_index(index) else {
            continue;
        };
        let name = entry.name();
        if name == "word/document.xml" {
            has_document = true;
        } else if name.starts_with("ppt/slides/slide") {
            has_slides = true;
        }
    }

    match (has_document, has_slides) {
        (true, _) => Some(DocumentFormat::Docx),
        (_, true) => Some(DocumentFormat::Pptx),
        _ => None,
    }
}

fn read_part(bytes: &[u8], name: &str) -> EngineResult<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| {
        EngineError::invalid_input(
            "ooxml_unreadable",
            format!("not a readable archive: {error}"),
        )
    })?;

    let mut file = archive.by_name(name).map_err(|_| {
        EngineError::invalid_input("ooxml_part_missing", format!("archive has no {name}"))
    })?;

    let mut contents = String::new();
    file.read_to_string(&mut contents).map_err(|error| {
        EngineError::invalid_input(
            "ooxml_part_unreadable",
            format!("could not read {name}: {error}"),
        )
    })?;

    Ok(contents)
}

/// Paragraph-ish runs of text pulled out of an OOXML part.
///
/// `paragraph_tag` closes a block; `break_tag` is a soft line break inside one.
fn paragraphs_from_xml(xml: &str, paragraph_tag: &str, break_tag: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_text = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = local_name(element.name().into_inner());
                if name == "t" {
                    in_text = true;
                } else if name == break_tag {
                    current.push('\n');
                }
            }
            Ok(Event::Empty(element)) => {
                if local_name(element.name().into_inner()) == break_tag {
                    current.push('\n');
                }
            }
            Ok(Event::Text(text)) if in_text => {
                current.push_str(&text.xml10_content());
            }
            Ok(Event::End(element)) => {
                let name = local_name(element.name().into_inner());
                if name == "t" {
                    in_text = false;
                } else if name == paragraph_tag {
                    let finished = tidy(&current);
                    if !finished.is_empty() {
                        out.push(finished);
                    }
                    current.clear();
                }
            }
            Ok(Event::GeneralRef(entity)) if in_text => {
                if let Ok(Some(character)) = entity.resolve_char_ref() {
                    current.push(character);
                } else {
                    let name = entity.xml10_content();
                    if let Some(value) = quick_xml::escape::resolve_predefined_entity(&name) {
                        current.push_str(value);
                    }
                }
            }
            Ok(Event::Eof) => break,
            // A malformed part yields what was recovered rather than nothing:
            // partial text from a damaged deck still answers questions.
            Err(_) => break,
            _ => {}
        }
    }

    let trailing = tidy(&current);
    if !trailing.is_empty() {
        out.push(trailing);
    }

    out
}

/// Strip any namespace prefix: `w:p` and `a:p` both matter as `p`.
fn local_name(raw: &str) -> String {
    raw.rsplit(':').next().unwrap_or(raw).to_string()
}

pub(crate) fn extract_docx(bytes: &[u8]) -> EngineResult<NormalizedDocument> {
    let xml = read_part(bytes, "word/document.xml")?;
    let paragraphs = paragraphs_from_xml(&xml, "p", "br");

    let mut blocks = Vec::new();
    let mut section: Vec<String> = Vec::new();

    for (ordinal, text) in paragraphs.into_iter().enumerate() {
        let kind = if super::looks_like_heading(&text) {
            section = vec![text.clone()];
            BlockKind::Heading
        } else {
            BlockKind::Paragraph
        };

        // DOCX has no fixed pagination — pages depend on the renderer — so no
        // page number is claimed rather than inventing one.
        blocks.push(DocumentBlock::new(text, kind, ordinal).with_section(section.clone()));
    }

    Ok(NormalizedDocument {
        format: DocumentFormat::Docx,
        metadata: DocumentMetadata::default(),
        blocks,
        warnings: Vec::new(),
    })
}

pub(crate) fn extract_pptx(bytes: &[u8]) -> EngineResult<NormalizedDocument> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| {
        EngineError::invalid_input(
            "pptx_unreadable",
            format!("not a readable archive: {error}"),
        )
    })?;

    // Collect slide part names first; the archive is not ordered by slide.
    let mut slide_parts: Vec<(u32, String)> = Vec::new();
    for index in 0..archive.len() {
        let Ok(entry) = archive.by_index(index) else {
            continue;
        };
        let name = entry.name().to_string();
        if let Some(number) = slide_number(&name) {
            slide_parts.push((number, name));
        }
    }
    slide_parts.sort_by_key(|(number, _)| *number);

    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut ordinal = 0usize;
    let mut empty_slides = 0u32;
    let slide_count = slide_parts.len() as u32;

    for (number, part) in slide_parts {
        let mut xml = String::new();
        match archive.by_name(&part) {
            Ok(mut file) => {
                if file.read_to_string(&mut xml).is_err() {
                    warnings.push(format!("slide {number} could not be read"));
                    continue;
                }
            }
            Err(_) => continue,
        }

        let paragraphs = paragraphs_from_xml(&xml, "p", "br");
        if paragraphs.is_empty() {
            // A slide that is purely an image is a real thing, and a candidate
            // for OCR rather than a failure.
            empty_slides += 1;
            continue;
        }

        // The first line of a slide is its title far more often than not, and
        // it makes a good section label for everything under it.
        let section = paragraphs.first().cloned().into_iter().collect::<Vec<_>>();

        for (index, text) in paragraphs.into_iter().enumerate() {
            let kind = if index == 0 {
                BlockKind::Heading
            } else {
                BlockKind::Paragraph
            };
            blocks.push(
                DocumentBlock::new(text, kind, ordinal)
                    .with_page(number)
                    .with_section(section.clone()),
            );
            ordinal += 1;
        }
    }

    if empty_slides > 0 {
        warnings.push(format!(
            "{empty_slides} of {slide_count} slides carried no text; they are probably images and need OCR"
        ));
    }

    Ok(NormalizedDocument {
        format: DocumentFormat::Pptx,
        metadata: DocumentMetadata {
            page_count: Some(slide_count),
            ..Default::default()
        },
        blocks,
        warnings,
    })
}

/// `ppt/slides/slide12.xml` → 12.
fn slide_number(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("ppt/slides/slide")?;
    rest.strip_suffix(".xml")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slide_numbers_come_from_the_filename() {
        assert_eq!(slide_number("ppt/slides/slide12.xml"), Some(12));
        assert_eq!(slide_number("ppt/slides/_rels/slide1.xml.rels"), None);
        assert_eq!(slide_number("word/document.xml"), None);
    }

    #[test]
    fn namespace_prefixes_are_stripped() {
        assert_eq!(local_name("w:p"), "p");
        assert_eq!(local_name("a:t"), "t");
        assert_eq!(local_name("p"), "p");
    }

    #[test]
    fn extracts_paragraphs_across_runs() {
        // Word splits a sentence across runs whenever formatting changes; the
        // extractor has to rejoin them or every bolded word breaks a sentence.
        let xml = r#"<w:document><w:body>
            <w:p><w:r><w:t>The column </w:t></w:r><w:r><w:t>space</w:t></w:r></w:p>
            <w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p>
        </w:body></w:document>"#;

        let paragraphs = paragraphs_from_xml(xml, "p", "br");
        assert_eq!(paragraphs, vec!["The column space", "Second paragraph."]);
    }

    #[test]
    fn a_soft_break_stays_inside_its_paragraph() {
        let xml = r#"<w:p><w:r><w:t>one</w:t></w:r><w:br/><w:r><w:t>two</w:t></w:r></w:p>"#;
        let paragraphs = paragraphs_from_xml(xml, "p", "br");
        assert_eq!(paragraphs.len(), 1);
        assert!(paragraphs[0].contains("one"));
        assert!(paragraphs[0].contains("two"));
    }

    #[test]
    fn recovers_what_it_can_from_malformed_xml() {
        let xml = r#"<w:p><w:r><w:t>salvageable</w:t></w:r></w:p><w:p><w:r><w:t>truncat"#;
        let paragraphs = paragraphs_from_xml(xml, "p", "br");
        assert!(
            paragraphs
                .first()
                .is_some_and(|first| first == "salvageable")
        );
    }
}
