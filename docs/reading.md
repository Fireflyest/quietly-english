# 阅读模块设计

目标：把「读一段英文」变成和背单词同一套对话流 —— 读到生词就地查、顺手收进词书，
读完留下进度和统计。可用内容全部来自真实语料，不生成、不臆造。

## 为什么这样切分

现有单词模块的价值在于：**词条质量**（ECDICT + 朗道 + Wiktionary）和**记忆曲线**。
阅读模块不该重复实现查词，而是把「正在读的这段文本」当作入口，把生词导进同一套卡片。

```
读一段  →  标出没掌握的词  →  按序号看卡片  →  a 收进词书  →  正常复习
                ↑                                              │
                └──────────── 下一段用新的「已掌握」集合 ──────┘
```

所以阅读模块**不新增词库**，只新增两个东西：文本本身，和阅读进度。

## 数据模型

```sql
texts(id, title UNIQUE, source, license, body, created_at)
reading_progress(text_id PK, paragraph, updated_at)
reading_log(id, text_id, day, started_at, ended_at, seconds,
            paragraphs, words_opened, words_added)
```

- `body` 存原文（段落用空行分隔），段落切分在运行时做，避免再建一张表。
- `reading_progress` 单独一张表而不是放 `settings`：一个文本一份进度，`/read` 无参时续读。
- `reading_log` 与 `sessions` 并列，不混用：学习会话记的是评分，阅读记的是段落与查词。

## 文本从哪来

**优先用户自备**，这也是唯一没有授权风险的路径：

```bash
quietly read ~/articles/sapiens-ch1.md    # 导入并开始阅读
```

其它可用来源：

| 来源 | 授权 | 说明 |
| --- | --- | --- |
| 用户自己的文件 / 粘贴 | — | `quietly read <file>` 或 TUI 里 `/read` 后粘贴 |
| Project Gutenberg | 公有领域 | 整本书，纯英文；无中文对照 |
| Simple English Wikipedia | CC BY-SA | 句子短，适合入门；需注明来源 |
| VOA Learning English | 美国政府作品，公有领域 | 语速与用词分级，适合精读 |

不做的：抓扇贝/百词斩/新闻网站正文。理由和 `docs/data-sources.md` 里对词书的结论一致。

## 生词怎么判定

只用库里已有的真实数据，不做词表猜测：

| 信号 | 来源 | 用法 |
| --- | --- | --- |
| 柯林斯星级 | ECDICT `-K5` … `-1` | 星级越低越生僻，默认把 ≤2 星标为生词 |
| BNC/COCA 排名 | ECDICT `(中高雅 663/406)` | 星级缺失时的兜底 |
| 是否在词书里 | `book_words` | 已收录但长期没复习的不重复标 |
| 已掌握 | `cards.interval_days >= 7` | 认为掌握，不再高亮 |
| 查不到 | `words` 无此词条 | 单独标为 `unknown`（多为专名），默认不高亮 |

判定顺序：

1. 分词：小写化，只保留 `[a-z'-]`，长度 ≥ 3
2. 去掉停用词（内置约 100 个高频虚词）
3. 命中「已掌握」集合 → 跳过
4. 命中词书但未掌握 → 标为 `review`（黄色，提示该复习了）
5. 星级 ≤ 2 或排名靠后 → 标为 `new`（青色，值得学）
6. 其余 → 不高亮

每段最多标 8 个，按难度排序，避免一段全是高亮。

## 界面

一段一段读，复用现有的 `Message` / `ToolView`：

```
⏺ read  Animal Farm · 3/12
⎿ 5 words to look at · n next · p prev · 1-5 open · a add word
  All animals are equal, but some animals are more equal than
  others. Whatever goes upon two legs is an enemy. ▌tyranny▐
  begins when one group claims the right to decide for another.

  1 review  equal      a. 相等的, 平等的
  2 new     tyranny    n. 专制, 暴政
  3 new     claim      vt. 声称, 要求
  ...
```

- 高亮的词用 accent 色，其余用正文色；换行沿用 `wrap_spans()`（代码块已经在用，能保留逐词样式）
- 下面的编号列表只给词性和中文，**不展开例句和词根**——想看细节按数字，直接复用 `word_card`
- `a` 把当前高亮词加进当前词书（走已有的 `add_to_book`）

按键：

| 键 | 行为 |
| --- | --- |
| `n` / `space`（输入框为空） | 下一段 |
| `p` | 上一段 |
| `1`-`9` | 打开第 i 个生词的完整卡片 |
| `a` | 把选中的词收进词书 |
| `/read <file>` | 换一篇 |
| `esc` | 退出阅读（写进度） |

与学习会话互斥：进入阅读时若正在 `study`，先结束会话并提示。

## 分层落点

按 `docs/architecture.md` 的四步扩展：

| 层 | 新增 |
| --- | --- |
| `qe-core/src/reading.rs` | 段落切分、句子切分、分词、生词判定、`paragraph_view()` 构造视图 |
| `qe-core/src/store.rs` | `texts()` / `text_by_title()` / `save_text()` / `reading_progress()` / `log_reading()` |
| `qe-store/src/db.rs` | 上面五个方法 + v4 迁移（三张新表） |
| `qe-app/src/main.rs` | `quietly read <file>` 子命令、`Command::Read` |
| `qe-app/src/reading.rs` | `ReadingSession { text, paragraph, highlights, cursor }` 与按键 |
| `qe-tui/src/lines.rs` | `ToolLine::Marked { spans }`（复用 `wrap_spans`）、`ToolLine::Pick { index, kind, word, gloss }` |

`qe-core` 里判定生词需要「已掌握集合」，用一个 `HashSet<String>` 传入，
不依赖 `Store`，保持纯函数可测。

## 里程碑

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| M1 | 导入文本、段落切分、`n`/`p` 翻页 | 能读完一篇文章，进度落库 |
| M2 | 生词高亮 + 编号 + 数字键开卡片 | 任意段落能查到词，卡片信息与查词一致 |
| M3 | `a` 收词、`/read` 续读、阅读时长与段落数入库 | 收的词第二天出现在复习队列 |
| M4 | `/summary` 增加阅读行 | 总结里出现 `read 12 paragraphs · 3 words` |
| M5（可选） | 句子级对照（用户自备译文）、读后填空 | 需要额外数据源，先不做 |

M1-M3 是核心，M4 是小改动，M5 视数据情况再说。

## 已知难点

- **分词**：`don't`、`animals'`、连字符复合词要保留成一个 token，不能拆成 `don` +`t`
- **句子切分**：`Mr.` `Dr.` `e.g.` 后面不是句末；中文标点 `。！？` 也要处理
- **专有名词**：句首大写无法区分普通词和地名；查不到词条的按 `unknown` 处理，不高亮
- **长文本**：只把当前段和上下各一段渲染成消息，不要一次生成全文，否则滚动列表会爆
- **高亮颜色**：`qe-tui` 的 accent 已用于评分卡，阅读要用独立颜色，避免误读成学习状态

## 待定

1. 段落级还是句子级？（句子级更适合精读，段落级更接近真实阅读）
2. 要不要中文对照？若需要，只能由用户自备或选双语语料（Gutenberg 无译文）
3. 生词阈值：柯林斯 ≤2 星起步，还是按用户正确率动态调整？
4. 读后是否要测验？要的话题目必须有出处，不能自动生成
