# 数据来源与导入记录

词库分四层，各自独立、可以单独更新：

| 层 | 命令 | 作用 |
| --- | --- | --- |
| 词书 | `quietly import <deck>.json` | 考试大纲词汇，决定"学什么" |
| 词典 | `quietly dict <name>.ifo` | 查词底库，决定"查得到什么" |
| 词源 | `quietly etymology <kaikki>.jsonl` | 词根分解 + 出处 |
| 例句 | `quietly examples <cmn> <eng> <links>` | 中英对照例句 |

导入结果落在 `quietly where` 给出的数据库里（默认 `%APPDATA%\quietly-english\quietly.db`），
数据源文件本身可以随时删除。

## 已验证的导入结果

以下是在本机实际跑通的结果，可作为参考基线。

```
cet4   CET-4 核心词    2608 词
cet6   CET-6 核心词    2347 词
toefl  TOEFL 核心词    4267 词
ky     考研核心词      3727 词
ielts  IELTS 核心词    3575 词
gre    GRE 核心词      6514 词
```

| 导入 | 结果 | 耗时 |
| --- | --- | --- |
| 六本 qwerty-learner 词书 | 共 23038 词条 | 秒级 |
| ECDICT StarDict（327 万条） | `added 3264734 · updated 11032` | 约 11 分钟 |
| Wiktionary 词源（150 万行 JSONL） | `215004 words enriched · 429515 morphemes` | 约 4 分钟 |
| Tatoeba 中英例句 | `12585 examples · 4709 words covered` | 约 3.5 分钟 |

数据库最终约 440 MB。词书里 11037 个词中：4124 个有词根，4715 个有例句。

---

## 1. 词书：qwerty-learner（MIT）

- 目录页：<https://github.com/RealKai42/qwerty-learner/tree/master/public/dicts>
- 直链模板：`https://raw.githubusercontent.com/RealKai42/qwerty-learner/master/public/dicts/CET6_T.json`

常用文件：`CET4_T` `CET6_T` `KaoYan_3_T` `TOEFL_3_T` `IELTS_3_T` `GRE_3_T`，
另有专四专八（`Level4luan_*`、`Level8_*`）、`GMAT_3_T`、`SAT_*`。

```bash
quietly import CET6_T.json
```

导入器认识 `{"name": ..., "trans": [...], "usphone": ...}` 这种原生结构，
并**按文件名自动归入对应词书**（`CET6_T` → `cet6`），不会多出一本 `cet6-t`：

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

## 2. 词典：ECDICT（MIT）

- 发布页：<https://github.com/skywind3000/ECDICT/releases>
- StarDict 包（70 MB）：`ecdict-stardict-28.zip` → 解压得到 `.ifo` `.idx` `.dict`
- CSV 包：`ecdict-csv.7z` → 解压得到 `stardict.csv`（走 `quietly import`）

```bash
unzip ecdict-stardict-28.zip
quietly dict stardict-ecdict-2.4.2/stardict-ecdict-2.4.2.ifo
```

StarDict 是二进制格式：`.idx` 存"词条 + 偏移 + 长度"，`.dict` 是数据区。
导入器顺序读取索引并顺序取数据，327 万条约 11 分钟。
条目内容形如 `*[音标]\n词性. 释义`，音标行会被识别并单独存字段。

> ECDICT **没有词根、没有例句**，这两层分开导入。

## 3. 词源：kaikki.org（Wiktionary，CC BY-SA）

这是唯一带学术出处的词根来源：Wiktionary 编辑者依据词源词典逐条填写构词模板，
kaikki 把它们抽成 JSON。

- 入口：<https://kaikki.org/dictionary/English/>
- 全量（3.4 GB）：`https://kaikki.org/dictionary/English/kaikki.org-dictionary-English.jsonl`
- 按词性切分（体积小得多）：
  - 副词 85 MB：`.../pos-adv/kaikki.org-dictionary-English-by-pos-adv.jsonl`
  - 动词 450 MB：`.../pos-verb/kaikki.org-dictionary-English-by-pos-verb.jsonl`
  - 形容词 507 MB：`.../pos-adj/kaikki.org-dictionary-English-by-pos-adj.jsonl`
  - 名词 1.9 GB：`.../pos-noun/kaikki.org-dictionary-English-by-pos-noun.jsonl`
