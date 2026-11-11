//! StarDict 词典导入。
//!
//! ECDICT 的发布包有两种：`ecdict-sqlite-28.zip`（现成 SQLite）和
//! `ecdict-stardict-28.zip`（StarDict 二进制）。这里读后者，因为体积最小、
//! 也不需要额外的 SQLite 依赖。
//!
//! StarDict 的结构：
//!
//! * `*.ifo`：元数据，`wordcount` 给出词条数，`sametypesequence` 给出内容格式
//! * `*.idx`：每条 `词条\0` + 4 字节大端偏移 + 4 字节大端长度
//! * `*.dict`：数据区，按偏移取片段
//!
//! ECDICT 的条目内容形如：
//!
//! ```text
//! *[ɪnˈspekt]
//! vt. 检查，检验；视察
//! ```
//!
//! 首行是音标，其余行是释义。`sametypesequence=m` 表示片段本身就是纯文本。
//!
//! 数据区有两种存放方式：未压缩的 `.dict`（ECDICT），以及 dictzip 压缩的
//! `.dict.dz`（胡正站的绝大多数词典）。后者是带 FEXTRA 的 gzip 流，可以整体
//! 顺序解压；索引里的偏移是**解压后**的偏移，所以解压流上的跳过是逐字节丢弃。

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use flate2::read::MultiGzDecoder;

use qe_core::model::{Example, Word};

/// StarDict 词典的元数据。
#[derive(Debug, Clone)]
pub struct Meta {
    pub book_name: String,
    pub word_count: u64,
    pub idx_size: u64,
    /// `m` = 纯文本释义，`t` = 纯文本，`h` = HTML …
    pub same_type_sequence: String,
}

/// 打开一组 StarDict 文件。传入 `.ifo`、`.idx` 或 `.dict` 路径均可。
pub struct Stardict {
    meta: Meta,
    idx: BufReader<File>,
    dict: DictData,
    /// 已按顺序读取的下一个数据位置；索引通常递增，顺序读比 seek 快一个量级。
    dict_pos: u64,
}

/// 数据区。压缩的只能顺序读，但索引里的偏移本来也是递增的。
enum DictData {
    Plain(BufReader<File>),
    /// dictzip（`.dict.dz`）是带 FEXTRA 的 gzip，整体顺序解压即可。
    Compressed(BufReader<MultiGzDecoder<File>>),
}

