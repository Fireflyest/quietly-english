# 阅读模块

两种模式：**全篇阅读**（连续读完整篇）和**精读**（一段一段，带译文和生词表）。
生词由读者自己标记，程序不判断哪个词难。

## 快速开始

```bash
quietly read book.epub                # 直接导入 EPUB，一章一篇，自动归成一本
quietly read article.txt              # 导入纯文本（可双语）
quietly read ch1.txt --book "Animal Farm"   # 归入指定的书
quietly read                          # 列出阅读书与散篇进度
quietly library                       # 只看书与进度
quietly news                          # 从 TED / BBC / 新华网 / Guardian 长读 / Aeon 各抓一批
quietly quiz "Why I Write" q.tsv      # 给某篇导入题目
```

进入 TUI：

```
/read book.epub          导入 EPUB 并开始读第一章
/read article.txt        导入纯文本并开始阅读
/read                    续读上次那篇
/read Why I Write        按标题打开
/read animal-farm        接着读这本书里下一篇没读完的
/library                 列出书与每本进度
/fetch（/news 同义）      立刻从所有来源抓一批
/texts                   列出所有文本与进度
```

## 每天自动抓阅读材料

**每天第一次启动时**，程序会在后台从五个来源各抓一批，每个来源归成自己的一本书：

| 来源 | 书（slug） | 每次 | 译文 | 直连 | 长度 |
|---|---|---|---|---|---|
| TED Talks | `ted-talks` | 3 篇 | ✅ 官方中文字幕，逐段对照 | ✅ | ~1,300 词 |
| BBC 媒体英语 | `bbc-media-english` | 3 期 | ✅ 英段紧跟中段 | ⚠️ 国内需代理 | ~110 词 |
| 新华网英文 | `xinhua-english` | 3 篇 | ❌ 只有英文 | ✅ | ~800 词 |
| Guardian 长读 | `guardian-long-read` | 1 篇 | ❌ 只有英文 | ⚠️ 国内需代理 | **~4,000 词** |
| Aeon 随笔 | `aeon-essays` | 1 篇 | ❌ 只有英文 | ✅ | **~3,000–6,000 词** |

- 抓取在后台线程里跑（自己开一条 SQLite 连接），界面不冻结；
- 完成后在对话里列出每个来源抓了多少：`TED Talks 3 new · en + 中文`，失败只提示一次；
- 抓过就把日期记进 `settings.news_fetched_on`，同一天不会再抓；
- `/fetch` 可以随时手动再抓一次，不受「今天抓过」限制；
- 抓到的文章只进本地库，**不写仓库**；
- **默认直连**：只有环境里配了 `HTTPS_PROXY`/`HTTP_PROXY` 才走代理。**BBC 和 Guardian
  长读在国内直连不通**（实测 curl 直连 20 秒超时），没配代理时这两行会显示
  `unavailable`，其余来源照常抓；配了代理它们就自动可用。
- 一个来源挂了不影响别人：TED 打不开也得把新华网抓回来。

> 为什么不是中国日报？「英语点津」的双语新闻栏目 2024-09-05 之后就没再更新（列表页
> 最新一条就是那天），21 世纪英文报的双语栏目停在 2024-03，所以在国内**还在更新、
> 又带中文译文、又能直连**的源基本没有了。于是拆成三类：TED 给带官方译文的精读，
> 新华网英文给每日新鲜的英文原文，Guardian 长读和 Aeon 给**真正够长的**文章
> （3,000–6,000 词，没有译文，靠词条和上下文读）。
>
> 双语长篇这条路实测走不通：联合国教科文组织《信使》杂志英文版和中文版是两个
> 不同的 slug，取中文首页 8 篇只有 1 篇段落数对得上；联合国新闻的中英文是不同的
> node id，页面里没有互指链接；FT 中文网限流（429），纽约时报中文网要登录。

每条想多抓几篇：改各来源模块里的 `limit`（如 `crates/qe-app/src/ted.rs`）。配对不上的段落会被
丢掉而不是错位——宁可少一段，也不让译文串段。

