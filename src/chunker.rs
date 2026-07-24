//! 文本分块：段落级切分 + 长段落滑动窗口。

use std::path::Path;

use crate::models::Chunk;

/// 把纯文本按段落（`\n\n`）切分，长段落用滑动窗口再细分。
///
/// ## 算法
///
/// 1. 按 `\n\n` 分割成段落
/// 2. 每个段落 ≤ `chunk_size` → 保留为一个 chunk
/// 3. 每个段落 > `chunk_size` → 用长度为 `chunk_size`、步长为
///    `chunk_size - chunk_overlap` 的滑动窗口切
/// 4. 单字符（换行）段落丢弃
pub fn split(
    text: &str,
    source_path: &Path,
    chunk_size: usize,
    chunk_overlap: usize,
) -> Vec<Chunk> {
    let source_path = source_path.to_string_lossy().to_string();
    let step = chunk_size - chunk_overlap;
    let mut ordinal = 0usize;
    let mut chunks = Vec::new();

    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }

        let char_indices: Vec<usize> = para.char_indices().map(|(i, _)| i).collect();
        let len = char_indices.len(); // 字符数

        if len <= chunk_size {
            chunks.push(Chunk {
                text: para.to_string(),
                ordinal,
                source_path: source_path.clone(),
            });
            ordinal += 1;
        } else {
            // 滑动窗口切长段落
            let mut start_char = 0usize;
            while start_char < len {
                let end_char = (start_char + chunk_size).min(len);
                let byte_start = char_indices[start_char];
                let byte_end = if end_char < len {
                    char_indices[end_char]
                } else {
                    para.len()
                };

                let slice = &para[byte_start..byte_end];
                chunks.push(Chunk {
                    text: slice.to_string(),
                    ordinal,
                    source_path: source_path.clone(),
                });
                ordinal += 1;

                if end_char >= len {
                    break;
                }
                start_char += step;
            }
        }
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_paragraphs() {
        let text = "hello world\n\nfoo bar\n\nbaz";
        let path = Path::new("test.txt");
        let chunks = split(text, path, 500, 50);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "hello world");
        assert_eq!(chunks[1].text, "foo bar");
        assert_eq!(chunks[2].text, "baz");
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.ordinal, i);
            assert!(c.source_path.contains("test.txt"));
        }
    }

    #[test]
    fn test_long_paragraph_split() {
        // 用 "A" 填充一个 300 字符的段落，chunk_size=100, overlap=20
        let long = "A".repeat(300);
        let path = Path::new("long.txt");
        let chunks = split(&long, path, 100, 20);
        // 步长 = 100 - 20 = 80
        // win0: chars[0..100], win1: chars[80..180], win2: chars[160..260],
        // win3: chars[240..300] = 60 chars（最后一段不填满）
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].text.len(), 100);
        assert_eq!(chunks[1].text.len(), 100);
        assert_eq!(chunks[2].text.len(), 100);
        assert_eq!(chunks[3].text.len(), 60);
    }

    #[test]
    fn test_empty_text() {
        let chunks = split("", Path::new("e.txt"), 100, 20);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_overlap_content() {
        // 构造 "abcdefghij" (10 chars), chunk=5, overlap=2 → step=3
        // windows: "abcde", "defgh", "ghij"
        let text = "abcdefghij";
        let path = Path::new("o.txt");
        let chunks = split(text, path, 5, 2);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "abcde");
        assert_eq!(chunks[1].text, "defgh");
        assert_eq!(chunks[2].text, "ghij");
    }
}