impl Stardict {
    pub fn open(path: &Path) -> Result<Self> {
        let ifo_path = with_extension(path, "ifo");
        let idx_path = with_extension(path, "idx");
        let dict_path = with_extension(path, "dict");

        let meta = read_meta(&ifo_path)?;
        let idx = BufReader::with_capacity(
            256 * 1024,
            File::open(&idx_path).with_context(|| format!("无法打开 {}", idx_path.display()))?,
        );

        let dict = if dict_path.exists() {
            DictData::Plain(BufReader::with_capacity(
                512 * 1024,
                File::open(&dict_path)
                    .with_context(|| format!("无法打开 {}", dict_path.display()))?,
            ))
        } else {
            let dz = PathBuf::from(format!("{}.dz", dict_path.to_string_lossy()));
            let file = File::open(&dz).with_context(|| {
                format!(
                    "缺少数据区：{} 和 {} 都不存在",
                    dict_path.display(),
                    dz.display()
                )
            })?;
            DictData::Compressed(BufReader::with_capacity(
                512 * 1024,
                MultiGzDecoder::new(file),
            ))
        };

        Ok(Self {
            meta,
            idx,
            dict,
            dict_pos: 0,
        })
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// 逐条读取。`on_word` 返回 `true` 表示继续。
    pub fn for_each_word(
        &mut self,
        mut progress: impl FnMut(u64),
        mut on_word: impl FnMut(Word) -> Result<()>,
    ) -> Result<u64> {
        let mut count = 0u64;
        let mut buffer: Vec<u8> = Vec::with_capacity(256);
        let mut entry: Vec<u8> = Vec::with_capacity(512);

        loop {
            buffer.clear();
            // 词条以 `\0` 结尾。
            let read = self.idx.read_until(0, &mut buffer)?;
            if read == 0 {
                break;
            }
            if buffer.last() != Some(&0) {
                // 索引末尾被截断，停止。
                break;
            }
            buffer.pop();
            if buffer.is_empty() {
                continue;
            }

            let mut offset_bytes = [0u8; 4];
            let mut size_bytes = [0u8; 4];
            if self.idx.read_exact(&mut offset_bytes).is_err()
                || self.idx.read_exact(&mut size_bytes).is_err()
            {
                break;
            }
            let offset = u32::from_be_bytes(offset_bytes) as u64;
            let size = u32::from_be_bytes(size_bytes) as usize;
            if size == 0 {
                continue;
            }

            let Ok(lemma) = String::from_utf8(buffer.clone()) else {
                continue;
            };
            let Ok(lemma) = normalise_lemma(lemma.trim()) else {
                continue;
            };

            entry.resize(size, 0);
            self.read_at(offset, &mut entry)?;

            let Ok(text) = String::from_utf8(entry.clone()) else {
                continue;
            };
            let (phonetic, meaning) = split_entry(&text);
            if meaning.is_empty() {
                continue;
            }

            count += 1;
            if count % 100_000 == 0 {
                progress(count);
            }
            on_word(Word {
                lemma,
                phonetic,
                translation: meaning,
                ..Default::default()
            })?;
        }

        Ok(count)
    }

    /// 读取 `dict` 中 `[offset, offset+len)` 的内容，尽量顺序前进。
    fn read_at(&mut self, offset: u64, target: &mut [u8]) -> Result<()> {
        let gap = offset as i64 - self.dict_pos as i64;
        if gap != 0 {
            match &mut self.dict {
                DictData::Plain(file) => {
                    if (0..1024 * 1024).contains(&gap) {
                        skip_forward(file, gap as u64)?;
                    } else {
                        file.seek(SeekFrom::Start(offset))?;
                    }
                }
                DictData::Compressed(reader) => {
                    // 解压流无法回退；索引本身递增，正向跳过是唯一需要的方向。
                    if gap < 0 {
                        bail!("索引偏移不是递增的，压缩数据区无法回退");
                    }
                    skip_forward(reader, gap as u64)?;
                }
            }
        }
        match &mut self.dict {
            DictData::Plain(file) => file.read_exact(target)?,
            DictData::Compressed(reader) => reader.read_exact(target)?,
        }
        self.dict_pos = offset + target.len() as u64;
        Ok(())
    }
}

/// 丢弃 `count` 字节。压缩流上等价于解压后跳过。
fn skip_forward(reader: &mut impl Read, count: u64) -> Result<()> {
    let mut skip = count;
    let mut hole = [0u8; 8192];
    while skip > 0 {
        let take = skip.min(hole.len() as u64) as usize;
        reader.read_exact(&mut hole[..take])?;
        skip -= take as u64;
    }
    Ok(())
}

/// 只保留能用键盘打出来、长度合理的词条，过滤掉符号条目与超长短语。
fn normalise_lemma(raw: &str) -> Result<String> {
    let lemma = raw.trim();
    if lemma.is_empty() || lemma.len() > 48 {
        bail!("skip");
    }
    let ok = lemma.chars().all(|ch| {
        ch.is_ascii_alphabetic() || ch == '\'' || ch == '-' || ch == ' ' || ch == '.'
    });
    if !ok {
        bail!("skip");
    }
    // 至少含一个字母。
    if !lemma.chars().any(|ch| ch.is_ascii_alphabetic()) {
        bail!("skip");
    }
    Ok(lemma.to_lowercase())
}

/// 拆分 `*[音标]\n释义`。
fn split_entry(text: &str) -> (Option<String>, String) {
    let mut rest = text;
    let mut phonetic = None;

    // 音标行可能不止一行，也可能缺失。
    while let Some(stripped) = rest.strip_prefix('*') {
        let Some(end) = stripped.find(']') else { break };
        let inside = stripped[..end].trim_start_matches('[').trim();
        if !inside.is_empty() && phonetic.is_none() && looks_like_phonetic(inside) {
            phonetic = Some(format!("/{inside}/"));
        }
        rest = stripped[end + 1..].trim_start_matches('\n');
    }

    let meaning = rest
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        // `[网络] ...` 这类标签行保留，纯粹的交换信息行跳过。
        .collect::<Vec<_>>()
        .join("\n");

    (phonetic, meaning.trim().to_string())
}

/// IPA 音标只用有限的符号集；用它区分 `*[ˈæpl]` 和 `*[注释]`。
fn looks_like_phonetic(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|ch| {
            ch.is_ascii_alphabetic()
                || ch.is_ascii_digit()
                || "ˈˌːəɜɔɪʊʌæɑɒθðʃʒŋɡɹɾʔ()/,.;' -".contains(ch)
        })
}

