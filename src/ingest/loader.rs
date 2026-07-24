//! 按文件扩展名分派到具体 loader。
//!
//! 每个 loader 实现 `pub fn extract(path: &Path) -> Result<String>`，
//! 把文件内容提取为纯文本字符串。

use std::path::Path;

use anyhow::{Result, anyhow};

use super::{docx, md, pdf, pptx, txt, xlsx};

/// 根据文件扩展名选择 loader 并提取文本。
///
/// 不认识的扩展名返回错误，不猜测文件内容。
pub fn extract(path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match ext.as_deref() {
        Some("pdf") => pdf::extract(path),
        Some("docx") => docx::extract(path),
        Some("pptx") => pptx::extract(path),
        Some("xlsx") => xlsx::extract(path),
        Some("md") => md::extract(path),
        Some("txt") => txt::extract(path),
        Some(other) => Err(anyhow!(
            "unsupported file type: .{other} (supported: pdf, docx, pptx, xlsx, md, txt)"
        )),
        None => Err(anyhow!(
            "cannot determine file type from path (no extension): {}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unsupported_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xyz");
        std::fs::write(&path, b"data").unwrap();
        let result = extract(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unsupported"),
            "expected 'unsupported' in: {err}"
        );
    }

    #[test]
    fn test_no_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noext");
        std::fs::write(&path, b"data").unwrap();
        let result = extract(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no extension"),
            "expected 'no extension' in: {err}"
        );
    }

    #[test]
    fn test_txt_loader_dispatched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "hello world").unwrap();
        let text = extract(&path).unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn test_md_loader_dispatched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readme.md");
        std::fs::write(&path, "# Title\n\nBody text here.").unwrap();
        let text = extract(&path).unwrap();
        assert!(text.contains("Title"), "expected 'Title' in: {text}");
        assert!(
            text.contains("Body text here"),
            "expected body text in: {text}"
        );
    }

    #[test]
    fn test_docx_loader_dispatched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.docx");
        crate::ingest::docx::tests::write_minimal_docx(&path).unwrap();
        let text = extract(&path).unwrap();
        assert!(!text.is_empty(), "expected non-empty text from docx");
    }

    #[test]
    fn test_pptx_loader_dispatched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pptx");
        crate::ingest::pptx::tests::write_minimal_pptx(&path).unwrap();
        let text = extract(&path).unwrap();
        assert!(!text.is_empty(), "expected non-empty text from pptx");
    }

    #[test]
    fn test_xlsx_loader_dispatched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xlsx");
        crate::ingest::xlsx::tests::write_minimal_xlsx(&path).unwrap();
        let text = extract(&path).unwrap();
        assert!(!text.is_empty(), "expected non-empty text from xlsx");
    }
}
