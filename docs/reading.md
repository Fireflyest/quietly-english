# 阅读模块

两种模式：**全篇阅读**（连续读完整篇）和**精读**（一段一段，带译文和生词表）。
生词由读者自己标记，程序不判断哪个词难。

## 快速开始

```bash
quietly read article.txt          # 导入一篇（可双语）
quietly read                      # 列出已导入的
quietly quiz "Why I Write" q.tsv  # 给某篇导入题目
```

进入 TUI：

```
/read article.txt        导入并开始阅读
/read                    续读上次那篇
/read Why I Write        按标题打开
/texts                   列出所有文本与进度
```

## 两种模式

```bash
/mode                # 在全篇 / 精读之间切换
/mode full           # 连续读完整篇
/mode intensive      # 一次一段，带译文与生词表
```

**全篇**（`full`）：

```
⏺ read  full · 50
⎿ Why I Write
  From a very early age, perhaps the age of five or six, I knew
  that when I grew up I should be a writer.

  I had the lonely child's habit of making up stories and holding
  conversations with imaginary persons.
```

**精读**（`intensive`，译文默认隐藏）：

```
⏺ read  intensive · 1/2
⎿ Why I Write
  From a very early age I knew I should be a writer.

  translation hidden · tab to show

  words · 2 marked · /mark <word> to mark any
    1 writer  n. 作家 ●
    2 lonely  a. 孤独的
    3 habit   n. 习惯 ●
```

按 `tab` 后同一张卡片变成：

```
  translation
    从很小的时候，我就知道我要当作家。
```

`●` 表示已标记。标记的词在原文里用强调色显示。

## 选词：不用另开卡片

在段落里选一个词，说明就贴在段落下面，正文和译文不动，历史也不会被顶走：

```
⏺ read  intensive · 1/2
⎿ Why I Write
  From a very early age I knew I should be a writer.
  ↑ 反显的就是当前选中的词

  translation hidden · tab to show

  word
    early  /ˈɜːli/                        early age → 早年
    a.  早的，早期的
  enter full card · /mark marks it · esc clears the selection
```

词组直接写在释义右侧，**不加标题、不画表格**：第一行就和词头对齐，箭头后面是译文。
终端窄到放不下两栏时，词组折到释义下方，形式不变。

**只有本段原文里真的出现的词组才会显示**：词条里登记了 `early bird`，但这段没用到，
就不会拿它来打扰。判定按整词、大小写不敏感，词组可以只在中段覆盖选中的词
（选中 `sake` 也会命中本段的 `for the sake of`）。最多 6 条，其余在 `enter` 的完整卡片里。

三种选中方式：

| 方式 | 说明 |
|---|---|
| `,` / `.` | 按词前后移动（1 个字母的词不算，与查词口径一致） |
| 鼠标点击 | 直接点段落里的词；点空白不选中。全篇模式下点到**别的段**会先跳到那一段再选中，不会选错词 |
| `1`-`9` | 打开当前段第 n 个词的完整卡片 |

选中之后：

- `enter` 打开这个词的完整卡片（词根、例句、搭配）；
- `/mark` 标记它，`/take` 收进词书；
- `esc` 先取消选中，再按一次才结束阅读。

说明块只放音标 + 前三条常见义项。词根、例句、英文释义仍留在完整卡片里，
免得一块说明把正文顶出屏幕。词典里没有的词也会显示一行 `(no definition in the dictionary)`，
不会出现「点了没反应」。

**说明块就在当前段落下方**，不是整篇末尾：全篇模式把整篇画成一条消息，
说明块如果挂在末尾，长文里要滚到底才看得见。翻页或选词后，当前段若在视野外，
视图会自动把它带回视野顶部（已经在视野内则不动，不会把读者拽走）。

## 按键

| 键 | 行为 |
| --- | --- |
| `space` / `→` | 下一段 |
| `enter` | 有选中词 → 打开完整卡片；没有 → 下一段 |
| `←` | 上一段 |
| `,` / `.` | 选中上一个 / 下一个词 |
| 鼠标点击 | 选中点到的词 |
| `1`-`9` | 打开当前段第 n 个词的完整卡片 |
| `tab` | 显示 / 隐藏译文（**默认隐藏**，先读原文） |
| `esc` | 有选中词 → 取消选中；没有 → 结束阅读（写进度与统计） |

字母键不拦截，所以想输入单词查词、或者敲 `/命令` 都不会被吞掉。

**全篇模式**也支持 `next` / `prev` / `goto`：当前段前面会出现一行暗色 `▸ 12/50`，
badge 也显示段号，所以定位结果看得见。译文同样受 `tab` 控制，显示时跟在各自段落后面。

## 命令

