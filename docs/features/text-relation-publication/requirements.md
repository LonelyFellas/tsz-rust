# 手输关联词允许发布

关联词不是必填项。已填写的手输文本不因未绑定具体词条而阻止发布前校验或发布。

- 草稿、发布响应、不可变快照与重新读取均保留文本和已有文本释义。
- 不自动创建词条、不自动匹配目标、不生成不存在的目标引用。
- 已绑定关联词沿用现有引用校验；文本长度、控制字符等存储限制保留。
- 删除 `relation_pending_target_unresolved` 发布限制及前后端错误码、提示和契约枚举。

发布顺序：后端先更新，再更新前端；新前端不再接受旧后端的废弃错误码。

验收命令（先显式设置隔离的 `DATABASE_URL`、`TEST_REDIS_URL`）：
`SQLX_OFFLINE=true cargo test --locked --all-features --test lexicon_handler v3_text_relation_round_trips_through_publication -- --exact`