### 加一个新来源

在 `crates/qe-app/src/` 里加一个模块，实现两个函数，然后登记到 `news.rs` 的 `SOURCES`：

```rust
pub const SOURCE: Source = Source {
    name: "某站",
    book: "某站",          // 归到哪本书
    translated: true,      // 有没有译文
    limit: 3,              // 每次最多抓几篇新的
    listing,               // fn() -> Result<Vec<String>>，候选 URL，最新的在前
    article,               // fn(&str) -> Result<Option<Article>>，解析不出就 Ok(None)
};
```

去重、限量、写库、失败隔离都由 `news.rs` 统一处理。

## 阅读书：同源归一本

零散地一篇一篇读，进度记不住；同一个来源的东西本来就该是一本书。规则很简单：

| 来源 | 归到哪本书 |
|---|---|
| EPUB 文件 | 用 `dc:title` 当书名，spine 里每一章自动成为一篇 |
| 自动抓取的新闻 | 每个来源一本书：`TED Talks`（`ted-talks`）、`BBC Media English`（`bbc-media-english`）、`Xinhua English`（`xinhua-english`）、`Guardian Long Read`（`guardian-long-read`）、`Aeon Essays`（`aeon-essays`）；旧的存档在 `China Daily Bilingual` |
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

`/library` 的显示（左边是可直接输入的书名 slug）：

```
⏺ library  2 books · 31 texts
⎿ barlasch-of-the-guard   Barlasch of the Guard · 0/30 · next Barlasch of the Guard — 1
  george-orwell-why-i-write-1946   George Orwell, "Why I Write", 1946 · 1/1 · done
  /read <slug> continues it · /read <file> imports txt/epub
```

`/read` 的补全只列**书**，不会把三十章标题摊满屏幕；输入 `/read bar` 按 tab 就是
`barlasch-of-the-guard`，回车从下一篇没读完的开始。`/texts` 同样按书汇总，散篇才逐条列出。

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

  words · 2 marked · /mark <word> to mark any
    1 writer  n. 作家 ●
    2 lonely  a. 孤独的
    3 habit   n. 习惯 ●
```

按 `tab` 后同一张卡片变成：

```
  From a very early age I knew I should be a writer.
    从很小的时候，我就知道我要当作家。
```

译文直接跟在段落下面，没有标题行，也没有「隐藏」提示。

按 `tab` 或 `/translate` 请求机翻时，当前段会显示 `translation: translating...`；
完成后会标明缓存/使用的后端，失败时保留错误和重试提示。官方译文会显示为
`translation: available`。阅读中的文章、当前段落和阅读模式会即时保存，点击全篇模式
里的其他段落也会立即记住新位置，重新执行 `/read` 时从上次位置继续。

`●` 表示已标记。标记的词在原文里用强调色显示。

## 复习统计

`/summary` 除了今日新词、复习次数、评分分布和连续学习天数，还显示 Again 占比
（遗忘率）以及当天最常重复出错的词，帮助调整学习量和定位困难词。

## 学习会话恢复

背词会话会自动保存当前卡片、答案/提示状态、剩余队列顺序和已完成数量。
程序意外关闭后重新执行 `/study` 会继续未完成的会话；如果要放弃旧会话并按指定数量
重新开始，使用 `/study <n>`。正常结束会话后，旧快照会自动清除。

## 选词：不用另开卡片

在段落里选一个词，说明就贴在段落下面，正文和译文不动，历史也不会被顶走：

```
⏺ read  intensive · 1/2
⎿ Why I Write
  From a very early age I knew I should be a writer.
  ↑ 反显的就是当前选中的词

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

当前段会用很淡的背景色标出；段落之间有一条细线进度条，按当前段的词数以每分钟
约 180 词估算时长，并随停留时间增长。进度达到预计时长后会变色提示，但不会自动
跳段。全篇模式下直接点击下一段的词，会先把那一段设为当前段，再选中该词。

## 按键