| 命令 | 说明 |
| --- | --- |
| `/read <title|file>` | 打开一篇；不给参数续读上次那篇 |
| `/texts` | 列出已导入的文本与进度 |
| `/drop <title>` | 删除一篇及其进度、标记、题目（不可撤销，需精确标题） |
| `/mode` | 在全篇 / 精读之间切换 |
| `/translate` | 显示 / 隐藏译文（`tab` 同效，默认隐藏，两种模式都生效） |
| `/mark <word>` | 标记 / 取消标记一个词 |
| `/mark` | 标记选中的词（没有选中时标当前段第一个未标记的词） |
| `/mark all` | 标记当前段**全部**词（不受数字键 9 个上限影响） |
| `/take [n]` | 把第 n 个词收进词书；不给 n 就收取选中的词 |
| `/goto <n>` | 跳到第 n 段 |
| `/next` · `/prev` | 翻页 |
| `/words` | 列出当前段所有不重复的词 |
| `/quiz [file]` | 做题；给文件则先导入题目 |

## 文本格式

**单语**：空行分段。

```
# title: Why I Write
# source: George Orwell, 1946
# license: public domain

First paragraph...

Second paragraph...
```

**双语**：`---` 独占一行分隔原文与译文，两半的段落数按序对齐。

```
From a very early age I knew I should be a writer.

I had the lonely child's habit.

---

从很小的时候，我就知道我要当作家。

我有个孤独孩子的习惯。
```

译文可以只写前几段，没译的段落不显示 `translation` 区块。

文件头的 `# title:` / `# source:` / `# license:` 会被读进元数据。`license` 尤其重要——
导入有版权的文本时自己要清楚来源。

## 题目格式

TSV 五列，`#` 开头是注释：

```
段落号<TAB>题干<TAB>答案<TAB>出处<TAB>选项1|选项2|选项3
```

```tsv
1	What did Orwell know from an early age?	should be a writer	Orwell, Why I Write, para 1	should be a writer|should be a doctor|should be a soldier
2	What habit did he have as a lonely child?	making up stories	Orwell, Why I Write, para 2	making up stories|playing football|painting
```

- 段落号从 1 开始
- 选项用 `|` 分隔；**少于两个选项就是填空题**：按 `enter` 揭示答案，不判对错，也不写 `quiz_attempts`
- **出处列必填**：没有出处的题目会被跳过。凭空生成的题目无法核对对错，宁可不收。

作答时按 `1`-`9` 选选项，`enter` 提交并翻下一题，`esc` 退出。每轮从零分起算，
重复做同一篇不会累加。

## 数据从哪来

**中文对照是这个模块的硬约束**：程序不会翻译，译文必须随文本一起提供。

| 来源 | 授权 | 有中文对照 |
| --- | --- | --- |
| 自己写的笔记 / 已购电子书 | 你自己的 | 视情况 |
| [Project Gutenberg](https://www.gutenberg.org/) | 公有领域 | ❌ 纯英文 |
| [Wikisource 多语言版](https://zh.wikisource.org/) | CC BY-SA / 公有领域 | ✅ 部分作品有旧译 |
| [Tatoeba 句对](https://tatoeba.org/en/downloads) | CC-BY 2.0 FR | ✅ 句子级，非成篇 |
| [TED 演讲字幕](https://www.ted.com/participate/translate) | CC BY-NC-ND | ✅ 逐句对照 |
| [VOA Learning English](https://learningenglish.voanews.com/) | 美国政府作品 | ❌ 但用词分级，适合精读 |
| [Simple English Wikipedia](https://simple.wikipedia.org/) | CC BY-SA | ❌ 同上 |

**推荐路径**：用 Gutenberg 拿原文，再找公有领域旧译（民国时期译本多已进入公有领域，
但各家出版社的**新译**有版权，注意区分）。或者直接用 Tatoeba 的句对做短篇精读素材。

> 不要抓扇贝、百词斩、新闻网站正文——和词书部分的结论一致。

## 数据表

```sql
texts(id, title UNIQUE, source, license, body, translation, created_at)
reading_progress(text_id PK, paragraph, mode, updated_at)
reading_log(id, text_id, day, started_at, ended_at, seconds,
            paragraphs, words_opened, words_added)
reading_marks(text_id, lemma, paragraph, created_at)   -- 读者标记的词
quizzes(id, text_id, paragraph, prompt, answer, choices, source, created_at)
quiz_attempts(id, quiz_id, day, at, correct)
```

- 段落不单独建表：`body` 存原文，切段在读取时做，所以重新导入同一篇不会打乱进度
- 同一标题重复导入只更新正文与译文，**保留进度和标记**
- 标记按 `(text_id, lemma)` 去重，跨会话保留

## 统计

`quietly stats` 与 `/summary` 会多一行：

```
reading: 12 paragraphs · 8 min · 5 words opened · 2 added
quiz: 3/4 correct
```

## 分层

| 层 | 文件 | 职责 |
| --- | --- | --- |
| `qe-core` | `reading.rs` | 切段、分词、高亮片段、会话状态；纯函数可测 |
| `qe-core` | `view.rs` | `reading_card` / `reading_summary` / `quiz_card` |
| `qe-store` | `db.rs` | v4 迁移、文本与进度的读写、测验 |
| `qe-app` | `reader.rs` | 导入文件、翻页、标记、收词、测验交互 |
| `qe-tui` | `lines.rs` | `Marked`（逐词高亮）与 `Pick`（候选词行） |

`qe-core::reading` 里没有数据库依赖，`highlight_segments` 的判定完全由调用方传入的闭包决定，
所以想改成「按星级自动高亮」也只是换一个闭包。
