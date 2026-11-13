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

每次提交都会保留，不会覆盖之前的答案。评分和反馈可以先由用户或外部批改流程写入，
后续再接入 Gemini 等服务。

数据库中的 `translation_exercises` 保存题目和参考译文，
`translation_attempts` 保存答案、评分、反馈、provider 和时间。
下一步会接入多行编辑器、练习会话和结构化 AI 批改结果。
