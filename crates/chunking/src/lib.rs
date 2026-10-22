//! Chunking.
//!
//! The unit of retrieval is a passage a person could read and judge, not a
//! fixed byte window. Splitting mid-sentence produces chunks that retrieve
//! well and cite badly, so boundaries follow the document's own structure
//! first and fall back to sentences only when a section is too large.

mod location;

pub use location::ChunkLocation;

#[derive(Debug, Clone)]
pub struct ChunkingConfig {
    /// Target size in characters. Roughly 4 chars per token for English prose.
    pub target_chars: usize,
    pub max_chars: usize,
    /// Overlap carried into the next chunk so a claim spanning a boundary is
    /// still retrievable from either side.
    pub overlap_chars: usize,
    pub min_chars: usize,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            // Sized so a chunk holds one idea, not four. Larger chunks retrieve
            // worse, not better: the embedding averages every topic inside the
            // window, so a chunk spanning four concepts is close to none of
            // them. Recall is recovered with overlap and by returning
            // neighbours, not by widening the window.
            target_chars: 600,
            max_chars: 1_100,
            overlap_chars: 120,
            min_chars: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DraftChunk {
    /// What the reader sees and what the model is given. Clean — no overlap.
    pub content: String,
    /// What gets embedded. Carries the previous chunk's tail so a claim
    /// spanning a boundary is retrievable from either side.
    ///
    /// Kept separate from `content` on purpose: overlap helps the vector and
    /// actively hurts everything else. Folding it into the stored text makes
    /// every citation open mid-sentence and feeds the model duplicated prose.
    pub embed_text: String,
    pub location: ChunkLocation,
    /// True when this chunk began at a boundary the document itself declared —
    /// a heading or a page break — rather than one imposed by the size cap.
    pub structural_start: bool,
}

/// Whether a block reads as a heading rather than prose.
///
/// Documents tell you where their own boundaries are, and a heading is the
/// strongest signal available: it marks the start of a new idea. Merging
/// across one is how a chunk ends up covering four unrelated topics.
fn looks_like_heading(text: &str) -> bool {
    let trimmed = text.trim();

    if trimmed.is_empty() || trimmed.len() > 80 {
        return false;
    }

    // Markdown and numbered headings are unambiguous.
    if trimmed.starts_with('#') {
        return true;
    }

    // Otherwise: short, few words, and not a sentence.
    let ends_like_prose =
        trimmed.ends_with('.') || trimmed.ends_with('?') || trimmed.ends_with('!');
    let word_count = trimmed.split_whitespace().count();

    !ends_like_prose && word_count <= 10
}

/// A block of source text with whatever positional information the parser knew.
#[derive(Debug, Clone)]
pub struct SourceBlock {
    pub text: String,
    pub location: ChunkLocation,
    /// Whether the parser that produced this block knows it is a heading.
    ///
    /// `None` means nobody said, and the chunker falls back to guessing from
    /// the text. An extractor that read real structure (Markdown `##`, HTML
    /// `<h2>`, a DOCX heading style) should say so: a list item like "Yellow
    /// leaves that feel soft" passes the guess and is not a heading, and a
    /// twelve-word heading fails it and is one.
    pub heading: Option<bool>,
}

impl SourceBlock {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            location: ChunkLocation::default(),
            heading: None,
        }
    }

    /// A block spoken at a point in a recording.
    ///
    /// The timestamp is what makes a spoken claim citable: a chunk carries the
    /// second its first block was said, so a citation can open the recording
    /// there rather than at the top.
    pub fn with_timestamp(text: impl Into<String>, seconds: f64) -> Self {
        Self {
            text: text.into(),
            location: ChunkLocation {
                timestamp: Some(seconds),
                ..Default::default()
            },
            heading: None,
        }
    }

    /// A block from one document inside a source that holds several.
    ///
    /// A Notion page folded into its parent stops being its own source, and
    /// without this a citation into it would resolve to the parent and lose
    /// which page it came from. The title goes in the section trail and the
    /// page's own address in `url`, so a citation still reads "Project Plan ->
    /// Phase 3" and still opens the right page.
    pub fn within_document(
        text: impl Into<String>,
        title: impl Into<String>,
        path: impl Into<String>,
        url: Option<String>,
    ) -> Self {
        let title = title.into();
        Self {
            text: text.into(),
            location: ChunkLocation {
                path: Some(path.into()),
                url,
                section: if title.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![title]
                },
                ..Default::default()
            },
            heading: None,
        }
    }

    /// Record what the extractor knew about this block's role.
    pub fn with_heading_hint(mut self, heading: bool) -> Self {
        self.heading = Some(heading);
        self
    }

    pub fn with_page(text: impl Into<String>, page: i32) -> Self {
        Self {
            text: text.into(),
            location: ChunkLocation {
                page: Some(page),
                ..Default::default()
            },
            heading: None,
        }
    }
}

