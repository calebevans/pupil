#[cfg(feature = "learn")]
use super::{ExtractedContent, Heading};
#[cfg(feature = "learn")]
use crate::learn::source::ResolvedSource;

#[cfg(feature = "learn")]
const MAX_URL_RESPONSE_BYTES: usize = 50_000_000;

#[cfg(feature = "learn")]
pub fn extract_html(
    html: &str,
    raw_bytes: &[u8],
    source: &ResolvedSource,
) -> anyhow::Result<ExtractedContent> {
    extract_html_inner(html, raw_bytes, source)
}

#[cfg(feature = "learn")]
pub async fn fetch_and_extract_url(
    url: &str,
    source: &ResolvedSource,
) -> anyhow::Result<ExtractedContent> {
    validate_url_safety(url)?;

    let client = reqwest::Client::builder()
        .user_agent("PupilBot/1.0 (+https://github.com/calebevans/pupil)")
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to fetch URL '{}': {}", url, e))?;

    let status = response.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "HTTP {} when fetching URL '{}': {}",
            status.as_u16(),
            url,
            status.canonical_reason().unwrap_or("Unknown")
        ));
    }

    if let Some(len) = response.content_length() {
        if len as usize > MAX_URL_RESPONSE_BYTES {
            return Err(anyhow::anyhow!(
                "Response from '{}' is too large ({} bytes, max {})",
                url,
                len,
                MAX_URL_RESPONSE_BYTES
            ));
        }
    }

    let raw_bytes = response
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read response body from '{}': {}", url, e))?
        .to_vec();

    if raw_bytes.len() > MAX_URL_RESPONSE_BYTES {
        return Err(anyhow::anyhow!(
            "Response body from '{}' exceeds size limit ({} bytes, max {})",
            url,
            raw_bytes.len(),
            MAX_URL_RESPONSE_BYTES
        ));
    }

    let html = String::from_utf8_lossy(&raw_bytes).into_owned();

    extract_html_inner(&html, &raw_bytes, source)
}

#[cfg(feature = "learn")]
fn validate_url_safety(url: &str) -> anyhow::Result<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| anyhow::anyhow!("Invalid URL '{}': {}", url, e))?;

    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(anyhow::anyhow!(
                "Blocked URL scheme '{}' in '{}': only http:// and https:// are allowed",
                scheme,
                url
            ));
        }
    }

    if let Some(host) = parsed.host_str() {
        let blocked_hosts = [
            "169.254.169.254",
            "metadata.google.internal",
            "metadata.azure.internal",
            "localhost",
            "127.0.0.1",
            "::1",
            "[::1]",
            "0.0.0.0",
        ];
        let host_lower = host.to_lowercase();
        for blocked in &blocked_hosts {
            if host_lower == *blocked {
                return Err(anyhow::anyhow!(
                    "Blocked request to private/metadata host '{}' in URL '{}'",
                    host,
                    url
                ));
            }
        }
        if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
            let octets = ip.octets();
            if octets[0] == 10
                || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 168)
                || (octets[0] == 169 && octets[1] == 254)
                || octets[0] == 0
            {
                return Err(anyhow::anyhow!(
                    "Blocked request to private IP '{}' in URL '{}'",
                    host,
                    url
                ));
            }
        }
    }

    Ok(())
}

#[cfg(feature = "learn")]
fn extract_html_inner(
    html: &str,
    raw_bytes: &[u8],
    source: &ResolvedSource,
) -> anyhow::Result<ExtractedContent> {
    let mut parser = readability_rust::Readability::new(html, None);
    let (article_text, article_title) = match parser {
        Ok(ref mut p) => match p.parse() {
            Some(article) => {
                let title = article
                    .title
                    .filter(|t| !t.is_empty());
                let text = article.text_content.unwrap_or_default();
                (text, title)
            }
            None => {
                let text = fallback_text_extraction(html);
                (text, None)
            }
        },
        Err(_) => {
            let text = fallback_text_extraction(html);
            (text, None)
        }
    };

    let title = article_title
        .or_else(|| extract_title_tag(html))
        .or_else(|| extract_first_h1(html))
        .unwrap_or_else(|| source.source_key.clone());

    let headings = extract_headings_from_text(&article_text);
    let text = normalize_html_text(&article_text);

    Ok(ExtractedContent {
        title,
        text,
        headings,
        raw_bytes: raw_bytes.to_vec(),
    })
}

