//! XLSX loader：使用 `calamine` 读取所有 sheet 并拼接单元格文本。

use std::fmt::Write;
use std::path::Path;

use anyhow::Result;
use calamine::Reader;

/// 从 .xlsx 文件中提取所有 sheet 的单元格文本。
///
/// 使用 calamine 的 `open_workbook_auto` 自动识别格式（xlsx / xls / ods）。
/// 每个 sheet 内的单元格用 tab 分隔，每行用换行分隔。
pub fn extract(path: &Path) -> Result<String> {
    let mut workbook = calamine::open_workbook_auto(path)
        .map_err(|e| anyhow::anyhow!("failed to open xlsx: {}: {}", path.display(), e))?;

    let sheet_names = workbook.sheet_names().to_vec();
    if sheet_names.is_empty() {
        return Err(anyhow::anyhow!(
            "failed to parse xlsx: no sheets found in {}",
            path.display()
        ));
    }

    let mut output = String::new();
    for sheet_name in &sheet_names {
        let range = workbook.worksheet_range(sheet_name).map_err(|e| {
            anyhow::anyhow!(
                "failed to read sheet '{}' in {}: {}",
                sheet_name,
                path.display(),
                e
            )
        })?;

        let mut sheet_text = String::new();

        // 计算实际使用的列数和行数
        let (start_row, end_row) = range
            .start()
            .ok_or_else(|| anyhow::anyhow!("xlsx sheet '{}' has no rows", sheet_name))?;

        if end_row < start_row {
            continue;
        }

        // 第一个 sheet 不需要前缀，后续 sheet 加 sheet 名标识
        if !output.is_empty() {
            output.push('\n');
        }
        if sheet_names.len() > 1 {
            output.push_str(&format!("--- Sheet: {} ---\n", sheet_name));
        }

        let rows = range.rows();
        for row in rows {
            let row_text: Vec<String> = row
                .iter()
                .map(|cell| cell.to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !row_text.is_empty() {
                writeln!(&mut sheet_text, "{}", row_text.join("\t")).unwrap();
            }
        }

        output.push_str(sheet_text.trim_end());
    }

    Ok(output.trim().to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// 在指定路径创建最小合法 xlsx 文件（ZIP 包，含 shared strings）。
    pub(crate) fn write_minimal_xlsx(path: &std::path::Path) -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let content_types = dir.path().join("[Content_Types].xml");
        let rels_dir = dir.path().join("_rels");
        let xl_dir = dir.path().join("xl");
        let worksheets_dir = xl_dir.join("worksheets");
        let xl_rels_dir = xl_dir.join("_rels");
        std::fs::create_dir_all(&rels_dir)?;
        std::fs::create_dir_all(&worksheets_dir)?;
        std::fs::create_dir_all(&xl_rels_dir)?;

        // [Content_Types].xml
        std::fs::write(
            &content_types,
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>"#,
        )?;

        // _rels/.rels
        std::fs::write(
            rels_dir.join(".rels"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
        )?;

        // xl/workbook.xml
        std::fs::write(
            xl_dir.join("workbook.xml"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#,
        )?;

        // xl/_rels/workbook.xml.rels
        std::fs::write(
            xl_rels_dir.join("workbook.xml.rels"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
        )?;

        // xl/worksheets/sheet1.xml
        std::fs::write(
            worksheets_dir.join("sheet1.xml"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="B1" t="s"><v>1</v></c>
    </row>
    <row r="2">
      <c r="A2" t="s"><v>2</v></c>
      <c r="B2" t="s"><v>3</v></c>
    </row>
  </sheetData>
</worksheet>"#,
        )?;

        // xl/sharedStrings.xml
        std::fs::write(
            xl_dir.join("sharedStrings.xml"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="4" uniqueCount="4">
  <si><t>Hello</t></si>
  <si><t>World</t></si>
  <si><t>from</t></si>
  <si><t>XLSX</t></si>
</sst>"#,
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
                "xl/workbook.xml",
                std::fs::read_to_string(xl_dir.join("workbook.xml")).unwrap(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                std::fs::read_to_string(xl_rels_dir.join("workbook.xml.rels")).unwrap(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                std::fs::read_to_string(worksheets_dir.join("sheet1.xml")).unwrap(),
            ),
            (
                "xl/sharedStrings.xml",
                std::fs::read_to_string(xl_dir.join("sharedStrings.xml")).unwrap(),
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
    fn test_xlsx_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xlsx");
        write_minimal_xlsx(&path).unwrap();
        let text = extract(&path).unwrap();
        assert!(text.contains("Hello"), "expected 'Hello' in: {text:?}");
        assert!(text.contains("World"), "expected 'World' in: {text:?}");
        assert!(text.contains("from"), "expected 'from' in: {text:?}");
        assert!(text.contains("XLSX"), "expected 'XLSX' in: {text:?}");
    }

    #[test]
    fn test_xlsx_file_not_found() {
        let path = std::path::Path::new("nonexistent.xlsx");
        let result = extract(path);
        assert!(result.is_err());
    }
}
