//! Escapes caller text first, then applies the fixed Markdown subset (Q3.2, Q4.5).
//!
//! Escaping before converting is what keeps the API safe: markup that arrives from a caller is
//! never treated as markup, only the subset below is built by us. Links accept `http`/`https`
//! only. Unbalanced markers stay literal text.

/// Escapes the characters that would otherwise become markup.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Renders the caller's text as the HTML a standard Mumble server forwards to chat.
pub fn render_html(text: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut list: Vec<String> = Vec::new();
    let mut lines = text.split('\n').peekable();

    let flush_paragraph = |paragraph: &mut Vec<String>, blocks: &mut Vec<String>| {
        if !paragraph.is_empty() {
            blocks.push(paragraph.join("<br>"));
            paragraph.clear();
        }
    };
    let flush_list = |list: &mut Vec<String>, blocks: &mut Vec<String>| {
        if !list.is_empty() {
            let items = list.join("</li><li>");
            blocks.push(format!("<ul><li>{items}</li></ul>"));
            list.clear();
        }
    };

    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("```") {
            flush_paragraph(&mut paragraph, &mut blocks);
            flush_list(&mut list, &mut blocks);
            let mut code = Vec::new();
            for inner in lines.by_ref() {
                if inner.trim_start().starts_with("```") {
                    break;
                }
                code.push(escape_html(inner));
            }
            blocks.push(format!("<pre><code>{}</code></pre>", code.join("\n")));
            continue;
        }

        if let Some(level) = heading_level(line) {
            flush_paragraph(&mut paragraph, &mut blocks);
            flush_list(&mut list, &mut blocks);
            let title = inline(&escape_html(line.trim_start()[level..].trim()));
            blocks.push(format!("<h{level}>{title}</h{level}>"));
            continue;
        }

        if let Some(item) = list_item(line) {
            flush_paragraph(&mut paragraph, &mut blocks);
            list.push(inline(&escape_html(item)));
            continue;
        }

        flush_list(&mut list, &mut blocks);
        paragraph.push(inline(&escape_html(line)));
    }

    flush_paragraph(&mut paragraph, &mut blocks);
    flush_list(&mut list, &mut blocks);
    blocks.join("\n")
}

/// `# title` … `###### title`; seven hashes are plain text.
fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ') {
        Some(hashes)
    } else {
        None
    }
}

fn list_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed.strip_prefix("- ").map(str::trim)
}

/// Inline spans of the subset: `code`, **bold**, *italic*, plain http/https links.
fn inline(escaped: &str) -> String {
    let bytes = escaped.as_bytes();
    let mut out = String::with_capacity(escaped.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`'
            && let Some(end) = find(bytes, b'`', i + 1)
        {
            out.push_str("<code>");
            out.push_str(&escaped[i + 1..end]);
            out.push_str("</code>");
            i = end + 1;
            continue;
        }
        if bytes[i] == b'*' {
            if bytes.get(i + 1) == Some(&b'*') {
                if let Some(end) = find_pair(bytes, i + 2) {
                    out.push_str("<b>");
                    out.push_str(&escaped[i + 2..end]);
                    out.push_str("</b>");
                    i = end + 2;
                    continue;
                }
            } else if let Some(end) = find(bytes, b'*', i + 1) {
                out.push_str("<i>");
                out.push_str(&escaped[i + 1..end]);
                out.push_str("</i>");
                i = end + 1;
                continue;
            }
        }
        if let Some(len) = link_len(escaped, i) {
            let url = &escaped[i..i + len];
            out.push_str("<a href=\"");
            out.push_str(url);
            out.push_str("\">");
            out.push_str(url);
            out.push_str("</a>");
            i += len;
            continue;
        }
        let c = escaped[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn find(bytes: &[u8], needle: u8, from: usize) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|b| *b == needle)
        .map(|p| p + from)
}

fn find_pair(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Length of a plain http/https URL at `start`; stops at whitespace or a quote.
fn link_len(escaped: &str, start: usize) -> Option<usize> {
    if !escaped[start..].starts_with("http://") && !escaped[start..].starts_with("https://") {
        return None;
    }
    let end = escaped[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>'))
        .count();
    if end > 7 { Some(end) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_markup_stays_literal_text() {
        assert_eq!(render_html("<b>hi</b>"), "&lt;b&gt;hi&lt;/b&gt;");
        assert_eq!(
            render_html("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
    }

    #[test]
    fn converts_the_subset() {
        assert_eq!(render_html("**bold**"), "<b>bold</b>");
        assert_eq!(render_html("*italic*"), "<i>italic</i>");
        assert_eq!(render_html("`code`"), "<code>code</code>");
        assert_eq!(render_html("# Title"), "<h1>Title</h1>");
        assert_eq!(
            render_html("- one\n- two"),
            "<ul><li>one</li><li>two</li></ul>"
        );
        assert_eq!(render_html("a\nb"), "a<br>b");
        assert_eq!(
            render_html("```\n<b>x</b>\n```"),
            "<pre><code>&lt;b&gt;x&lt;/b&gt;</code></pre>"
        );
        assert_eq!(
            render_html("https://example.com/x"),
            "<a href=\"https://example.com/x\">https://example.com/x</a>"
        );
    }

    #[test]
    fn other_link_schemes_are_not_links() {
        assert_eq!(render_html("data:text/html,x"), "data:text/html,x");
        assert_eq!(render_html("javascript:alert(1)"), "javascript:alert(1)");
    }

    #[test]
    fn unbalanced_markers_stay_literal() {
        assert_eq!(render_html("**open"), "**open");
        assert_eq!(render_html("`open"), "`open");
        assert_eq!(render_html("####### seven"), "####### seven");
    }

    #[test]
    fn escaping_happens_before_conversion() {
        // The ampersand is escaped, the Markdown span still converts.
        assert_eq!(render_html("**a & b**"), "<b>a &amp; b</b>");
    }
}
