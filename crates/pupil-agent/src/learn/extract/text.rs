#[cfg(feature = "learn")]
use super::ExtractedContent;
#[cfg(feature = "learn")]
use crate::learn::source::ResolvedSource;

#[cfg(feature = "learn")]
pub fn extract_plain_text(
    text: &str,
    raw_bytes: &[u8],
    source: &ResolvedSource,
) -> anyhow::Result<ExtractedContent> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = super::normalize_blank_lines(&normalized);
    let normalized = normalized.trim().to_string();

    let title = source
        .file_path
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or(&source.source_key)
        .to_string();

    Ok(ExtractedContent {
        title,
        text: normalized,
        headings: Vec::new(),
        raw_bytes: raw_bytes.to_vec(),
    })
}



#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;
    use crate::learn::source::SourceType;
    use std::path::PathBuf;

    fn make_source(key: &str) -> ResolvedSource {
        ResolvedSource {
            source_key: key.into(),
            file_path: Some(PathBuf::from(format!("/curriculum/{key}"))),
            url: None,
            source_type: SourceType::PlainText,
            learning_profile: None,
            learning_prompt: None,
            namespace: "knowledge".into(),
            extra_tags: vec![],
        }
    }

    #[test]
    fn test_plain_text_extraction() {
        let text = "Hello world\n\nThis is content.\n";
        let source = make_source("notes.txt");
        let result = extract_plain_text(text, text.as_bytes(), &source).unwrap();
        assert_eq!(result.title, "notes");
        assert!(result.headings.is_empty());
        assert_eq!(result.text, "Hello world\n\nThis is content.");
    }

    #[test]
    fn test_crlf_normalization() {
        let text = "line1\r\nline2\rline3\n";
        let source = make_source("test.txt");
        let result = extract_plain_text(text, text.as_bytes(), &source).unwrap();
        assert!(!result.text.contains('\r'));
    }

    #[test]
    fn test_empty_text() {
        let text = "";
        let source = make_source("empty.txt");
        let result = extract_plain_text(text, text.as_bytes(), &source).unwrap();
        assert_eq!(result.text, "");
        assert_eq!(result.title, "empty");
    }
}
