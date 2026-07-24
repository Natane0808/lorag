//! PPTX loader：解 ZIP → 遍历 `ppt/slides/slide*.xml` → 抽取 `<a:t>` 文本。

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

/// 从 .pptx 文件中提取文本（所有 slide 的文本拼在一起）。
///
/// 打开 ZIP → 找到所有 `ppt/slides/slide*.xml` → 用 quick-xml 抽取 `<a:t>` 元素内容。
pub fn extract(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open pptx file: {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("failed to open pptx as zip: {}", path.display()))?;

    // 收集所有 slide 文件的索引
    let slide_indices: Vec<usize> = (0..archive.len())
        .filter(|&i| {
            archive
                .name_for_index(i)
                .map(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
                .unwrap_or(false)
        })
        .collect();

    if slide_indices.is_empty() {
        return Err(anyhow::anyhow!(
            "failed to parse pptx: no slide xml files found in {} (corrupted or not a valid .pptx?)",
            path.display()
        ));
    }

    let mut all_text = String::new();
    for idx in slide_indices {
        let mut xml = String::new();
        let mut entry = archive
            .by_index(idx)
            .with_context(|| format!("failed to read slide entry in pptx: {}", path.display()))?;
        entry
            .read_to_string(&mut xml)
            .with_context(|| format!("failed to read slide xml from: {}", path.display()))?;

        let slide_text = extract_at_text(&xml);
        if !slide_text.is_empty() {
            if !all_text.is_empty() {
                all_text.push('\n');
            }
            all_text.push_str(&slide_text);
        }
    }

    Ok(all_text.trim().to_string())
}

/// 从 PPTX slide XML 字符串中抽取所有 `<a:t>` 元素文本。
fn extract_at_text(xml: &str) -> String {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut output = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"a:t"
                    && let Ok(Event::Text(t)) = reader.read_event_into(&mut Vec::new())
                {
                    let s = t.unescape().unwrap_or_else(|_| {
                        std::borrow::Cow::Owned(
                            std::str::from_utf8(t.as_ref()).unwrap_or("").to_string(),
                        )
                    });
                    output.push_str(&s);
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

    /// 在指定路径创建最小合法 pptx 文件（ZIP 包）。
    pub(crate) fn write_minimal_pptx(path: &std::path::Path) -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let content_types = dir.path().join("[Content_Types].xml");
        let rels_dir = dir.path().join("_rels");
        let ppt_dir = dir.path().join("ppt");
        let slides_dir = ppt_dir.join("slides");
        let ppt_rels_dir = ppt_dir.join("_rels");
        std::fs::create_dir_all(&rels_dir)?;
        std::fs::create_dir_all(&slides_dir)?;
        std::fs::create_dir_all(&ppt_rels_dir)?;

        // [Content_Types].xml
        std::fs::write(
            &content_types,
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#,
        )?;

        // _rels/.rels
        std::fs::write(
            rels_dir.join(".rels"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#,
        )?;

        // ppt/presentation.xml
        std::fs::write(
            ppt_dir.join("presentation.xml"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:sldIdLst>
    <p:sldId id="256" r:id="rId1"/>
  </p:sldIdLst>
</p:presentation>"#,
        )?;

        // ppt/_rels/presentation.xml.rels
        std::fs::write(
            ppt_rels_dir.join("presentation.xml.rels"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#,
        )?;

        // ppt/slides/slide1.xml
        std::fs::write(
            slides_dir.join("slide1.xml"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:txBody>
          <a:bodyPr/>
          <a:p>
            <a:r>
              <a:t>Hello from PPTX</a:t>
            </a:r>
          </a:p>
          <a:p>
            <a:r>
              <a:t>Slide 1 content</a:t>
            </a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#,
        )?;

        // 打包 ZIP
        let out_file = std::fs::File::create(path)?;
        let mut zip_writer = zip::ZipWriter::new(out_file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        let entries = [
            (
                "[Content_Types].xml",
                std::fs::read_to_string(&content_types).unwrap(),
            ),
            (
                "_rels/.rels",
                std::fs::read_to_string(rels_dir.join(".rels")).unwrap(),
            ),
            (
                "ppt/presentation.xml",
                std::fs::read_to_string(ppt_dir.join("presentation.xml")).unwrap(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                std::fs::read_to_string(ppt_rels_dir.join("presentation.xml.rels")).unwrap(),
            ),
            (
                "ppt/slides/slide1.xml",
                std::fs::read_to_string(slides_dir.join("slide1.xml")).unwrap(),
            ),
        ];

        for (entry_name, content) in &entries {
            zip_writer.start_file(*entry_name, options)?;
            zip_writer.write_all(content.as_bytes())?;
        }
        zip_writer.finish()?;
        Ok(())
    }

    #[test]
    fn test_pptx_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pptx");
        write_minimal_pptx(&path).unwrap();
        let text = extract(&path).unwrap();
        assert!(
            text.contains("Hello from PPTX"),
            "expected 'Hello from PPTX' in: {text:?}"
        );
        assert!(
            text.contains("Slide 1 content"),
            "expected 'Slide 1 content' in: {text:?}"
        );
    }

    #[test]
    fn test_pptx_file_not_found() {
        let path = std::path::Path::new("nonexistent.pptx");
        let result = extract(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_at_text_basic() {
        let xml = r#"<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:p><a:r><a:t>Slide text</a:t></a:r></a:p></p:sld>"#;
        let text = extract_at_text(xml);
        assert_eq!(text, "Slide text");
    }
}