/// Split blocks into chunks, preserving each block's location.
///
/// Blocks are merged until they approach `target_chars`; a block larger than
/// `max_chars` on its own is split on sentence boundaries. Locations are never
/// merged across blocks with different pages — a chunk that spans a page break
/// reports the page it started on, which is where a reader should be sent.
pub fn chunk_blocks(blocks: &[SourceBlock], config: &ChunkingConfig) -> Vec<DraftChunk> {
    let mut chunks = Vec::new();
    let mut buffer = String::new();
    let mut buffer_location: Option<ChunkLocation> = None;
    let mut buffer_structural = true;
    // True while the buffer holds nothing but a heading. A heading is never
    // flushed on its own: it is shorter than `min_chars`, so flushing it
    // would silently drop it, and the passage it introduces would start a
    // "size" chunk that borrows overlap from the previous section.
    let mut buffer_heading_only = false;

    let flush = |buffer: &mut String,
                 location: &mut Option<ChunkLocation>,
                 structural: &mut bool,
                 out: &mut Vec<DraftChunk>| {
        let text = buffer.trim();
        if text.len() >= config.min_chars {
            out.push(DraftChunk {
                content: text.to_string(),
                embed_text: text.to_string(),
                location: location.clone().unwrap_or_default(),
                structural_start: *structural,
            });
        }
        buffer.clear();
        *location = None;
        *structural = false;
    };

    for block in blocks {
        let text = block.text.trim();
        if text.is_empty() {
            continue;
        }

        // Oversized block: split it alone rather than dragging neighbours in.
        if text.len() > config.max_chars {
            // A pending heading belongs to this block, not to nothing: it
            // leads the first piece, which then starts at the heading's
            // structural boundary and carries its section.
            let pending_heading = if buffer_heading_only {
                buffer_heading_only = false;
                let heading_text = std::mem::take(&mut buffer);
                Some((heading_text, buffer_location.take(), buffer_structural))
            } else {
                flush(
                    &mut buffer,
                    &mut buffer_location,
                    &mut buffer_structural,
                    &mut chunks,
                );
                None
            };
            for (piece_index, piece) in split_long(text, config).into_iter().enumerate() {
                match (&pending_heading, piece_index) {
                    (Some((heading_text, heading_location, structural)), 0) => {
                        let content = format!("{}\n\n{piece}", heading_text.trim());
                        chunks.push(DraftChunk {
                            embed_text: content.clone(),
                            content,
                            location: heading_location
                                .clone()
                                .unwrap_or_else(|| block.location.clone()),
                            structural_start: *structural,
                        });
                    }
                    _ => chunks.push(DraftChunk {
                        embed_text: piece.clone(),
                        content: piece,
                        location: block.location.clone(),
                        // Split by size, so overlap is warranted here.
                        structural_start: false,
                    }),
                }
            }
            buffer_structural = false;
            continue;
        }

        let page_changed = matches!(
            (&buffer_location, &block.location),
            (Some(current), incoming) if current.page.is_some() && current.page != incoming.page
        );

        // Speech has no headings. The heuristic looks for something short,
        // with few words, that does not end like a sentence -- which describes
        // a great many spoken fragments: "you're there, subscribe, and as
        // always,". Treating one as a heading puts a sentence in the section
        // trail, and worse, breaks the chunk there, cutting a transcript at an
        // arbitrary phrase instead of where the speaker changed subject.
        let heading = match block.heading {
            Some(known) => known,
            None => block.location.timestamp.is_none() && looks_like_heading(text),
        };

        let over_target = !buffer_heading_only && buffer.len() + text.len() > config.target_chars;
        if !buffer.is_empty() && (heading || page_changed || over_target) {
            flush(
                &mut buffer,
                &mut buffer_location,
                &mut buffer_structural,
                &mut chunks,
            );
        }

        if buffer.is_empty() {
            buffer_structural = heading || page_changed;
            buffer_heading_only = heading;
            let mut location = block.location.clone();
            // Carry the heading as section context so a retrieved chunk can say
            // where in the document it came from.
            if heading {
                let heading_text = text.trim_start_matches('#').trim().to_string();
                // An extractor that already tracked the full trail (`Guide >
                // Watering > Signs of overwatering`) knows more than the
                // one-deep guess here, so its trail is kept when it ends in
                // this heading.
                if location.section.last() != Some(&heading_text) {
                    location.section = vec![heading_text];
                }
            }
            buffer_location = Some(location);
        } else {
            buffer.push_str("\n\n");
            buffer_heading_only = false;
        }
        buffer.push_str(text);
    }

    flush(
        &mut buffer,
        &mut buffer_location,
        &mut buffer_structural,
        &mut chunks,
    );
    apply_overlap(chunks, config)
}

