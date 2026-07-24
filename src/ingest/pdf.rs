//! PDF loader：使用 `pdf-extract` 提取文本。

use std::path::Path;

use anyhow::{Context, Result};

/// 从 PDF 文件中提取文本内容。
///
/// 基于 `pdf-extract` crate，不支持扫描版 PDF（需 OCR）。
/// 扫描版 PDF 解析结果为空或包含少量乱码，不报错。
pub fn extract(path: &Path) -> Result<String> {
    let text = pdf_extract::extract_text(path)
        .with_context(|| format!("failed to extract text from pdf: {}", path.display()))?;
    Ok(text.trim().to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 写一个包含 "Hello World" 的最小合法 PDF。
    ///
    /// 这用的是手写的 PostScript-level PDF，不依赖任何外部 PDF 库。
    /// 各个 obj 的偏移量已在 write! 里显式计算。
    pub(crate) fn write_minimal_pdf(path: &std::path::Path) -> std::io::Result<()> {
        let pdf = minimal_pdf_bytes();
        std::fs::write(path, &pdf)
    }

    fn minimal_pdf_bytes() -> Vec<u8> {
        // 手工构造一个最小 PDF，含 "Hello PDF" 文本。
        // obj 偏移（每行末尾 \n）：
        // 0: %PDF-1.4\n                    → len=9
        // 1: 1 0 obj...\n                  → len=49 → offset 9
        // 2: 2 0 obj...\n                  → len=58 → offset 58
        // 3: 3 0 obj...\n                  → len=131 → offset 116
        // 4: 4 0 obj...\n                  → len=67 → offset 247
        // 5: 5 0 obj...\n                  (stream, length computed)
        // xref + trailer

        let header = b"%PDF-1.4\n";
        let obj1 = b"1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n";
        let obj2 = b"2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n";
        let obj3 =
            b"3 0 obj<</Type/Page/MediaBox[0 0 612 792]/Parent 2 0 R/Resources<</Font<</F1 4 0 R>>>>/Contents 5 0 R>>endobj\n";
        let obj4 = b"4 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\n";

        // content stream with "Hello PDF" text
        let stream_content = b"BT\n/F1 24 Tf\n100 700 Td\n(Hello PDF) Tj\nET";
        let obj5_before_stream = format!("5 0 obj<</Length {}>>stream\n", stream_content.len());
        let obj5_end = b"\nendstream\nendobj\n";

        // compute offsets
        let off_1 = header.len() as u32; // 9
        let off_2 = off_1 + obj1.len() as u32; // 9 + 49 = 58
        let off_3 = off_2 + obj2.len() as u32; // 58 + 58 = 116
        let off_4 = off_3 + obj3.len() as u32; // 116 + 131 = 247
        let off_5 = off_4 + obj4.len() as u32; // 247 + 67 = 314

        let xref_offset = off_5
            + obj5_before_stream.len() as u32
            + stream_content.len() as u32
            + obj5_end.len() as u32;

        let xref = format!(
            "xref\n0 6\n0000000000 65535 f \n{off_1:010} 00000 n \n{off_2:010} 00000 n \n{off_3:010} 00000 n \n{off_4:010} 00000 n \n{off_5:010} 00000 n \n"
        );
        let trailer = format!("trailer<</Size 6/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n");

        let mut buf = Vec::new();
        buf.extend_from_slice(header);
        buf.extend_from_slice(obj1);
        buf.extend_from_slice(obj2);
        buf.extend_from_slice(obj3);
        buf.extend_from_slice(obj4);
        buf.extend_from_slice(obj5_before_stream.as_bytes());
        buf.extend_from_slice(stream_content);
        buf.extend_from_slice(obj5_end);
        buf.extend_from_slice(xref.as_bytes());
        buf.extend_from_slice(trailer.as_bytes());
        buf
    }

    #[test]
    fn test_pdf_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pdf");
        write_minimal_pdf(&path).unwrap();
        let text = extract(&path).unwrap();
        assert!(
            text.to_lowercase().contains("hello"),
            "expected 'hello' in extracted text: {text:?}"
        );
    }

    #[test]
    fn test_pdf_file_not_found() {
        let path = std::path::Path::new("nonexistent.pdf");
        let result = extract(path);
        assert!(result.is_err());
    }
}
