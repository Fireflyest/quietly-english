# 翻译学习

翻译学习和阅读中的 `/translate` 不同：阅读翻译用于理解文章，翻译学习保存用户答案，
方便批改、修改和复习。

当前已支持保存练习题和多次提交：

```text
quietly translate-practice create "尽管下雨，他们还是来了。" `
  --reference "Although it rained, they still came."

quietly translate-practice submit 1 "Although it was raining, they came." `
  --score 85 --feedback "meaning is accurate"

quietly translate-practice history 1
```

每次提交都会保留，不会覆盖之前的答案。

## 题目来源

目前有两种：

1. **手动**：`quietly translate-practice create <原文> --reference <参考译文>`
   （`source_type: manual`）。
2. **中文网页**：抓一个中文文章页，按 `<p>` 拆段，每段一题
   （`source_type: web:<url>`，方向 `zh-en`）——参考译文没有现成的，
   由 `models.json` 里选中的模型逐段生成。

```text
# CLI
quietly translate-practice from https://example.cn/article/1

# 应用内（后台跑，界面不卡）
/practice https://example.cn/article/1
```

导题的取舍：**先验模型 key**（没有 key 出不了参考译文，不白抓网页）；
**按原文去重**（同一页面重复导入不长重复题）；**一次最多 12 题**（逐段过模型，
不能一篇长文打光额度）；导航/版权/过短/英文段自动滤掉。结果里会列出新建题的
`#id`，接着 `history <id>` 就能看题。

## 合集与列表

每道题都挂在一个**合集**（`practice_sets`）下，类似阅读的书：

| 合集 | 什么时候建 |
|---|---|
| 页面标题 | 网页导入（`from` / `/practice <url>`），一篇文一个合集 |
| 来源名 | 每日自动发现，一个 `practice_sources.json` 条目一个合集 |
| 手动出题 | `create` 手动建的题 |
| 未分组 | 迁移前就存在的旧题 |

```text
/practice                       # 应用内：合集一览（题数 · 已作答 · 已批改 · 来源）
quietly translate-practice list # CLI 同款
```

## 每日自动发现

和阅读新闻同一套机制：每天第一次启动查 `practice_fetched_on`，后台跑一轮，
界面不阻塞。来源配在数据目录的 `practice_sources.json`（缺了自动写出示例）：

```json
[
  { "name": "某网历史", "url": "https://www.example.cn/lishi/", "limit": 2 }
]
```

程序抓列表页、挑**同域文章链接**（标签页、翻页、图片、外站自动排除），
导过的 URL 记在库里直接跳过；每个来源每天最多 `limit` 篇（默认 2，封顶 6）。
手动跑一轮：`quietly translate-practice daily` 或应用内 `/practice daily`。

评分和反馈可以先由用户或外部批改流程写入，
也可以直接让 AI 批改：

```text
# 提交时顺手批改
quietly translate-practice submit 1 "Although it was raining, they came." --ai

# 事后批改某次提交（覆盖旧分数，可反复重批）
quietly translate-practice grade 7
```

批改返回 **0–100 的分数**和中文短评（先说可取之处，再说主要问题和改法），
按「准确 50 + 通顺 30 + 语法完整 20」打分；结果写进该次提交的
`score` / `feedback` / `provider`，`history` 里直接能看到。
显式给了 `--score` / `--feedback` 时以人写的为准，AI 结果只作底稿。

批改走**模型无关**的通用入口（`translator::ai_settings` / `ai_generate`），选哪家按
数据目录 **`models.json` 的条目顺序**取第一个配了 key 的（默认 **Gemini > 小米 MiMo >
DeepSeek**，改文件顺序就改调用顺序）；key 用 `/apikey set gemini_api_key <key>` 这样配
（或环境变量）。清单里同时存着地址和候选型号，型号升级、换端点都改这个文件，
不用改代码——格式与说明见 [docs/reading.md 机翻](reading.md#机翻免费额度按段用)。
全都没配会报错并指路 `models.json` 与 `/apikey`。

数据库中的 `translation_exercises` 保存题目和参考译文，
`translation_attempts` 保存答案、评分、反馈、provider 和时间。
下一步会接入多行编辑器和练习会话。