/// Sentence-aware split for a block that exceeds `max_chars` by itself.
fn split_long(text: &str, config: &ChunkingConfig) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();

    for sentence in split_sentences(text) {
        if !current.is_empty() && current.len() + sentence.len() > config.target_chars {
            out.push(current.trim().to_string());
            current.clear();
        }

        // A single sentence longer than the cap: hard-wrap on whitespace.
        if sentence.len() > config.max_chars {
            for word in sentence.split_whitespace() {
                if current.len() + word.len() + 1 > config.target_chars {
                    out.push(current.trim().to_string());
                    current.clear();
                }
                current.push_str(word);
                current.push(' ');
            }
            continue;
        }

        current.push_str(sentence);
    }

    let tail = current.trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// Deliberately simple: split after `.`/`?`/`!` followed by whitespace.
/// Good enough for chunk boundaries, and it never needs to be perfect —
/// a slightly wrong boundary costs recall, not correctness.
fn split_sentences(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;

    for index in 0..bytes.len() {
        let terminal = matches!(bytes[index], b'.' | b'?' | b'!');
        let followed_by_space = bytes
            .get(index + 1)
            .is_some_and(|next| next.is_ascii_whitespace());

        if terminal && followed_by_space && text.is_char_boundary(index + 2) {
            out.push(&text[start..index + 2]);
            start = index + 2;
        }
    }

    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Prefix each chunk with the tail of the previous one.
///
/// Overlap is what stops a definition split across a boundary from becoming
/// unretrievable — both halves carry enough context to match.
fn apply_overlap(chunks: Vec<DraftChunk>, config: &ChunkingConfig) -> Vec<DraftChunk> {
    if config.overlap_chars == 0 || chunks.len() < 2 {
        return chunks;
    }

    let mut out = Vec::with_capacity(chunks.len());
    for (index, chunk) in chunks.iter().enumerate() {
        // A heading or page break is a boundary the document asserted. Bleeding
        // the previous section into this one's vector makes it match queries
        // about a topic it does not cover — the neighbouring chunk already
        // answers those, and better.
        if index == 0 || chunk.structural_start {
            out.push(chunk.clone());
            continue;
        }

        let previous = &chunks[index - 1].content;
        let take_from = previous
            .char_indices()
            .rev()
            .take(config.overlap_chars)
            .last()
            .map(|(byte, _)| byte)
            .unwrap_or(0);

        // Start the overlap at a word boundary; a half word helps nothing.
        let tail = previous[take_from..]
            .split_once(char::is_whitespace)
            .map(|(_, rest)| rest)
            .unwrap_or("")
            .trim();

        out.push(DraftChunk {
            content: chunk.content.clone(),
            embed_text: if tail.is_empty() {
                chunk.content.clone()
            } else {
                format!("{tail}\n\n{}", chunk.content)
            },
            location: chunk.location.clone(),
            structural_start: chunk.structural_start,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heading_boundary_carries_no_overlap() {
        let blocks = vec![
            SourceBlock::new("Eigenvalues"),
            SourceBlock::new("A vector is stretched but not rotated by the transformation."),
            SourceBlock::new("Determinants"),
            SourceBlock::new("The determinant measures how much a transformation scales volume."),
        ];
        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());

        assert_eq!(chunks.len(), 2);
        assert!(
            !chunks[1].embed_text.contains("stretched"),
            "a heading boundary must not bleed the previous topic into this vector"
        );
        assert_eq!(chunks[1].embed_text, chunks[1].content);
    }

    #[test]
    fn overlap_stays_out_of_stored_content() {
        let blocks: Vec<_> = (0..6)
            .map(|i| {
                SourceBlock::new(format!(
                    "Paragraph {i}. {}",
                    "Filler sentence here. ".repeat(12)
                ))
            })
            .collect();
        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());

        assert!(chunks.len() > 1);
        for (index, chunk) in chunks.iter().enumerate().skip(1) {
            assert!(
                chunk.embed_text.len() > chunk.content.len(),
                "chunk {index} should embed more than it stores"
            );
            assert!(
                chunk.embed_text.ends_with(&chunk.content),
                "the stored content must be the tail of what was embedded"
            );
        }
    }

    #[test]
    fn starts_a_new_chunk_at_a_heading() {
        let blocks = vec![
            SourceBlock::new("Column space"),
            SourceBlock::new("The column space is every reachable vector. ".repeat(3)),
            SourceBlock::new("Null space"),
            SourceBlock::new("The null space collapses to zero. ".repeat(3)),
        ];
        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());

        assert_eq!(chunks.len(), 2, "each heading should begin a chunk");
        assert_eq!(chunks[0].location.section, vec!["Column space".to_string()]);
        assert_eq!(chunks[1].location.section, vec!["Null space".to_string()]);
        assert!(!chunks[0].content.contains("null space"));
    }

    #[test]
    fn treats_a_sentence_as_prose_not_a_heading() {
        assert!(!looks_like_heading("This is a short sentence."));
        assert!(looks_like_heading("Column space"));
        assert!(looks_like_heading("## Rank and nullity"));
        assert!(!looks_like_heading(&"word ".repeat(20)));
    }

    #[test]
    fn merges_small_blocks_up_to_target() {
        let blocks: Vec<_> = (0..10)
            .map(|i| SourceBlock::new(format!("Block number {i}. ")))
            .collect();
        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());
        assert_eq!(chunks.len(), 1, "small blocks should merge into one chunk");
    }

    #[test]
    fn starts_a_new_chunk_when_the_page_changes() {
        let blocks = vec![
            SourceBlock::with_page("Content on the first page.".repeat(4), 1),
            SourceBlock::with_page("Content on the second page.".repeat(4), 2),
        ];
        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].location.page, Some(1));
        assert_eq!(chunks[1].location.page, Some(2));
    }

    #[test]
    fn splits_a_block_larger_than_the_cap() {
        let long = "This is a sentence that repeats. ".repeat(200);
        let chunks = chunk_blocks(&[SourceBlock::new(long)], &ChunkingConfig::default());
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.content.len() <= 2_600));
    }

    #[test]
    fn preserves_page_through_a_long_split() {
        let long = "Sentence about linear algebra. ".repeat(150);
        let chunks = chunk_blocks(
            &[SourceBlock::with_page(long, 42)],
            &ChunkingConfig::default(),
        );
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.location.page == Some(42)));
    }
}