#[cfg(feature = "learn")]
fn fallback_text_extraction(html: &str) -> String {
    let document = scraper::Html::parse_document(html);
    let body_selector = scraper::Selector::parse("body").unwrap();
    document
        .select(&body_selector)
        .flat_map(|el| el.text())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(feature = "learn")]
fn extract_title_tag(html: &str) -> Option<String> {
    let document = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse("title").ok()?;
    document
        .select(&selector)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(feature = "learn")]
fn extract_first_h1(html: &str) -> Option<String> {
    let document = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse("h1").ok()?;
    document
        .select(&selector)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(feature = "learn")]
fn extract_headings_from_text(text: &str) -> Vec<Heading> {
    let mut headings = Vec::new();
    let mut byte_offset = 0;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            headings.push(Heading {
                level: 1,
                text: rest.to_string(),
                byte_offset,
            });
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            headings.push(Heading {
                level: 2,
                text: rest.to_string(),
                byte_offset,
            });
        } else if let Some(rest) = trimmed.strip_prefix("### ") {
            headings.push(Heading {
                level: 3,
                text: rest.to_string(),
                byte_offset,
            });
        } else if let Some(rest) = trimmed.strip_prefix("#### ") {
            headings.push(Heading {
                level: 4,
                text: rest.to_string(),
                byte_offset,
            });
        } else if let Some(rest) = trimmed.strip_prefix("##### ") {
            headings.push(Heading {
                level: 5,
                text: rest.to_string(),
                byte_offset,
            });
        } else if let Some(rest) = trimmed.strip_prefix("###### ") {
            headings.push(Heading {
                level: 6,
                text: rest.to_string(),
                byte_offset,
            });
        }
        byte_offset += line.len() + 1;
    }

    headings
}

#[cfg(feature = "learn")]
fn normalize_html_text(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    super::normalize_blank_lines(&normalized).trim().to_string()
}

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;

    #[test]
    fn test_validate_url_safety_blocks_private() {
        assert!(validate_url_safety("http://127.0.0.1/secret").is_err());
        assert!(validate_url_safety("http://localhost/admin").is_err());
        assert!(validate_url_safety("http://169.254.169.254/metadata").is_err());
        assert!(validate_url_safety("http://10.0.0.1/internal").is_err());
        assert!(validate_url_safety("http://192.168.1.1/router").is_err());
        assert!(validate_url_safety("ftp://example.com/file").is_err());
    }

    #[test]
    fn test_validate_url_safety_allows_public() {
        assert!(validate_url_safety("https://example.com/page").is_ok());
        assert!(validate_url_safety("http://wiki.company.com/docs").is_ok());
    }

    #[test]
    fn test_extract_title_tag() {
        let html = "<html><head><title>My Page</title></head><body></body></html>";
        assert_eq!(extract_title_tag(html), Some("My Page".to_string()));
    }

    #[test]
    fn test_extract_first_h1() {
        let html = "<html><body><h1>Main Title</h1><p>Content</p></body></html>";
        assert_eq!(extract_first_h1(html), Some("Main Title".to_string()));
    }

    #[test]
    fn test_extract_headings_from_text() {
        let text = "# Title\nSome text\n## Section\nMore text\n";
        let headings = extract_headings_from_text(text);
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].text, "Title");
        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[1].text, "Section");
    }

    #[test]
    fn test_normalize_html_text() {
        let text = "Hello\n\n\n\n\nWorld";
        let normalized = normalize_html_text(text);
        assert_eq!(normalized, "Hello\n\nWorld");
    }
}
