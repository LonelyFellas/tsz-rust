# 实现

复用现有纯文本关联存储与发布快照结构，无数据库迁移或请求字段变化。

`pending_relation_issue` 移除按保存/发布模式分叉的必绑定检查，仅执行共享文本合法性校验。
未绑定关联继续跳过真实目标解析，不进入目标引用收集。V3 校验、发布及旧版发布共用此实现。

删除后端 DTO 错误码并重新导出 OpenAPI；前端同步 wire 枚举、运行时 schema、快照哈希及错误提示。
回归覆盖发布前校验、纯文本和中文自由文本发布、不可变快照、不自动创建词条及多档例句翻译保留。

发布顺序：后端先、前端后。旧前端接受新后端的成功响应；新前端已移除旧错误码。

验收命令（先显式设置隔离的 `DATABASE_URL`、`TEST_REDIS_URL`）：
`SQLX_OFFLINE=true cargo test --locked --all-features --test lexicon_handler`
