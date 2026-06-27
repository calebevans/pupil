#[cfg(feature = "learn")]
pub mod markdown;
#[cfg(feature = "learn")]
pub mod text;
#[cfg(feature = "learn")]
pub mod pdf;
#[cfg(feature = "learn")]
pub mod html;

#[cfg(feature = "learn")]
use super::source::{ResolvedSource, SourceType};

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub byte_offset: usize,
}

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct ExtractedContent {
    pub title: String,
    pub text: String,
    pub headings: Vec<Heading>,
    pub raw_bytes: Vec<u8>,
}

/// Collapse runs of more than 2 consecutive newlines into 2.
#[cfg(feature = "learn")]
pub(crate) fn normalize_blank_lines(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut newline_count = 0;
    for ch in text.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                result.push(ch);
            }
        } else {
            newline_count = 0;
            result.push(ch);
        }
    }
    result
}

#[cfg(feature = "learn")]
pub async fn extract(source: &ResolvedSource) -> anyhow::Result<ExtractedContent> {
    use anyhow::Context;
    match source.source_type {
        SourceType::Markdown => {
            let path = source
                .file_path
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Markdown source missing file path"))?;
            let raw_bytes = tokio::fs::read(path).await
                .with_context(|| format!("reading {}", path.display()))?;
            let text = String::from_utf8_lossy(&raw_bytes).into_owned();
            markdown::extract_markdown(&text, &raw_bytes, source)
        }
        SourceType::PlainText => {
            let path = source
                .file_path
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("PlainText source missing file path"))?;
            let raw_bytes = tokio::fs::read(path).await
                .with_context(|| format!("reading {}", path.display()))?;
            let text_str = String::from_utf8_lossy(&raw_bytes).into_owned();
            text::extract_plain_text(&text_str, &raw_bytes, source)
        }
        SourceType::Pdf => {
            let path = source
                .file_path
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("PDF source missing file path"))?;
            let raw_bytes = tokio::fs::read(path).await
                .with_context(|| format!("reading {}", path.display()))?;
            pdf::extract_pdf(&raw_bytes, source)
        }
        SourceType::Html => {
            let path = source
                .file_path
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("HTML source missing file path"))?;
            let raw_bytes = tokio::fs::read(path).await
                .with_context(|| format!("reading {}", path.display()))?;
            let text_str = String::from_utf8_lossy(&raw_bytes).into_owned();
            html::extract_html(&text_str, &raw_bytes, source)
        }
        SourceType::Url => {
            let url = source
                .url
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("URL source missing url"))?;
            html::fetch_and_extract_url(url, source).await
        }
    }
}
