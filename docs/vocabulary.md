# 词库来源与导入格式

quietly 不内置完整词库（体积与授权都不合适），而是把“词条”和“词书”分开：

- `words` 表存词条（拼写、音标、释义、词根、例句）
- `books` + `book_words` 存词书和顺序

所以你可以先用一本词典打底，再叠加任意多个词书、例句集。

## 推荐来源

| 来源 | 内容 | 授权 | 用法 |
| --- | --- | --- | --- |
| [ECDICT](https://github.com/skywind3000/ECDICT) | 77 万英汉词条，含音标、释义、`tag`（中高考/四六级/考研/托福/雅思/GRE）、柯林斯星级、BNC/COCA 词频 | MIT | **首选**。下载 `ecdict-csv.7z` 解压出 `stardict.csv` 后 `quietly import stardict.csv`，自动生成 8 本词书 |
| [qwerty-learner](https://github.com/Kaiyiwing/qwerty-learner) 词库 | CET-4/6、考研、IELTS、TOEFL、GRE、GMAT、SAT、专四专八、商务英语等 JSON 词表，每词带释义与例句 | MIT | 把 `public/dicts/*.json` 转成下面的通用 JSON 格式导入 |
| [Tatoeba](https://tatoeba.org/) | 多语例句对，`cmn-eng` 可得到中英对照例句 | CC-BY 2.0 FR | 导出 `sentence_id \t 英文 \t 中文`，转成 `examples.csv` 合并例句 |
| [kaikki.org / Wiktionary](https://kaikki.org/dictionary/) | 英文词条 + 中文释义的结构化 JSON 抽取 | CC BY-SA | 写个小脚本转成通用 JSON；数据量大，建议只取需要的词 |
| [COCA / BNC 词频表](https://www.wordfrequency.info/) | 按使用频率排序 | 各站不同，注意商用条款 | 用 `rank` 顺序决定新词出现顺序 |
| [CEFR-J Wordlist](https://github.com/openlanguageprofiles/olp-en-cefrj) | A1–B2 分级词汇 | CC BY-SA | 适合做“从 A1 开始”的入门词书 |

### 关于扇贝 / 百词斩词书

扇贝、百词斩等产品的词书数据没有公开授权，**不要直接抓取或导出**。
推荐替代：用 ECDICT 的 `tag`（`cet4`/`cet6`/`ky`/`toefl`/`ielts`/`gre`）直接生成同级别词书，
或用 qwerty-learner 的 MIT 词表，覆盖范围基本一致。

## 通用 JSON

最小结构：

```json
{
  "name": "我的词书",
  "slug": "my-book",
  "description": "可选",
  "words": [
    {
      "lemma": "inspect",
      "phonetic": "/ɪnˈspekt/",
      "translation": "v. 检查；视察",
      "definition": "to look at something carefully",
      "roots": [
        { "part": "in-", "gloss": "向内", "kind": "prefix" },
        { "part": "spect", "gloss": "看", "kind": "root" }
      ],
      "examples": [
        { "en": "Engineers inspect the bridge every year.", "zh": "工程师每年检查这座桥。" }
      ],
      "tags": ["cet4"]
    }
  ]
}
```

也接受这些简写：

- 顶层直接是数组：`[{...}]`（词书名取文件名）
- 多本词书：`{"books": [ {"name": ..., "words": [...]}, ... ]}` 或 `{"词书名": [...]}`
- 单词对象里用 `word` 代替 `lemma`
- `"roots": "in-:向内:prefix;spect:看:root"`（分号分隔，冒号分段，kind ∈ `prefix`/`root`/`suffix`，缺省为 `root`）
- `"example": "...", "example_zh": "..."` 作为单条例句

> 所有字段都可缺省；缺 `translation` 时会在卡片上显示“暂无释义”。

## 通用 CSV / TSV

第一行是表头，列名不区分大小写；`tsv` 后缀或表头里 tab 更多时自动按制表符解析。

```csv
word,translation,phonetic,definition,roots,example,example_zh,tags,book
inspect,"v. 检查；视察",/ɪnˈspekt/,look at carefully,"in-:向内:prefix;spect:看:root",Engineers inspect the bridge.,工程师检查这座桥。,cet4,CET-4 核心词
```

可识别的列名（同义列任选其一）：

| 字段 | 列名 |
| --- | --- |
| 拼写 | `word` / `lemma` |
| 释义 | `translation` / `meaning` / `释义` / `中文` |
| 音标 | `phonetic` / `音标` |
| 英文释义 | `definition` / `english` / `英文释义` |
| 词根 | `roots` / `root` / `词根` |
| 例句 | `example` / `sentence` / `例句` |
| 例句翻译 | `example_zh` / `zh` / `sentence_zh` / `例句翻译` |
| 标签 | `tags` / `tag`（逗号或空格分隔） |
| 词书 | `book` / `book_name` / `词书`（不给则用文件名） |

## 例句合并

```csv
word,sentence,translation
inspect,Engineers inspect the bridge every year.,工程师每年检查这座桥。
```

`quietly import examples.csv` 只往已有词条追加例句（每词最多保留 8 条，重复句子自动跳过），
不会新建词条。适合用 Tatoeba 批量补语料。

## 纯词表

`list.txt` 一行一个词，`#` 开头为注释。词条必须已存在于 `words` 表（例如先导入 ECDICT），
否则会计入 `skipped`。词书名取文件名。

## 导入行为

- 同一拼写（忽略大小写）视为同一个 `word`，重复导入只更新空缺字段（音标、释义、词根、例句、标签）
- 词书按 `slug` 去重，`book_words.rank` 递增，新词按 `rank` 顺序出现
- 已存在的学习进度（`cards`）完全不动，可以放心重复导入
- 导入结果会显示 `added / updated / skipped`

## 数据位置

| 平台 | 路径 |
| --- | --- |
| Windows | `%APPDATA%\quietly-english\quietly.db` |
| macOS | `~/Library/Application Support/quietly-english/quietly.db` |
| Linux | `~/.local/share/quietly-english/quietly.db` |

用 `quietly where` 查看实际路径，`--db <path>` 或 `QUIETLY_HOME` 覆盖。
