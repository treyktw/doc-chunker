//! End-to-end over the checked-in samples: every format extracts, keeps its
//! positions, and chunks along its own structure.
//!
//! `pdf-extract` is pure Rust, so nothing here needs a system library.

use doc_chunker::{
    BlockKind, ChunkingConfig, DocumentFormat, DraftChunk, NormalizedDocument, extract_and_chunk,
};

async fn run(name: &str) -> (NormalizedDocument, Vec<DraftChunk>) {
    let path = format!("{}/samples/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    extract_and_chunk(&bytes, Some(name), &ChunkingConfig::default())
        .await
        .unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn sections(chunks: &[DraftChunk]) -> Vec<String> {
    chunks
        .iter()
        .map(|chunk| chunk.location.section.join(" > "))
        .collect()
}

#[tokio::test]
async fn pdf_keeps_page_numbers_and_rejoins_wrapped_lines() {
    let (document, chunks) = run("plant-care.pdf").await;
    assert_eq!(document.format, DocumentFormat::Pdf);
    assert_eq!(document.metadata.page_count, Some(3));
    assert!(document.warnings.is_empty(), "{:?}", document.warnings);

    // Wrapped lines on a narrow page must not turn into headings.
    let headings = document
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Heading)
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        headings,
        ["Houseplant Care Handbook", "Watering", "Light", "Repotting"]
    );

    let pages = chunks
        .iter()
        .map(|chunk| chunk.location.page)
        .collect::<Vec<_>>();
    assert_eq!(pages, [Some(1), Some(2), Some(3)]);
    assert_eq!(sections(&chunks), ["Watering", "Light", "Repotting"]);
    assert!(chunks.iter().all(|chunk| chunk.structural_start));
}

#[tokio::test]
async fn docx_is_detected_by_content_and_split_at_headings() {
    let (document, chunks) = run("plant-care.docx").await;
    assert_eq!(document.format, DocumentFormat::Docx);
    assert_eq!(sections(&chunks), ["Watering", "Light", "Repotting"]);
    // DOCX has no pagination, and the extractor says so rather than
    // defaulting to page 1.
    assert!(chunks.iter().all(|chunk| chunk.location.page.is_none()));
}

#[tokio::test]
async fn pptx_uses_slide_numbers_as_pages() {
    let (document, chunks) = run("plant-care.pptx").await;
    assert_eq!(document.format, DocumentFormat::Pptx);
    assert_eq!(document.metadata.page_count, Some(4));
    let pages = chunks
        .iter()
        .map(|chunk| chunk.location.page)
        .collect::<Vec<_>>();
    assert_eq!(pages, [Some(1), Some(2), Some(3), Some(4)]);
    assert_eq!(chunks[3].location.section, vec!["Pests".to_string()]);
}

#[tokio::test]
async fn html_drops_boilerplate_and_keeps_the_heading_trail() {
    let (document, chunks) = run("plant-care.html").await;
    assert_eq!(document.format, DocumentFormat::Html);
    assert_eq!(
        document.metadata.title.as_deref(),
        Some("Repotting Houseplants")
    );
    let all_text = document
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!all_text.contains("analytics"), "script text leaked");
    assert!(!all_text.contains("Shop"), "nav text leaked");
    assert_eq!(
        sections(&chunks),
        [
            "Repotting Houseplants",
            "Repotting Houseplants > Choosing a pot",
            "Repotting Houseplants > After repotting",
        ]
    );
}

#[tokio::test]
async fn markdown_list_items_do_not_split_chunks_and_trails_stay_deep() {
    let (document, chunks) = run("plant-care.md").await;
    assert_eq!(document.format, DocumentFormat::Markdown);
    assert_eq!(
        document.metadata.title.as_deref(),
        Some("Houseplant Care Guide")
    );
    assert!(
        document
            .blocks
            .iter()
            .any(|block| block.kind == BlockKind::Code)
    );
    assert_eq!(
        sections(&chunks),
        [
            "Houseplant Care Guide",
            "Houseplant Care Guide > Watering",
            "Houseplant Care Guide > Watering > Signs of overwatering",
            "Houseplant Care Guide > Light",
            "Houseplant Care Guide > A watering log",
        ]
    );
    // All three list items and the paragraph after them stay in one chunk:
    // the extractor marked them as not-headings, overriding the guess.
    let overwatering = &chunks[2];
    assert!(overwatering.content.contains("Fungus gnats"));
    assert!(overwatering.content.contains("rotten roots"));
}

#[tokio::test]
async fn small_targets_produce_size_boundaries_with_overlap_only_in_embed_text() {
    let path = format!("{}/samples/plant-care.md", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(path).unwrap();
    let config = ChunkingConfig {
        target_chars: 120,
        max_chars: 200,
        overlap_chars: 80,
        min_chars: 40,
    };
    let (_, chunks) = extract_and_chunk(&bytes, Some("plant-care.md"), &config)
        .await
        .unwrap();
    let size_chunks = chunks
        .iter()
        .filter(|chunk| !chunk.structural_start)
        .collect::<Vec<_>>();
    assert!(!size_chunks.is_empty(), "expected at least one size split");
    for chunk in &size_chunks {
        assert!(chunk.embed_text.len() > chunk.content.len());
        assert!(chunk.embed_text.ends_with(&chunk.content));
    }
    for chunk in chunks.iter().filter(|chunk| chunk.structural_start) {
        assert_eq!(chunk.embed_text, chunk.content);
    }
}

#[tokio::test]
async fn unknown_binary_is_refused_with_a_stable_code() {
    let error = extract_and_chunk(&[0xff, 0x00, 0xfe, 0x01], None, &ChunkingConfig::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, "document_format_unknown");
}