| 键 | 行为 |
| --- | --- |
| `space` / `→` | 下一段 |
| `enter` | 有选中词 → 打开完整卡片；没有 → 下一段 |
| `←` | 上一段 |
| `,` / `.` | 选中上一个 / 下一个词 |
| 鼠标点击 | 选中点到的词 |
| `1`-`9` | 打开当前段第 n 个词的完整卡片 |
| `tab` | 显示 / 隐藏译文（**默认隐藏**，先读原文；没有译文时按需机翻当前段） |
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
| `/fetch` · `/news` | 立刻从所有来源抓一批阅读材料（TED / BBC / 新华网 / Guardian 长读 / Aeon） |
| `/drop <title>` | 删除一篇及其进度、标记、题目（不可撤销，需精确标题） |
| `/mode` | 在全篇 / 精读之间切换 |
| `/translate` | 显示 / 隐藏译文（`tab` 同效，默认隐藏）；文本本身没有译文时按需机翻**当前段**，见下文 |
| `/mark <word>` | 标记 / 取消标记一个词 |
| `/mark` | 标记选中的词（没有选中时标当前段第一个未标记的词） |
| `/mark all` | 标记当前段**全部**词（不受数字键 9 个上限影响） |
| `/take [n]` | 把第 n 个词收进词书；不给 n 就收取选中的词 |
| `/goto <n>` | 跳到第 n 段 |
| `/next` · `/prev` | 翻页 |
| `/words` | 列出当前段所有不重复的词 |
| `/quiz [file]` | 做题；给文件则先导入题目 |

## 文本格式

**EPUB**：直接导入，不需要转换。

```powershell
quietly read "Pride and Prejudice.epub"
# imported 61 chapters from "Pride and Prejudice" · 35 paragraphs in the first chapter
# book: Pride and Prejudice (pride-and-prejudice)
# open it with /read pride-and-prejudice
```

只认 spine 顺序与 `epub:type="bodymatter"` 的正文，扉页/版权页/colophon 跳过；
斜体、脚注编号这类内联标记会被剥掉，段落边界保留。图片不导入。

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

译文可以只写前几段，没译的段落不显示译文块。

## 机翻：免费额度按段用

内建来源里只有 TED（官方字幕）和 BBC（双语栏目）带译文；新华网、Guardian 长读、
Aeon 都只有英文。按 `tab` 或 `/translate` 时，程序会把**当前这一段**送去翻译，
结果显示在原文下面，并写进库里——同一段只花一次额度。

