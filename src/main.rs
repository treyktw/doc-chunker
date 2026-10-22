//! `chunkprobe`: print the blocks a file extracts to and the chunks they
//! become, with every location attached.
//!
//! ```text
//! cargo run -- samples/plant-care.pdf
//! cargo run -- samples/plant-care.md --target 300 --max 600
//! cargo run -- samples/plant-care.docx --json
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use doc_chunker::{ChunkLocation, ChunkingConfig, extract_and_chunk};

struct Options {
    path: PathBuf,
    config: ChunkingConfig,
    json: bool,
    show_embed_text: bool,
}

const USAGE: &str = "usage: chunkprobe <file> [--target CHARS] [--max CHARS] [--overlap CHARS] [--min CHARS] [--embed-text] [--json]";

fn parse_options() -> Result<Options, String> {
    let mut arguments = std::env::args().skip(1);
    let mut path = None;
    let mut config = ChunkingConfig::default();
    let mut json = false;
    let mut show_embed_text = false;
    while let Some(argument) = arguments.next() {
        let mut number = |name: &str| -> Result<usize, String> {
            arguments
                .next()
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| format!("{name} needs a number"))
        };
        match argument.as_str() {
            "--target" => config.target_chars = number("--target")?,
            "--max" => config.max_chars = number("--max")?,
            "--overlap" => config.overlap_chars = number("--overlap")?,
            "--min" => config.min_chars = number("--min")?,
            "--json" => json = true,
            "--embed-text" => show_embed_text = true,
            "-h" | "--help" => return Err(USAGE.into()),
            other if other.starts_with("--") => {
                return Err(format!("unknown flag {other}\n{USAGE}"));
            }
            other => path = Some(PathBuf::from(other)),
        }
    }
    let path = path.ok_or_else(|| USAGE.to_string())?;
    if config.max_chars < config.target_chars {
        return Err("--max must be at least --target".into());
    }
    Ok(Options {
        path,
        config,
        json,
        show_embed_text,
    })
}

fn describe(location: &ChunkLocation) -> String {
    let mut parts = Vec::new();
    if let Some(page) = location.page {
        parts.push(format!("page {page}"));
    }
    if let Some(seconds) = location.timestamp {
        parts.push(format!("t={seconds:.1}s"));
    }
    if !location.section.is_empty() {
        parts.push(format!("§ {}", location.section.join(" > ")));
    }
    if let Some(path) = &location.path {
        parts.push(format!("path {path}"));
    }
    if parts.is_empty() {
        "no position".into()
    } else {
        parts.join("  ")
    }
}

fn preview(text: &str, width: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= width {
        flat
    } else {
        format!("{}…", flat.chars().take(width - 1).collect::<String>())
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let bytes = match std::fs::read(&options.path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("could not read {}: {error}", options.path.display());
            return ExitCode::from(1);
        }
    };
    let filename = options
        .path
        .file_name()
        .map(|name| name.to_string_lossy().to_string());

    let (document, chunks) =
        match extract_and_chunk(&bytes, filename.as_deref(), &options.config).await {
            Ok(result) => result,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };

    if options.json {
        let chunk_values = chunks
            .iter()
            .map(|chunk| {
                serde_json::json!({
                    "content": chunk.content,
                    "embed_text": chunk.embed_text,
                    "location": chunk.location,
                    "structural_start": chunk.structural_start,
                })
            })
            .collect::<Vec<_>>();
        let output = serde_json::json!({ "document": document, "chunks": chunk_values });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).expect("serializable")
        );
        return ExitCode::SUCCESS;
    }

    println!("file     {}", options.path.display());
    println!("format   {:?}", document.format);
    if let Some(title) = &document.metadata.title {
        println!("title    {title}");
    }
    if let Some(pages) = document.metadata.page_count {
        println!("pages    {pages}");
    }
    for warning in &document.warnings {
        println!("warning  {warning}");
    }

    println!("\nBLOCKS ({})", document.blocks.len());
    for block in &document.blocks {
        let mut position = Vec::new();
        if let Some(page) = block.page {
            position.push(format!("p{page}"));
        }
        if !block.section.is_empty() {
            position.push(block.section.join(" > "));
        }
        println!(
            "  {:>3} {:<9} [{}]",
            block.ordinal,
            format!("{:?}", block.kind),
            position.join(" | ")
        );
        println!("      {}", preview(&block.text, 88));
    }

    println!(
        "\nCHUNKS ({})  target={} max={} overlap={} min={}",
        chunks.len(),
        options.config.target_chars,
        options.config.max_chars,
        options.config.overlap_chars,
        options.config.min_chars
    );
    for (index, chunk) in chunks.iter().enumerate() {
        let overlap = chunk.embed_text.len().saturating_sub(chunk.content.len());
        let boundary = if chunk.structural_start {
            "boundary: structural".to_string()
        } else if overlap > 0 {
            format!("boundary: size, +{overlap} overlap")
        } else {
            "boundary: size".to_string()
        };
        println!(
            "  #{index:<2} {:>4} chars  {boundary:<28} {}",
            chunk.content.len(),
            describe(&chunk.location)
        );
        println!("      {}", preview(&chunk.content, 88));
        if options.show_embed_text && chunk.embed_text != chunk.content {
            println!("      embed: {}", preview(&chunk.embed_text, 88));
        }
    }
    ExitCode::SUCCESS
}
