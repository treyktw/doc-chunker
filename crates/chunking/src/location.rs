use serde::{Deserialize, Serialize};

/// Where inside a source a chunk came from.
///
/// Written at index time and never reconstructed later — citation
/// click-through depends on it, and recovering a page number after the fact
/// means reparsing the document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChunkLocation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i32>,
    /// Seconds into a recording.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Heading trail, so a chunk can say where it sits in the document.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub section: Vec<String>,
}
