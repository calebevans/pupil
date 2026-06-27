#[cfg(feature = "learn")]
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[cfg(feature = "learn")]
use super::{ExtractedContent, Heading};
#[cfg(feature = "learn")]
use crate::learn::source::ResolvedSource;

#[cfg(feature = "learn")]
pub fn extract_markdown(
    markdown: &str,
    raw_bytes: &[u8],
    source: &ResolvedSource,
) -> anyhow::Result<ExtractedContent> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(markdown, options);

    let mut output = String::with_capacity(markdown.len());
    let mut headings: Vec<Heading> = Vec::new();
    let mut title: Option<String> = None;
    let mut current_heading_text = String::new();
    let mut in_heading = false;
    let mut current_heading_level: u8 = 0;
    let mut heading_byte_offset: usize = 0;

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                in_heading = true;
                current_heading_level = heading_level_to_u8(level);
                current_heading_text.clear();
                heading_byte_offset = output.len();
                let prefix = "#".repeat(current_heading_level as usize);
                output.push_str(&prefix);
                output.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => {
                in_heading = false;
                let heading_text = current_heading_text.trim().to_string();
                if title.is_none() && current_heading_level == 1 {
                    title = Some(heading_text.clone());
                }
                headings.push(Heading {
                    level: current_heading_level,
                    text: heading_text,
                    byte_offset: heading_byte_offset,
                });
                output.push('\n');
            }
            Event::Text(text) => {
                if in_heading {
                    current_heading_text.push_str(&text);
                }
                output.push_str(&text);
            }
            Event::Code(code) => {
                if in_heading {
                    current_heading_text.push_str(&code);
                }
                output.push('`');
                output.push_str(&code);
                output.push('`');
            }
            Event::SoftBreak | Event::HardBreak => {
                output.push('\n');
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                output.push_str("\n\n");
            }
            Event::Start(Tag::CodeBlock(_)) => {
                output.push_str("```\n");
            }
            Event::End(TagEnd::CodeBlock) => {
                output.push_str("```\n\n");
            }
            Event::Start(Tag::List(_)) => {}
            Event::End(TagEnd::List(_)) => {
                output.push('\n');
            }
            Event::Start(Tag::Item) => {
                output.push_str("- ");
            }
            Event::End(TagEnd::Item) => {
                output.push('\n');
            }
            Event::Start(Tag::BlockQuote(_)) => {
                output.push_str("> ");
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                output.push('\n');
            }
            Event::Start(Tag::Table(_)) => {}
            Event::End(TagEnd::Table) => {
                output.push('\n');
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {}
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                output.push('\n');
            }
            Event::Start(Tag::TableCell) => {}
            Event::End(TagEnd::TableCell) => {
                output.push_str(" | ");
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                let stripped = strip_html_tags(&html);
                output.push_str(&stripped);
            }
            _ => {}
        }
    }

    let normalized = super::normalize_blank_lines(&output);

    let fallback_title = source.source_key.clone();
    Ok(ExtractedContent {
        title: title.unwrap_or(fallback_title),
        text: normalized,
        headings,
        raw_bytes: raw_bytes.to_vec(),
    })
}

#[cfg(feature = "learn")]
fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(feature = "learn")]
fn strip_html_tags(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

// normalize_blank_lines is now shared via super::normalize_blank_lines

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;
    use crate::learn::source::SourceType;
    use std::path::PathBuf;

    fn make_source(key: &str) -> ResolvedSource {
        ResolvedSource {
            source_key: key.into(),
            file_path: Some(PathBuf::from(format!("/{key}"))),
            url: None,
            source_type: SourceType::Markdown,
            learning_profile: None,
            learning_prompt: None,
            namespace: "knowledge".into(),
            extra_tags: vec![],
        }
    }

    #[test]
    fn test_markdown_heading_extraction() {
        let md = "# Title\n\nIntro text.\n\n## Section A\n\nContent A.\n\n### Subsection A.1\n\nContent A.1.\n\n## Section B\n\nContent B.\n";
        let source = make_source("test.md");
        let result = extract_markdown(md, md.as_bytes(), &source).unwrap();

        assert_eq!(result.title, "Title");
        assert_eq!(result.headings.len(), 4);
        assert_eq!(result.headings[0].level, 1);
        assert_eq!(result.headings[0].text, "Title");
        assert_eq!(result.headings[1].level, 2);
        assert_eq!(result.headings[1].text, "Section A");
        assert_eq!(result.headings[2].level, 3);
        assert_eq!(result.headings[2].text, "Subsection A.1");
        assert_eq!(result.headings[3].level, 2);
        assert_eq!(result.headings[3].text, "Section B");
    }

    #[test]
    fn test_no_h1_uses_source_key() {
        let md = "## Only H2\n\nSome content.\n";
        let source = make_source("fallback.md");
        let result = extract_markdown(md, md.as_bytes(), &source).unwrap();
        assert_eq!(result.title, "fallback.md");
    }

    #[test]
    fn test_empty_markdown() {
        let md = "";
        let source = make_source("empty.md");
        let result = extract_markdown(md, md.as_bytes(), &source).unwrap();
        assert_eq!(result.title, "empty.md");
        assert!(result.headings.is_empty());
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>bold</b>"), "bold");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<a href='x'>link</a>"), "link");
    }
}