#[cfg(test)]
mod timestamp_tests {
    use super::*;

    #[test]
    fn a_chunk_carries_the_second_its_first_line_was_spoken() {
        // Transcript segments are short, so many become one chunk. The chunk
        // has to point at where it started, or a citation opens the recording
        // at the wrong moment -- which is worse than not citing at all.
        let blocks = vec![
            SourceBlock::with_timestamp("The column space of a matrix", 10.0),
            SourceBlock::with_timestamp("is the set of all reachable vectors.", 14.5),
            SourceBlock::with_timestamp("Its dimension is called the rank.", 19.0),
        ];

        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());
        assert!(!chunks.is_empty(), "timestamped blocks produced no chunks");
        assert_eq!(
            chunks[0].location.timestamp,
            Some(10.0),
            "the chunk should point at where it began, not where it ended"
        );
    }
}

#[cfg(test)]
mod transcript_structure_tests {
    use super::*;

    #[test]
    fn spoken_fragments_are_not_headings() {
        // Every one of these passes the heading heuristic on its own: short,
        // few words, no sentence-ending punctuation. In a transcript they are
        // just how people talk.
        let blocks = vec![
            SourceBlock::with_timestamp("The column space of a matrix", 10.0),
            SourceBlock::with_timestamp("you're there, subscribe, and as always,", 14.0),
            SourceBlock::with_timestamp("its dimension is called the rank", 18.0),
        ];

        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());

