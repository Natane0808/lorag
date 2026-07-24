//! DOCX loader：解 ZIP → 读 `word/document.xml` → 抽取 `<w:t>` 文本。

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

/// 从 .docx 文件中提取文本。
///
/// 打开 ZIP → 读取 `word/document.xml` → 用 quick-xml 遍历抽取所有 `<w:t>` 元素内容。
pub fn extract(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open docx file: {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("failed to open docx as zip: {}", path.display()))?;

    // 读取 word/document.xml
    let mut doc_xml = String::new();
    let mut entry = archive.by_name("word/document.xml").with_context(|| {
        format!(
            "failed to find word/document.xml in docx: {} (corrupted or not a valid .docx?)",
            path.display()
        )
    })?;
    entry
        .read_to_string(&mut doc_xml)
        .with_context(|| format!("failed to read word/document.xml from: {}", path.display()))?;

    let text = extract_wt_text(&doc_xml);
    Ok(text)
}

/// 从 DOCX 的 document.xml 字符串中抽取所有 `<w:t>` 元素文本。
fn extract_wt_text(xml: &str) -> String {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut output = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                // 只有 <w:t> 才取其文本
                if e.name().as_ref() == b"w:t" {
                    // 读 inner text（quick-xml 会在下次 Text 事件给出）
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut Vec::new()) {
                        let s = t.unescape().unwrap_or_else(|_| {
                            std::borrow::Cow::Owned(
                                std::str::from_utf8(t.as_ref()).unwrap_or("").to_string(),
                            )
                        });
                        output.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    output.trim().to_string()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// 在指定路径创建最小合法 docx 文件（ZIP 包）。
    pub(crate) fn write_minimal_docx(path: &std::path::Path) -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let content_types = dir.path().join("[Content_Types].xml");
        let rels_dir = dir.path().join("_rels");
        let word_dir = dir.path().join("word");
        std::fs::create_dir_all(&rels_dir)?;
        std::fs::create_dir_all(&word_dir)?;

        // [Content_Types].xml
        std::fs::write(
            &content_types,
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#,
        )?;

        // _rels/.rels
        std::fs::write(
            rels_dir.join(".rels"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#,
        )?;

        // word/document.xml
        std::fs::write(
            word_dir.join("document.xml"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:t>Hello from DOCX</w:t>
      </w:r>
    </w:p>
    <w:p>
      <w:r>
        <w:t>Second paragraph.</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#,
        )?;

        // 打包 ZIP
        let out_file = std::fs::File::create(path)?;
        let mut zip_writer = zip::ZipWriter::new(out_file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        for (entry_name, content) in &[
            (
                "[Content_Types].xml",
                std::fs::read_to_string(&content_types).unwrap(),
            ),
            (
                "_rels/.rels",
                std::fs::read_to_string(rels_dir.join(".rels")).unwrap(),
            ),
            (
                "word/document.xml",
                std::fs::read_to_string(word_dir.join("document.xml")).unwrap(),
            ),
        ] {
            zip_writer.start_file(*entry_name, options)?;
            zip_writer.write_all(content.as_bytes())?;
        }
        zip_writer.finish()?;
        Ok(())
    }

    #[test]
    fn test_docx_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.docx");
        write_minimal_docx(&path).unwrap();
        let text = extract(&path).unwrap();
        assert!(
            text.contains("Hello from DOCX"),
            "expected 'Hello from DOCX' in: {text:?}"
        );
        assert!(
            text.contains("Second paragraph"),
            "expected 'Second paragraph' in: {text:?}"
        );
    }

    #[test]
    fn test_docx_file_not_found() {
        let path = std::path::Path::new("nonexistent.docx");
        let result = extract(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_wt_text_single() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>"#;
        let text = extract_wt_text(xml);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn test_extract_wt_text_multiple() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>A</w:t></w:r><w:r><w:t>B</w:t></w:r></w:p></w:body></w:document>"#;
        let text = extract_wt_text(xml);
        assert_eq!(text, "AB");
    }
}
