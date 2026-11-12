# 阅读模块

两种模式：**全篇阅读**（连续读完整篇）和**精读**（一段一段，带译文和生词表）。
生词由读者自己标记，程序不判断哪个词难。

## 快速开始

```bash
quietly read article.txt              # 导入一篇（可双语）
quietly read ch1.txt --book "Animal Farm"   # 归入一本书
quietly read                          # 列出阅读书与散篇进度
quietly library                       # 只看书与进度
quietly quiz "Why I Write" q.tsv      # 给某篇导入题目
```

进入 TUI：

```
/read article.txt        导入并开始阅读
/read                    续读上次那篇
/read Why I Write        按标题打开
/read animal-farm        接着读这本书里下一篇没读完的
/library                 列出书与每本进度
/texts                   列出所有文本与进度
```

## 阅读书：同源归一本

零散地一篇一篇读，进度记不住；同一个来源的东西本来就该是一本书。规则很简单：

| 来源 | 归到哪本书 |
|---|---|
| 文件头 `# book: Animal Farm` | `animal-farm` |
| `quietly read x.txt --book "Animal Farm"` | 同上，批量导入时不用改文件头 |
| 只有文件头 `# source: Project Gutenberg` | 用来源作书名（`project-gutenberg`） |
| 两者都没有 | 散篇，不硬凑一本书 |

书内顺序按导入先后排，`/next` 按它推进：

```
/read animal-farm        → 打开这本书里第一篇还没读完的
读到末段再按 space/enter  → 自动接上一篇（书里还有的话）
读完最后一篇              → "book finished · 3/3"
```

`/library` 的显示：

```
⏺ library  2 books · 9 texts · 1 on their own
⎿ Animal Farm                  1/2 · next Chapter Two
  Why I Write ·                2/2 · done Why I Write
  1 texts without a book · /texts lists them
  /read <book> continues its next text · /read <file> imports
```

`·` 标在「上次读的那篇所在的书」上。命令行对应 `quietly library`。

一篇文章只能属于一本书，但它可以读完后随时重读（`/read <title>` 不会清进度）。

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
| `/read <title|file|book>` | 打开一篇、导入文件，或接着读一本书的下一篇 |
| `/texts` | 列出已导入的文本与进度 |
| `/library` | 阅读书与每本进度（别名 `/shelf`） |
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

文件头的 `# title:` / `# source:` / `# license:` / `# book:` 会被读进元数据。`license`
尤其重要——导入有版权的文本时自己要清楚来源。`# book:` 决定它归到哪本书；没写时用
`# source:` 的函数：

```
# title: Chapter One
# book: Animal Farm
# source: Project Gutenberg
# license: public domain
```

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

| 来源 | 授权 | 有中文对照 | 适合当什么 |
| --- | --- | --- | --- |
| 自己写的笔记 / 已购电子书 | 你自己的 | 视情况 | 任意 |
| [Project Gutenberg](https://www.gutenberg.org/) | 公有领域 | ❌ 纯英文 | 长篇原著；**一章一个文件**，`# book:` 用书名 |
| [Standard Ebooks](https://standardebooks.org/) | 公有领域（重排版） | ❌ | 同上，排版干净、分章清楚 |
| [Wikisource 多语言版](https://zh.wikisource.org/) | CC BY-SA / 公有领域 | ✅ 部分作品有旧译 | 中英对照长篇：原文取 Gutenberg，旧译取这里 |
| [Tatoeba 句对](https://tatoeba.org/en/downloads) | CC-BY 2.0 FR | ✅ 句子级 | 短句精读素材，不是成篇 |
| [TED 演讲字幕](https://www.ted.com/participate/translate) | CC BY-NC-ND | ✅ 逐句对照 | 演讲/口语，一个演讲一篇 |
| [VOA Learning English](https://learningenglish.voanews.com/) | 美国政府作品 | ❌ | 分级新闻，按栏目归书（`# book: VOA Science`） |
| [Simple English Wikipedia](https://simple.wikipedia.org/) | CC BY-SA | ❌ | 主题短文，按主题归书 |
| [LibriVox 文本对照](https://librivox.org/) | 公有领域 | ❌ | 只用来找书，正文还是 Gutenberg |
| [The Guardian / BBC 等](https://www.theguardian.com/) | 版权归媒体 | ❌ | **不推荐**：正文有版权，且无译文 |

**推荐路径**：

1. 用 Gutenberg / Standard Ebooks 拿原文，**一章一个 `.txt`**，统一写 `# book: 书名`；
2. 需要译文时找公有领域旧译（民国时期译本多已进入公有领域，但各家出版社的
   **新译**有版权，注意区分），按 `---` 分段粘进同一个文件；
3. `quietly read ch01.txt --book "书名"` 批量导入，`/library` 看进度，`/read <slug>` 按书推进。

`packs/texts/` 里已经放了一篇带 `# source:` 的示例（`orwell.txt`），可以照它的头部格式写。

> 不要抓扇贝、百词斩、新闻网站正文——和词书部分的结论一致。

## 数据表

```sql
text_books(id, slug UNIQUE, name, source, created_at)   -- 阅读书
texts(id, title UNIQUE, source, license, book_slug, rank, body, translation, created_at)
reading_progress(text_id PK, paragraph, mode, updated_at)
reading_log(id, text_id, day, started_at, ended_at, seconds,
            paragraphs, words_opened, words_added)
reading_marks(text_id, lemma, paragraph, created_at)   -- 读者标记的词
quizzes(id, text_id, paragraph, prompt, answer, choices, source, created_at)
quiz_attempts(id, quiz_id, day, at, correct)
```

- 段落不单独建表：`body` 存原文，切段在读取时做，所以重新导入同一篇不会打乱进度
- 同一标题重复导入只更新正文与译文，**保留进度和标记**；顺带换书时才改 `book_slug`
- `book_slug` 直接存 slug（`qe_core::reading::book_slug` 由书名生成，保留中文），
  书内顺序由 `rank` 决定
- 老库升级（v7）时按 `source` 自动回填成书，之前读的散篇不需要重新导入
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
