# 词库与学习模块边界收敛

范围：保留模块化单体、共享数据库与现有可观察行为。实现基线为后端
`4a4722546ca84dcb07f5f39c7e0ada9f07fd569e`。

## 依赖与所有权

- `learning_tasks` 的来源适配继续原位放在 `repository.rs`，组合词表权限、成员身份和内容可用性。
- `lexicon` 拥有发布内容、当前发布指针、归档代际、词条锁和词形—词义语义。
- `wordlists` 保留编排、公开资格、审核及错误语义；学习保留出题、判题、快照、完成和奖励。
- 复用原生 V3 内容结构和既有测试，不增加词表来源接口、通用规则引擎或异步消费者。

## 实现

1. 新增 `lexicon/published.rs`，集中批量排序词条锁和当前未归档 V3 发布读取。
   接受调用方 `&mut Transaction<Postgres>`，不自行开启或提交事务。
2. `CurrentPublication` 暴露发布 ID 与归档代际，原始快照保持私有；仅生成候选时转换为
   `PublishedContentV3`，复用原有 forms/meanings 类型，不泄露后台聚合。
3. 内容读取校验快照所属词条；当前可用性检查不额外解析内容，不比较新旧 publication ID。
4. 学习通过该入口读取，词表锁函数保留本模块校验包装；词形—词义函数统一从内容入口导出。
5. 抽出冻结的文本规范化 v1 原语；词库与学习分别拥有校验、错误语义和版本选择。
   保留 `zh_base_v1`、`spelling_exact_v1` 和答案快照的 `normalization_version = 1`。

## 一致性与兼容

- 保持账户 → 任务/轮次 → 词表 → 词条锁序和各集合的 UUID 排序。
- 保留等待词条锁后的作者删除期限检查，以及提交前来源、账户、截止时间复核。
- 重发只影响新轮次；归档恢复、成员移除重加、公开资格撤回重发不复活旧失效轮次。
- 保留作者自用的公开资格例外、幂等回执、奖励原子性和同名发布确认行为。
- 无迁移、HTTP/OpenAPI 或前端变更；旧列名仅映射为内部 `archive_generation`。
- 数据格式与版本值不变，后端可独立发布和回退；发布、回退执行不在本任务授权范围。

## 验收

使用隔离 Postgres 和 Redis，显式设置 `DATABASE_URL`、`REDIS_URL`、`TEST_REDIS_URL`；
SQLx 测试账号须能创建临时数据库，不连接联调业务库。预期以下命令全部通过。

```sh
SQLX_OFFLINE=true cargo test --locked --all-features --test learning_runs_handler --test learning_tasks_handler
SQLX_OFFLINE=true cargo test --locked --all-features --lib
SQLX_OFFLINE=true python3 ops/ci_test_modules.py run lexicon
cargo fmt --all -- --check
SQLX_OFFLINE=true cargo clippy --locked --all-targets --all-features -- -D warnings
```

- 扩充重发测试：旧题面和答案不变，新轮次采用新发布内容。
- 扩充成员恢复测试：移除后立即重加，其间未读取轮次，旧轮次仍失效。
- 复用内容选择、归档/公开恢复、等锁跨注销期限、完成/奖励原子性和幂等测试。
- 规范化 v1 保持大小写、全角、标点、空白、长度与控制字符行为；未知版本明确拒绝。
- 核对 OpenAPI、迁移、Cargo.lock、SQLx 宏缓存均无变更，新增源码不新增测试 target。
