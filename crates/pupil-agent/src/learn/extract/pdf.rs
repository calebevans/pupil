#[cfg(feature = "learn")]
use super::ExtractedContent;
#[cfg(feature = "learn")]
use crate::learn::source::ResolvedSource;

#[cfg(feature = "learn")]
pub fn extract_pdf(
    raw_bytes: &[u8],
    source: &ResolvedSource,
) -> anyhow::Result<ExtractedContent> {
    let text = pdf_extract::extract_text_from_mem(raw_bytes).map_err(|e| {
        anyhow::anyhow!(
            "Failed to extract text from PDF '{}': {}",
            source.source_key,
            e
        )
    })?;

    let normalized = normalize_pdf_text(&text);

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

#[cfg(feature = "learn")]
fn normalize_pdf_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut prev_char = '\0';
    let mut newline_count = 0;

    for ch in text.chars() {
        match ch {
            '\n' => {
                if prev_char == '-' {
                    result.pop();
                    prev_char = result.chars().last().unwrap_or('\0');
                    continue;
                }
                newline_count += 1;
                if newline_count <= 2 {
                    result.push('\n');
                }
            }
            ' ' | '\t' => {
                newline_count = 0;
                if prev_char != ' ' && prev_char != '\t' && prev_char != '\n' {
                    result.push(' ');
                }
            }
            '\r' => {}
            _ => {
                newline_count = 0;
                result.push(ch);
            }
        }
        prev_char = ch;
    }

    result.trim().to_string()
}

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_pdf_text_basic() {
        let text = "Hello  world\n\n\n\nNew section";
        let result = normalize_pdf_text(text);
        assert_eq!(result, "Hello world\n\nNew section");
    }

    #[test]
    fn test_normalize_pdf_text_hyphenation() {
        let text = "docu-\nment";
        let result = normalize_pdf_text(text);
        assert_eq!(result, "document");
    }

    #[test]
    fn test_normalize_pdf_text_carriage_return() {
        let text = "line1\r\nline2";
        let result = normalize_pdf_text(text);
        assert!(!result.contains('\r'));
    }
}
