# 词库来源与导入格式

quietly 不内置完整词库（体积与授权都不合适），而是把“词条”和“词书”分开：

- `words` 表存词条（拼写、音标、释义、词根、例句）
- `books` + `book_words` 存词书和顺序

所以你可以先用一本词典打底，再叠加任意多个词书、例句集。

## 推荐来源

> 各文件的具体下载地址、体积和导入命令汇总在 [data-sources.md](data-sources.md)。
> **词根不靠算法猜**：只从 Wiktionary 词源模板或自备词根表读取，见下方「词根与词源」。

| 来源 | 内容 | 授权 | 用法 |
| --- | --- | --- | --- |
| [ECDICT](https://github.com/skywind3000/ECDICT) | 77 万英汉词条，含音标、释义、`tag`（中高考/四六级/考研/托福/雅思/GRE）、柯林斯星级、BNC/COCA 词频 | MIT | **查词底库**。下载 `ecdict-stardict-28.zip` 后 `quietly dict stardict-ecdict-2.4.2.ifo`；或用 `ecdict-csv.7z` 里的 `stardict.csv` 走 `quietly import` |
| [qwerty-learner](https://github.com/Kaiyiwing/qwerty-learner) 词库 | CET-4/6、考研、IELTS、TOEFL、GRE、GMAT、SAT、专四专八、商务英语等 JSON 词表，每词带释义与音标 | MIT | **免转换**：直接 `quietly import CET6_T.json`，见下方「按考试直接导入」 |
| [Tatoeba](https://tatoeba.org/) | 多语例句对，`cmn-eng` 可得到中英对照例句 | CC-BY 2.0 FR | 导出 `sentence_id \t 英文 \t 中文`，转成 `examples.csv` 合并例句 |
| [kaikki.org / Wiktionary](https://kaikki.org/dictionary/) | 英文词条 + 中文释义的结构化 JSON 抽取 | CC BY-SA | 写个小脚本转成通用 JSON；数据量大，建议只取需要的词 |
| [COCA / BNC 词频表](https://www.wordfrequency.info/) | 按使用频率排序 | 各站不同，注意商用条款 | 用 `rank` 顺序决定新词出现顺序 |
| [CEFR-J Wordlist](https://github.com/openlanguageprofiles/olp-en-cefrj) | A1–B2 分级词汇 | CC BY-SA | 适合做“从 A1 开始”的入门词书 |

### 关于扇贝 / 百词斩词书

扇贝、百词斩等产品的词书数据没有公开授权，**不要直接抓取或导出**。
推荐替代：用 ECDICT 的 `tag`（`cet4`/`cet6`/`ky`/`toefl`/`ielts`/`gre`）直接生成同级别词书，
或用 qwerty-learner 的 MIT 词表，覆盖范围基本一致。

## 按考试直接导入（最快）

[qwerty-learner 的 `public/dicts/`](https://github.com/RealKai42/qwerty-learner/tree/master/public/dicts)
里每个考试一个文件，**不需要转换格式**，直接导入即可：

```bash
# CET-6（2345 词）
curl -L -o CET6_T.json https://raw.githubusercontent.com/RealKai42/qwerty-learner/master/public/dicts/CET6_T.json
quietly import CET6_T.json

# CET-4 / 考研 / IELTS / TOEFL / GRE 同理
curl -L -o CET4_T.json https://raw.githubusercontent.com/RealKai42/qwerty-learner/master/public/dicts/CET4_T.json
curl -L -o KaoYan_3_T.json https://raw.githubusercontent.com/RealKai42/qwerty-learner/master/public/dicts/KaoYan_3_T.json
```

GitHub 慢的话在链接前加任意代理前缀，例如
`https://ghfast.top/https://raw.githubusercontent.com/...`。

导入器认识 `{"name": ..., "trans": [...], "usphone": ...}` 这种 qwerty-learner
原生结构，并且会**按文件名自动归入对应词书**，不会多出一本 `cet6-t`：

| 文件名（忽略大小写与 `_`/`-`） | 归入词书 |
| --- | --- |
| `CET4*` / `CET-4*` | `cet4` · CET-4 核心词 |
| `CET6*` / `CET-6*` | `cet6` · CET-6 核心词 |
| `TOEFL*` | `toefl` · TOEFL 核心词 |
| `IELTS*` | `ielts` · IELTS 核心词 |
| `GRE*` | `gre` · GRE 核心词 |
| `KaoYan*` / `KY*` | `ky` · 考研核心词 |
| `GaoKao*` / `GK*` | `gk` · 高考核心词 |
| `ZhongKao*` / `ZK*` | `zk` · 中考核心词 |

已验证：`CET6_T.json` 导入后 `cet6` 词书从 12 词变成 2347 词，`added 2321 · updated 24`。

也可以直接在 TUI 里 `/import CET6_T.json`，`tab` 会补全当前目录的文件名。

> 这些词表只有拼写、释义、音标，没有词根和例句。词根会用内置词缀表自动推测
> （卡片上标注 `guessed`），例句可以用 Tatoeba 单独补。

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

## 常用词组 / 固定搭配

ECDICT 本身没有搭配数据，`sake` 的「清酒」这类生僻义也不在它的条目里，所以搭配与补充义
单独维护，导入时**只更新已有词条**，不会凭空造词。

### 从词典里抽

朗道英汉字典在词条末尾有 `相关词组:` 段，是编者列的固定搭配。`quietly dict ... --merge`
（或不带 `--merge`）导入时会自动把它抽出来存进搭配字段，`sake` 就一次拿到 13 条：

```
│ sake  /seik/
│ 1 n.   目的, 缘故, 理由
│
│ phrases
│   for the sake of
│   for God's sake
│   art for art's sake
│   ...
```

抽取只认显式标记（`相关词组:` / `相关短语:` / `[词组]`），不做任何推导；
只有词组没有释义时，卡片就只显示词组本身，不画空的箭头。

### 自备表格

```bash
quietly phrases phrases.tsv
```

```tsv
lemma	phrase	gloss
sake	for the sake of	为了；为了……的缘故
sake	for its own sake	为了本身
bank	bank on	指望，依赖
```

- 首行可以是表头（含 `lemma` / `phrase` 字样），也可以直接写数据
- 逗号分隔同样可用，分隔符是探测出来的
- 同一词的重复 `phrase` 会被忽略，可反复导入

单条补录用 `quietly phrase` 或 `/phrase`，语序是「词 空格 搭配 = 释义」：

```bash
quietly phrase "sake for the sake of = 为了"
```
```
/phrase sake for the sake of = 为了
```

卡片上的呈现：

```
│ sake  /seik/
│ 1 n.   目的, 缘故, 理由
│
│ phrases
│   for the sake of   ->  为了；为了……的缘故
│   for its own sake  ->  为了本身
```

### 多义词与生僻义

`translation` 字段里的分行被解析成结构化词义：

| 原文 | 呈现 |
| --- | --- |
| `n. 银行, 堤, 岸` | `1 n.   银行, 堤, 岸`（常用义，编号） |
| `[医] 库` | 归入 `rare / technical` 分组，带上领域标签 |
| `[时态] banked, banking` | 单独一行的词形变化，不算词义 |
| `-K1`、`(中高雅 663/406)` | 丢弃，星级改渲染成 `★☆☆☆☆` |

带任何方括号标签（`[医]` `[古]` `[口]` `[网络]` …）的释义都归入次要分组，
这样「生僻义」和日常用法不会混在一起。

### 英文释义补充（Wiktionary）

ECDICT 的中文释义并不完整：`sake` 只有「目的, 缘故, 理由」，没有「清酒」。
`quietly etymology` 在导入词根时会**顺带**收集 Wiktionary 的英文释义，
存在 `glosses` 字段。

这部分**默认折叠**，卡片上只留一行提示：

```
│ sake  /seik/
│ 1 n.   目的, 缘故, 理由
│
│ more meanings · 5 · m
│
│ 1 again · 2 hard · 3 good · 4 easy · m more
```

按 `m` 展开当前这张卡（旧卡片保持各自的样子，不会被追溯改写）：

```
│ sake  /seik/
│ 1 n.   目的, 缘故, 理由
│
│ more meanings
│   • Cause, interest or account.
│   • Purpose or end; reason.
│   • Rice wine; a drink made from filtered fermented rice.
```

`quietly word <lemma>` 这种显式查询则直接显示全部，不用按 `m`。

几个细节：

- **只收有语体标记或真正不同的义项**，每词最多 6 条
- **保留语体标签**（`archaic` / `obsolete` / `slang` / `rare` …），显示为 `[archaic]`
- **跳过 `present participle of run` 这类变形说明**，它们不是词义
- **别名继承**：`sake` 在 Wiktionary 里只是 “Alternative spelling of saké”，
  真正的义项挂在 `saké` 上，导入时会自动把目标条目的释义复制过来

## 导入行为

- 同一拼写（忽略大小写）视为同一个 `word`，重复导入只更新空缺字段（音标、释义、词根、例句、标签）
- 词书按 `slug` 去重，`book_words.rank` 递增，新词按 `rank` 顺序出现
- 已存在的学习进度（`cards`）完全不动，可以放心重复导入
- 导入结果会显示 `added / updated / skipped`
## 词根与词源

**词根不由程序推测。** 早期版本用内置词缀表做最长匹配，会把 `surgeon` 拆出 `geo`、
把 `disapproval` 拆成 `dis- + -al`，属于编造，已删除。现在只接受有出处的数据：

```bash
quietly etymology <kaikki.jsonl>   # Wiktionary 词源模板（CC BY-SA）
quietly etymology <roots.tsv>      # 自备词根表
```

### kaikki.org / Wiktionary JSONL

[kaikki.org](https://kaikki.org/dictionary/English/) 把 Wiktionary 的词源模板抽成 JSONL，
每条词目形如：

```json
{"word": "unhappy",
 "etymology_templates": [
   {"name": "prefix", "args": {"1": "en", "2": "un", "3": "happy"}}
 ]}
```

导入器只认这四种模板，因为它们显式列出了构词成分：

| 模板 | 含义 | 分词结果 |
| --- | --- | --- |
| `prefix` | 前缀 + 词干 | 第 2 段标 `prefix`，其余 `root` |
| `suffix` | 词干 + 后缀 | 最后一段标 `suffix`，其余 `root` |
| `confix` | 前后缀同时 | 首段 `prefix`、末段 `suffix`、中间 `root` |
| `compound` | 复合词 | 全部 `root` |

`inh`（继承）、`der`（派生）、`bor`（借词）、`af`（合并）等一律**跳过**，不做猜测。
没有可用模板的词不写入，卡片显示 `no etymology on file`。

每个词素都会记录 `source: "Wiktionary etymology"`，卡片上以 `source:` 一行展示。

### 自备 TSV

```tsv
lemma	part	gloss	kind	source
inspect	in-	inward	prefix	OED
inspect	spect	look	root	OED
```

- `kind` ∈ `prefix` / `root` / `suffix`
- `source` 可留空；建议填教材或词典名，方便日后核对
- 同一 `lemma` 的多行会合并成一个词的多个词素

### 行为

- 只更新**已存在**的词条，词源文件不会新建单词
- 重复导入会覆盖该词的 `roots` 字段（以最后一次为准）
- 模型字段：`WordRoot { part, gloss, kind, source }`
## 数据位置

| 平台 | 路径 |
| --- | --- |
| Windows | `%APPDATA%\quietly-english\quietly.db` |
| macOS | `~/Library/Application Support/quietly-english/quietly.db` |
| Linux | `~/.local/share/quietly-english/quietly.db` |

用 `quietly where` 查看实际路径，`--db <path>` 或 `QUIETLY_HOME` 覆盖。