        // One chunk, because nothing here is a boundary.
        assert_eq!(chunks.len(), 1, "a transcript was split at a spoken phrase");
        assert!(
            chunks[0].location.section.is_empty(),
            "a spoken line became a section heading: {:?}",
            chunks[0].location.section
        );
    }

    #[test]
    fn documents_still_get_their_headings() {
        // The same heuristic is right for text that has structure.
        let blocks = vec![
            SourceBlock::new("# Column space"),
            SourceBlock::new("The set of all reachable vectors, which is what makes it useful."),
        ];

        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());
        assert_eq!(chunks[0].location.section, vec!["Column space".to_string()]);
    }
}

#[cfg(test)]
mod document_scope_tests {
    use super::*;

    /// A page folded into its parent stops being its own source. Without the
    /// title and address on the chunk, a citation resolves to the parent and
    /// loses which page it came from.
    #[test]
    fn a_folded_page_keeps_its_own_name_and_address() {
        let block = SourceBlock::within_document(
            "Phase 3 covers retrieval.",
            "Phase 3",
            "notion/page-3",
            Some("https://www.notion.so/page-3".into()),
        );

        assert_eq!(block.location.section, vec!["Phase 3".to_string()]);
        assert_eq!(block.location.path.as_deref(), Some("notion/page-3"));
        assert_eq!(
            block.location.url.as_deref(),
            Some("https://www.notion.so/page-3")
        );
    }

    /// An untitled page contributes no section rather than an empty heading --
    /// a blank entry in a citation trail reads as a missing one.
    #[test]
    fn an_untitled_page_adds_no_section() {
        let block = SourceBlock::within_document("text", "   ", "notion/x", None);
        assert!(block.location.section.is_empty());
        assert_eq!(block.location.path.as_deref(), Some("notion/x"));
    }

