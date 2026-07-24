//! Markdown loader：使用 `pulldown-cmark` 解析并提取纯文本。

use std::path::Path;

use anyhow::{Context, Result};
use pulldown_cmark::{Event, Parser};

/// 解析 Markdown 文件，提取所有文本节点。
///
/// 标题、段落、代码块、列表等都会被提取为纯文本。
/// 链接只保留文字，图片 alt 文本保留，URL 丢弃。
pub fn extract(path: &Path) -> Result<String> {
    let md = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read markdown file: {}", path.display()))?;

    let parser = Parser::new(&md);
    let mut output = String::new();

    for event in parser {
        match event {
            Event::Text(t) | Event::Code(t) => {
                output.push_str(&t);
            }
            Event::SoftBreak | Event::HardBreak => {
                output.push('\n');
            }
            // 段落/标题结束后加换行分隔
            Event::End(end_tag) => {
                use pulldown_cmark::TagEnd;
                match end_tag {
                    TagEnd::Paragraph
                    | TagEnd::Heading(..)
                    | TagEnd::CodeBlock
                    | TagEnd::List(..)
                    | TagEnd::BlockQuote(..)
                        if !output.ends_with('\n') =>
                    {
                        output.push('\n');
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // 去除多余的空白行
    Ok(output.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.md");
        std::fs::write(
            &path,
            "# 标题\n\n一段**文字**，带 [链接](https://example.com)。\n\n```rust\nlet x = 1;\n```\n\n- 列表项 1\n- 列表项 2",
        )
        .unwrap();
        let text = extract(&path).unwrap();
        assert!(text.contains("标题"), "expected '标题' in: {text}");
        assert!(text.contains("文字"), "expected '文字' in: {text}");
        assert!(text.contains("链接"), "expected '链接' in: {text}");
        assert!(text.contains("let x = 1;"), "expected code in: {text}");
        assert!(text.contains("列表项 1"), "expected list item in: {text}");
        assert!(
            !text.contains("https://example.com"),
            "URL should be stripped: {text}"
        );
    }

    #[test]
    fn test_md_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.md");
        std::fs::write(&path, "").unwrap();
        let text = extract(&path).unwrap();
        assert_eq!(text, "");
    }

    #[test]
    fn test_md_file_not_found() {
        let path = std::path::Path::new("nonexistent.md");
        let result = extract(path);
        assert!(result.is_err());
    }
}