- 词缀释义（共 15 MB，装了才有 `un- (prefix, not)` 里的 `not`）：
  - 前缀 8.2 MB：`.../pos-prefix/kaikki.org-dictionary-English-by-pos-prefix.jsonl`
  - 后缀 7.2 MB：`.../pos-suffix/kaikki.org-dictionary-English-by-pos-suffix.jsonl`

```bash
quietly etymology kaikki.org-dictionary-English.jsonl
```

### 只认显式的构词模板

| 模板 | 含义 | 分词结果 |
| --- | --- | --- |
| `prefix` / `pre` | 前缀 + 词干 | 首段 `prefix`，其余 `root` |
| `suffix` / `suf` | 词干 + 后缀 | 末段 `suffix`，其余 `root` |
| `confix` / `circumfix` | 前后缀同时 | 三段时首 `prefix`、末 `suffix`；两段时首 `prefix`、次 `root` |
| `compound` / `com` | 复合词 | 全部 `root` |
| `af` | 通用构词模板 | 用编辑者写的连字符判断类型（`un-` 前缀、`-able` 后缀） |
| `etymon` / `ety` | 包装模板 | 类型在参数 2（`:af`、`:prefix`…） |

`inh`（继承）、`der`（派生）、`bor`（借词）、`cog`（同源词）、`root`（原始印欧词根注记）
等一律**跳过**——它们描述的是历史来源，不是这个词自身的构词，转成词素拆分就是编造。

每个词素都记录 `source`，卡片上以 `source: Wiktionary {{af}}` 展示。
没有可用模板的词不写入，卡片显示 `no etymology on file`。

### 自备 TSV

```tsv
lemma	part	gloss	kind	source
inspect	in-	inward	prefix	OED
inspect	spect	look	root	OED
```

- `kind` ∈ `prefix` / `root` / `suffix`
- `source` 可留空，建议填教材或词典名
- 同一 `lemma` 的多行会合并

## 4. 例句：Tatoeba（CC-BY 2.0 FR）

- 下载页：<https://tatoeba.org/en/downloads>
- 中文句子（1.2 MB）：`https://downloads.tatoeba.org/exports/per_language/cmn/cmn_sentences.tsv.bz2`
- 英文句子（24 MB）：`https://downloads.tatoeba.org/exports/per_language/eng/eng_sentences.tsv.bz2`
- 句对连接表（144 MB）：`https://downloads.tatoeba.org/exports/links.tar.bz2`

```bash
bunzip2 -k cmn_sentences.tsv.bz2 eng_sentences.tsv.bz2
tar -xjf links.tar.bz2
quietly examples cmn_sentences.tsv eng_sentences.tsv links/links.csv
```

导入器分四遍流式处理，不会把 200 万条英文句子全塞进内存：

1. 读中文句子（约 9 万条，可直接放内存）
2. 扫 links，记下被中文句子引用到的英文 id
3. 读英文句子，只保留上一步要的那些
4. 再扫 links 生成配对，只为**词书里已有的词**收例句，每词最多 3 条

## 关于扇贝 / 百词斩词书

扇贝、百词斩等产品的词书数据没有公开授权，**不要抓取或导出**。
用上面 qwerty-learner 的同级别词表或 ECDICT 的 `tag` 即可获得等价覆盖。

## 数据位置

| 平台 | 路径 |
| --- | --- |
| Windows | `%APPDATA%\quietly-english\quietly.db` |
| macOS | `~/Library/Application Support/quietly-english/quietly.db` |
| Linux | `~/.local/share/quietly-english/quietly.db` |

用 `quietly where` 查看实际路径，`--db <path>` 或 `QUIETLY_HOME` 覆盖。