    /// The chunker carries a block's location onto the chunks it produces, so
    /// the identity survives chunking rather than stopping at the block.
    #[test]
    fn chunks_inherit_the_document_they_came_from() {
        let block = SourceBlock::within_document(
            "Retrieval is hybrid. ".repeat(80),
            "Phase 3",
            "notion/page-3",
            None,
        );
        let chunks = chunk_blocks(&[block], &ChunkingConfig::default());

        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert_eq!(chunk.location.path.as_deref(), Some("notion/page-3"));
        }
    }
}

#[cfg(test)]
mod heading_hint_tests {
    use super::*;

    /// A list item reads like a heading to the text heuristic. When the
    /// extractor says it is not one, it must not split the chunk or become a
    /// section.
    #[test]
    fn an_extractor_hint_overrides_the_heading_guess() {
        let blocks = vec![
            SourceBlock::new("Watering").with_heading_hint(true),
            SourceBlock::new("Let the top of the soil dry out between waterings.")
                .with_heading_hint(false),
            SourceBlock::new("Yellow lower leaves that feel soft").with_heading_hint(false),
            SourceBlock::new("Soil that still smells damp after a week").with_heading_hint(false),
        ];
        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());
        assert_eq!(chunks.len(), 1, "a list item split the chunk: {chunks:?}");
        assert_eq!(chunks[0].location.section, vec!["Watering".to_string()]);
    }

    /// A deep trail from the extractor survives; the chunker does not cut it
    /// back to the nearest heading.
    #[test]
    fn a_known_heading_trail_is_kept() {
        let mut heading = SourceBlock::new("Signs of overwatering").with_heading_hint(true);
        heading.location.section = vec![
            "Guide".into(),
            "Watering".into(),
            "Signs of overwatering".into(),
        ];
        let blocks = vec![
            heading,
            SourceBlock::new("Roots sitting in wet soil cannot take up oxygen, so they rot."),
        ];
        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());
        assert_eq!(
            chunks[0].location.section,
            vec!["Guide", "Watering", "Signs of overwatering"]
        );
    }
}

#[cfg(test)]
mod heading_attachment_tests {
    use super::*;

    /// With the default 600-char target, a heading followed by a 590-char
    /// paragraph trips the size rule. The heading must stay with its
    /// paragraph rather than be flushed alone and dropped under `min_chars`.
    #[test]
    fn a_heading_is_never_dropped_by_the_size_rule() {
        let paragraph = "Watering depends on light and pot size. ".repeat(15);
        assert!(paragraph.trim().len() > 580 && paragraph.trim().len() < 1_100);
        let blocks = vec![
            SourceBlock::new("Light"),
            SourceBlock::new("Bright indirect light suits most foliage plants in a home."),
            SourceBlock::new("Watering"),
            SourceBlock::new(paragraph),
        ];
        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());
        assert_eq!(chunks.len(), 2, "{chunks:#?}");
        assert!(chunks[1].content.starts_with("Watering\n\n"));
        assert_eq!(chunks[1].location.section, vec!["Watering".to_string()]);
        assert!(chunks[1].structural_start);
        assert_eq!(
            chunks[1].embed_text, chunks[1].content,
            "the previous section must not bleed into this one's vector"
        );
    }

    /// The same for a block too large to keep whole: the heading leads the
    /// first piece, which keeps the heading's section and boundary.
    #[test]
    fn a_heading_leads_the_first_piece_of_an_oversized_block() {
        let long = "Repot when roots circle the pot. ".repeat(60);
        let blocks = vec![
            SourceBlock::new("Light"),
            SourceBlock::new("Bright indirect light suits most foliage plants in a home."),
            SourceBlock::new("Repotting"),
            SourceBlock::new(long),
        ];
        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());
        assert!(chunks.len() > 2);
        assert!(chunks[1].content.starts_with("Repotting\n\nRepot when"));
        assert_eq!(chunks[1].location.section, vec!["Repotting".to_string()]);
        assert!(chunks[1].structural_start);
        assert_eq!(chunks[1].embed_text, chunks[1].content);
        assert!(chunks[2..].iter().all(|chunk| !chunk.structural_start));
    }
}
