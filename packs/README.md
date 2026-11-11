# 数据包

词库、文本、题目这些**输入数据**放在这里，和代码同一个仓库管理。
原则很简单：

- **小、自己产出的 → 提交进 git**（词书、阅读文本、题目）
- **大、能重新下载的 → 不提交**（词典、词源、例句语料）

```bash
quietly db     # 看运行时数据库（不是这里的数据源）的位置和体积
```

## 目录

| 目录 | 内容 | 进 git | 典型体积 |
| --- | --- | --- | --- |
| `decks/` | 词书（qwerty-learner JSON、通用 CSV、纯词表 TXT） | ✅ | KB |
| `texts/` | 阅读文本（可双语） | ✅ | KB |
| `quizzes/` | 阅读理解题 TSV | ✅ | KB |
| `dict/` | StarDict 词典包 | ❌ | 3–40 MB |
| `etymology/` | kaikki 词源 JSONL | ❌ | 15 MB – 3.2 GB |
| `examples/` | Tatoeba 句对与 links | ❌ | 170 MB |

## 导入

```bash
quietly import packs/decks/CET6_T.json          # 词书
quietly dict   packs/dict/ecdict.ifo            # 词典（第一次）
quietly dict   packs/dict/langdao-ec-gb.ifo --merge   # 叠第二本
quietly etymology packs/etymology/kaikki.jsonl  # 词根
quietly examples packs/examples/*.tsv packs/examples/links.csv
quietly read   packs/texts/article.txt          # 阅读文本
quietly quiz   "Article Title" packs/quizzes/q.tsv
```

## 为什么不把词典也提交

`quietly-words.db` 有 500 MB+，直接推 Git 会超出 GitHub 的单文件与仓库限制，
而且这些内容随时能用下载包重建：

```bash
quietly dict packs/dict/ecdict.ifo        # 约 11 分钟，327 万词条
quietly dict packs/dict/langdao.ifo --merge
```

另外多数商业词典的电子化版本**版权状态不明**，随仓库分发不合适。

## 换机器怎么恢复

```bash
git clone <repo> && cd quietly-english
quietly import packs/decks/CET6_T.json      # 词书，仓库自带
quietly dict   <下载的 ecdict.ifo>          # 词典，自己下载
quietly dict   <下载的 langdao.ifo> --merge
quietly etymology <下载的 kaikki.jsonl>
quietly examples <下载的 tatoeba 文件>
```

学习进度与阅读进度存在 `quietly.db`（几 MB）。要跨机器同步，把这个文件复制过去，
或者放进网盘再把 `QUIETLY_HOME` 指过去：

```bash
export QUIETLY_HOME=/path/to/quietly-data
```