| 服务 | 额度 | 要什么 | 直连 |
|---|---|---|---|
| MyMemory（默认） | 5000 字符/天 | 什么都不用 | ✅ |
| MyMemory + 邮箱 | 50000 字符/天 | 填 `MYMEMORY_EMAIL`，**不用注册** | ✅ |
| DeepL API Free | 50 万字符/月 | 要注册，设 `DEEPL_API_KEY`（[免费注册](https://www.deepl.com/pro-api)） | ✅ |

```bash
export MYMEMORY_EMAIL=you@example.com    # 额度翻十倍，不用注册、不用验证
export DEEPL_API_KEY=xxxxxxxx:fx         # 有 key 就优先用 DeepL，译文明显更顺
export QUIETLY_TRANSLATE=none            # 整个关掉，不联网
```

MyMemory 的邮箱只是请求里的 `de` 参数（[官方说明](https://mymemory.translated.net/doc/usagelimits.php)：
「填一个能联系到你的地址，额度从 5000 涨到 50000」），随便填一个真实邮箱即可，
**没有注册页、没有确认邮件、没有 API key**。额度按 IP 记，不是按邮箱记。

- **只翻当前段**，不整篇提交：一篇 Guardian 长读四五千词，一次性提交就把一个月的
  免费额度花光了；按段翻，额度花在你真正读的那几段上。
- 免费接口单次有上限（MyMemory 500 字节），长段自动按句号切开再拼回去。
- 译文缓存进 `text_translations`，换机器前不用重翻；`/drop` 删篇时一起清掉。
- 额度用完会明确说一句，不会静默留一个空白的译文区。
- 命令行也能翻：`quietly translate <标题> --para 3`（不给段落号就翻第一段还没译的）。

官方译文优先：TED / BBC 的对照译文不会被机翻覆盖。

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

**中文对照不是硬性前提**：文本自带译文时用自带的（TED / BBC / 你导入的双语文件），
没有就按 `tab` 走机翻，一段一段来（见上文「机翻」）。

| 来源 | 授权 | 有中文对照 | 适合当什么 |
| --- | --- | --- | --- |
| 自己写的笔记 / 已购电子书 | 你自己的 | 视情况 | 任意 |
| [Project Gutenberg](https://www.gutenberg.org/) | 公有领域 | ❌ 纯英文 | 长篇原著；下载 `.txt` 即可，一章一个文件时写 `# book:` |
| [Standard Ebooks](https://standardebooks.org/) | 公有领域（重排版） | ❌ | **下载 Compatible epub**，`quietly read book.epub` 直接按章导入 |
| [Wikisource 多语言版](https://zh.wikisource.org/) | CC BY-SA / 公有领域 | ✅ 部分作品有旧译 | 中英对照长篇：原文取 Gutenberg，旧译取这里 |
| [Tatoeba 句对](https://tatoeba.org/en/downloads) | CC-BY 2.0 FR | ✅ 句子级 | 短句精读素材，不是成篇 |
| [TED 演讲字幕](https://www.ted.com/participate/translate) | CC BY-NC-ND | ✅ 逐句对照 | **已内建**（`ted-talks` 书），每天自动补，见上文 |
| [VOA Learning English](https://learningenglish.voanews.com/) | 美国政府作品 | ❌ | 分级新闻，按栏目归书（`# book: VOA Science`） |
| [Simple English Wikipedia](https://simple.wikipedia.org/) | CC BY-SA | ❌ | 主题短文，按主题归书 |
| [LibriVox 文本对照](https://librivox.org/) | 公有领域 | ❌ | 只用来找书，正文还是 Gutenberg |
| [The Guardian 长读](https://www.theguardian.com/news/series/the-long-read) | 版权归媒体 | ❌ | **已内建**（`guardian-long-read`），一篇 4,000 词，个人学习用，不转发 |
| [Aeon 随笔](https://aeon.co/essays) | 版权归 Aeon | ❌ | **已内建**（`aeon-essays`），一篇 3,000–6,000 词 |
| [MyMemory](https://mymemory.translated.net/) / [DeepL Free](https://www.deepl.com/pro-api) | 服务条款 | ✅ 机翻 | **已内建**：没有译文的文章按段机翻，免费额度 |

内建的五个来源（`/fetch`）就是按这张表挑的：TED（有译文、直连）、BBC 媒体英语
（有译文、需代理）、[新华网英文](https://english.news.cn/)（无译文、直连、每日更新）、
Guardian 长读（无译文、需代理、**长篇**）、Aeon（无译文、直连、**长篇**）。
抓下来的文本存进本地库并标 `personal study only`，只供自己读，不随项目分发。

**推荐路径**：

1. **Standard Ebooks**：每本书页面 Downloads → **Compatible epub**（azw3/kepub 是给 Kindle/Kobo 的，
   advanced epub 结构更花哨但对抽文本没好处）→ `quietly read book.epub`。spine 里的每一章
   自动成为一篇，`dc:title` 当书名，`dc:creator` 当来源，扉页/版权页/colophon 按
   `epub:type` 跳过。
2. **Gutenberg**：只提供 `.txt` / `.html`，一章一个文件导入并写 `# book: 书名`。
3. 需要更好的译文时找公有领域旧译（民国时期译本多已进入公有领域，但各家出版社的
   **新译**有版权，注意区分），按 `---` 分段粘进同一个文件；不想找就按 `tab` 机翻。
4. `quietly library` 看进度，`/read <slug>` 按书推进。

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
text_translations(text_id, paragraph, lang, body, provider, created_at)  -- 机翻缓存
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