fn with_extension(path: &Path, ext: &str) -> PathBuf {
    let mut out = path.to_path_buf();
    // `stardict-ecdict-2.4.2.dict` → `stardict-ecdict-2.4.2.<ext>`
    if out.to_string_lossy().ends_with(&format!(".{ext}")) {
        return out;
    }
    out.set_extension(ext);
    out
}

fn read_meta(ifo: &Path) -> Result<Meta> {
    let text = std::fs::read_to_string(ifo)
        .with_context(|| format!("无法读取 {}", ifo.display()))?;
    let mut book_name = String::new();
    let mut word_count = 0u64;
    let mut idx_size = 0u64;
    let mut same_type_sequence = String::from("m");

    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "bookname" => book_name = value.trim().to_string(),
            "wordcount" => word_count = value.trim().parse().unwrap_or(0),
            "idxfilesize" => idx_size = value.trim().parse().unwrap_or(0),
            "sametypesequence" => same_type_sequence = value.trim().to_string(),
            _ => {}
        }
    }

    if word_count == 0 {
        bail!("{} 缺少 wordcount", ifo.display());
    }
    Ok(Meta {
        book_name,
        word_count,
        idx_size,
        same_type_sequence,
    })
}

/// 把释义拆成例句无关的纯文本，去掉 ECDICT 的 `[网络]` 等前缀标签价值不高，
/// 但保留第一行的词性缩写。
pub fn examples_placeholder() -> Vec<Example> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_phonetic_and_meaning() {
        let (phonetic, meaning) = split_entry("*[ɪnˈspekt]\nvt. 检查，检验；视察\nn. 检查");
        assert_eq!(phonetic.as_deref(), Some("/ɪnˈspekt/"));
        assert_eq!(meaning, "vt. 检查，检验；视察\nn. 检查");
    }

    #[test]
    fn handles_missing_phonetic() {
        let (phonetic, meaning) = split_entry("n. 苹果\nvt. 渴望");
        assert!(phonetic.is_none());
        assert_eq!(meaning, "n. 苹果\nvt. 渴望");
    }

    #[test]
    fn does_not_treat_a_note_as_phonetic() {
        let (phonetic, meaning) = split_entry("*[网络] 苹果\nn. 苹果");
        assert!(phonetic.is_none(), "中文注释不是音标");
        assert!(meaning.contains("苹果"));
    }

    #[test]
    fn lemma_filter_drops_noise() {
        assert_eq!(normalise_lemma("Inspect").unwrap(), "inspect");
        assert_eq!(normalise_lemma("ice cream").unwrap(), "ice cream");
        assert!(normalise_lemma("…").is_err());
        assert!(normalise_lemma("").is_err());
        assert!(normalise_lemma(&"a".repeat(60)).is_err());
    }
}
