//! File in, citable chunks out.
//!
//! Two crates do the work: `document_extract` turns bytes into positioned
//! blocks, and `chunking` turns blocks into retrieval-sized passages. This
//! crate is the bridge between them, and the one place where an extractor's
//! notion of position is translated into a chunk's.

#![forbid(unsafe_code)]

pub use chunking::{ChunkLocation, ChunkingConfig, DraftChunk, SourceBlock, chunk_blocks};
pub use document_extract::{
    BlockKind, DocumentBlock, DocumentEngine, DocumentFormat, EngineError, EngineResult,
    NormalizedDocument, StandardDocumentEngine,
};

/// Carry every positional fact the extractor found onto a chunker input.
///
/// Page (or slide), the heading trail, and whether the block is a heading
/// all cross over. A table or code block is marked as not a heading, because
/// short lines in either look like headings to a text heuristic and a split
/// mid-table is exactly what the chunker is meant to avoid.
pub fn source_block_from(block: &DocumentBlock) -> SourceBlock {
    let mut source_block = match block.page {
        Some(page) => SourceBlock::with_page(block.text.trim(), page as i32),
        None => SourceBlock::new(block.text.trim()),
    };
    source_block.location.section = block.section.clone();
    source_block.with_heading_hint(block.kind == BlockKind::Heading)
}

/// Extract and chunk in one call.
pub async fn extract_and_chunk(
    bytes: &[u8],
    filename: Option<&str>,
    config: &ChunkingConfig,
) -> EngineResult<(NormalizedDocument, Vec<DraftChunk>)> {
    let document = StandardDocumentEngine::new()
        .normalize(bytes, filename)
        .await?;
    let source_blocks = document
        .blocks
        .iter()
        .map(source_block_from)
        .collect::<Vec<_>>();
    let chunks = chunk_blocks(&source_blocks, config);
    Ok((document, chunks))
}
