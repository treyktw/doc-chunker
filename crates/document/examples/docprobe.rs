fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::PathBuf::from(&args[1]);
    let bytes = std::fs::read(&path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy().to_string();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        use document_extract::{DocumentEngine, StandardDocumentEngine};
        let engine = StandardDocumentEngine::new();
        match engine.normalize(&bytes, Some(&name)).await {
            Ok(doc) => {
                println!("format   {:?}", doc.format);
                println!("title    {:?}", doc.metadata.title);
                println!("pages    {:?}", doc.metadata.page_count);
                println!(
                    "blocks   {}  ({} chars)",
                    doc.blocks.len(),
                    doc.total_characters()
                );
                for w in &doc.warnings {
                    println!("warning  {w}");
                }
                for b in doc.blocks.iter().take(6) {
                    println!("  [{:?}] page={:?} section={:?}", b.kind, b.page, b.section);
                    println!("      {}", b.text.chars().take(72).collect::<String>());
                }
            }
            Err(e) => println!("ERROR: {e}"),
        }
    });
}
