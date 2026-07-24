//! Plain text loader: UTF-8 文件直接读取全文。

use std::path::Path;

use anyhow::{Context, Result};

/// 读取 UTF-8 文本文件全部内容。
///
/// 不做任何格式转换，直接把文件内容作为纯文本返回。
pub fn extract(path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read text file: {}", path.display()))?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_txt_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "Hello, 世界!\n第二行。").unwrap();
        let text = extract(&path).unwrap();
        assert_eq!(text, "Hello, 世界!\n第二行。");
    }

    #[test]
    fn test_txt_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();
        let text = extract(&path).unwrap();
        assert_eq!(text, "");
    }

    #[test]
    fn test_txt_file_not_found() {
        let path = std::path::Path::new("nonexistent_file.txt");
        let result = extract(path);
        assert!(result.is_err());
    }
}
