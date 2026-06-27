#[cfg(feature = "learn")]
use super::extract::ExtractedContent;

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct ReadingSection {
    pub document_title: String,
    pub heading_path: String,
    pub text: String,
    pub section_number: usize,
    pub total_sections: usize,
    pub is_summary_checkpoint: bool,
}

#[cfg(feature = "learn")]
const DEFAULT_MAX_SECTION_CHARS: usize = 24_000;

#[cfg(feature = "learn")]
const SUMMARY_CHECKPOINT_INTERVAL: usize = 5;

#[cfg(feature = "learn")]
pub fn split_into_sections(
    content: &ExtractedContent,
    max_section_chars: Option<usize>,
) -> Vec<ReadingSection> {
    let max_chars = max_section_chars.unwrap_or(DEFAULT_MAX_SECTION_CHARS);
    let text = &content.text;
    let title = &content.title;

    if text.len() <= max_chars {
        let heading_path = if content.headings.is_empty() {
            String::new()
        } else {
            content.headings[0].text.clone()
        };
        return vec![ReadingSection {
            document_title: title.clone(),
            heading_path,
            text: text.clone(),
            section_number: 1,
            total_sections: 1,
            is_summary_checkpoint: false,
        }];
    }

    let mut sections = Vec::new();
    let mut current_start = 0;
    let mut current_heading_path = Vec::<String>::new();

    let mut split_points: Vec<(usize, String, u8)> = content
        .headings
        .iter()
        .map(|h| (h.byte_offset, h.text.clone(), h.level))
        .collect();

    split_points.push((text.len(), String::new(), 0));

    for i in 0..split_points.len() {
        let (offset, ref heading_text, level) = split_points[i];

        if offset <= current_start {
            if level > 0 {
                while current_heading_path.len() >= level as usize {
                    current_heading_path.pop();
                }
                current_heading_path.push(heading_text.clone());
            }
            continue;
        }

        let segment = &text[current_start..offset];

        if segment.len() <= max_chars {
            let path = build_heading_path(&current_heading_path);
            sections.push((current_start, offset, path));
            current_start = offset;
        } else {
            let sub_sections = split_at_paragraphs(segment, max_chars);
            for (sub_start, sub_end) in sub_sections {
                let path = build_heading_path(&current_heading_path);
                sections.push((
                    current_start + sub_start,
                    current_start + sub_end,
                    path,
                ));
            }
            current_start = offset;
        }

        if level > 0 {
            while current_heading_path.len() >= level as usize {
                current_heading_path.pop();
            }
            current_heading_path.push(heading_text.clone());
        }
    }

    if current_start < text.len() {
        let path = build_heading_path(&current_heading_path);
        let remaining = &text[current_start..];
        if remaining.len() <= max_chars {
            sections.push((current_start, text.len(), path));
        } else {
            let sub_sections = split_at_paragraphs(remaining, max_chars);
            for (sub_start, sub_end) in sub_sections {
                sections.push((
                    current_start + sub_start,
                    current_start + sub_end,
                    path.clone(),
                ));
            }
        }
    }

    if sections.is_empty() {
        sections.push((0, text.len(), String::new()));
    }

    let total = sections.len();
    sections
        .into_iter()
        .enumerate()
        .map(|(i, (start, end, path))| {
            let section_num = i + 1;
            ReadingSection {
                document_title: title.clone(),
                heading_path: path,
                text: text[start..end].to_string(),
                section_number: section_num,
                total_sections: total,
                is_summary_checkpoint: section_num % SUMMARY_CHECKPOINT_INTERVAL == 0
                    && section_num < total,
            }
        })
        .collect()
}

#[cfg(feature = "learn")]
fn build_heading_path(stack: &[String]) -> String {
    stack.join(" > ")
}

#[cfg(feature = "learn")]
fn split_at_paragraphs(text: &str, max_chars: usize) -> Vec<(usize, usize)> {
    let mut sections = Vec::new();
    let mut current_start = 0;

    let paragraph_breaks: Vec<usize> = text.match_indices("\n\n").map(|(idx, _)| idx).collect();

    if paragraph_breaks.is_empty() {
        return hard_split(text.len(), max_chars);
    }

    let mut last_break = 0;

    for &break_pos in &paragraph_breaks {
        let segment_len = break_pos - current_start;
        if segment_len >= max_chars {
            if last_break > current_start {
                sections.push((current_start, last_break));
                current_start = last_break;
            } else {
                let hard = hard_split(break_pos - current_start, max_chars);
                for (s, e) in hard {
                    sections.push((current_start + s, current_start + e));
                }
                current_start = break_pos;
            }
        }
        last_break = break_pos + 2;
    }

    if current_start < text.len() {
        sections.push((current_start, text.len()));
    }

    if sections.is_empty() {
        sections.push((0, text.len()));
    }

    sections
}

#[cfg(feature = "learn")]
fn hard_split(total_len: usize, max_chars: usize) -> Vec<(usize, usize)> {
    let mut sections = Vec::new();
    let mut start = 0;
    while start < total_len {
        let end = (start + max_chars).min(total_len);
        sections.push((start, end));
        start = end;
    }
    sections
}

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;
    use crate::learn::extract::Heading;

    #[test]
    fn test_single_section_fits() {
        let content = ExtractedContent {
            title: "Small Doc".into(),
            text: "Short text.".into(),
            headings: vec![],
            raw_bytes: vec![],
        };
        let sections = split_into_sections(&content, Some(1000));
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].document_title, "Small Doc");
        assert_eq!(sections[0].section_number, 1);
        assert_eq!(sections[0].total_sections, 1);
        assert!(!sections[0].is_summary_checkpoint);
    }

    #[test]
    fn test_split_at_headings() {
        let content = ExtractedContent {
            title: "Test Doc".into(),
            text: "# Title\n\nIntro.\n\n## A\n\nLong A content.\n\n## B\n\nLong B content.\n"
                .into(),
            headings: vec![
                Heading {
                    level: 1,
                    text: "Title".into(),
                    byte_offset: 0,
                },
                Heading {
                    level: 2,
                    text: "A".into(),
                    byte_offset: 18,
                },
                Heading {
                    level: 2,
                    text: "B".into(),
                    byte_offset: 40,
                },
            ],
            raw_bytes: vec![],
        };

        let sections = split_into_sections(&content, Some(25));
        assert!(sections.len() >= 2);
        assert_eq!(sections[0].document_title, "Test Doc");
        assert_eq!(sections[0].section_number, 1);
    }

    #[test]
    fn test_summary_checkpoint() {
        let long_text = "word ".repeat(50000);
        let content = ExtractedContent {
            title: "Long Doc".into(),
            text: long_text,
            headings: vec![],
            raw_bytes: vec![],
        };
        let sections = split_into_sections(&content, Some(1000));
        assert!(sections.len() > 5);
        assert!(sections[4].is_summary_checkpoint);
        assert!(!sections.last().unwrap().is_summary_checkpoint);
    }

    #[test]
    fn test_empty_text() {
        let content = ExtractedContent {
            title: "Empty".into(),
            text: String::new(),
            headings: vec![],
            raw_bytes: vec![],
        };
        let sections = split_into_sections(&content, Some(1000));
        assert_eq!(sections.len(), 1);
    }

    #[test]
    fn test_heading_path() {
        assert_eq!(build_heading_path(&[]), "");
        assert_eq!(
            build_heading_path(&["Auth".to_string(), "OAuth2".to_string()]),
            "Auth > OAuth2"
        );
    }
}
