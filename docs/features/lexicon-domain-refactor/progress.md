# 实施进度

## 状态

**第一批已通过 PR #181 合入并部署（用户确认），合并提交 `cbf2c2632da65962fe96d3284fece2bec08dfa99`。第二批已开始，当前只完成关联搜索分页子项；整批未完成。第三至第五批未开始。**

- Worktree：`/Users/darwish/Dev/tsz-core/tsz-rust-lexicon-domain-refactor`
- 分支：`feat/lexicon-domain-refactor`
- 基线：`72747b6186562ecf87ac2984a2d1824f6c50fc9e`
- 上述 worktree/分支/基线为第一批实施记录。第一批后续合入部署状态见本节顶部。
- 第二批继续沿用已确认决策，不写旧格式/旧数据兼容；本轮不提交、推送或部署，不清理远端数据。

## 第一批完成清单

| 验收项 | 实现/证据 |
| --- | --- |
| 单一完整内容模型 | 读取、保存、发布、语义校验、稳定节点生成、关系表写入、自动例句扫描直接使用 V3 内容。移除 `DraftMeaningsStepContent`、`DraftFormsStepContent` 及旧聚合族，无别名或双轨分流 |
| 无聚合往返 | 删除 `meanings_relational_projection`、反向重建和翻译/成分/语音/链接 restore 函数；源码及测试检索不再有这些类型/适配器 |
| 原位规范化、字段保真 | 完整内容原位处理，富文本叶节点复用标注算法；全文相等与幂等测试覆盖多档翻译、语音、音频、文本链接、词义组、成分和多组绑定 |
| 原生稳定节点 | `validation::proposed_meaning_nodes` 包含全量译文和成分；主译文别名不重复建节点，只读例句关联不参与。跨节点 ID 冲突不能通过去重被吞掉 |
| 无伪造词形/词头 | 删除 `v3_meaning_validation_forms/headwords`；词性、组定位和目录引用检查使用真实原生词形 |
| 原生仓储 | 仓储直接写入完整模型的对应字段；不再显式写已停用的预绑定字段；多档翻译写入不再转换为旧 RichText 结构 |
| 音频服务端事实 | 原位灌入数据库元数据，客户端篡改无效；发布、引用回收、词形重存回归通过 |
| 失败原子性 | HTTP 回归新增非主译文与词义 ID 碰撞，返回可定位 `node_id_reused`、不留下词义行，原 revision 仍可正常提交 |
| 契约与生成物 | OpenAPI 重导：只有三个共享类型的说明文字变化，去掉 `description` 后与基线完全一致；SQLx 刷新无缓存差异 |

### 刻意保留的边界

- `RichTextV1/V2` 是富文本文档格式，不是旧词条内容格式，继续复用标注算法。
- surface snapshot 和搜索分页中独立版本轴的名字不按 V2 字样一刀切删除。
- 缺字段保留、翻译展示别名等既有 wire 协议并非本批新建兼容；破坏性接口调整须在相关批次同步前后端。
- 本批不实施独立例句发布、批次发布、发布权限、新版本式回退、搜索分页规则等后续决策。现有发布草稿目标等行为的回归通过，不等于那些新规则已经上线。
- 没有 schema migration。现有 nullable 预绑定列没有为本次新建或迁移数据，本批仅停止旧模型对它们的写入。

## 最终验证

所有运行在任务独立容器中，不访问共享 5433/6379 或远端服务：

- `tsz-lexicon-domain-pg`：Postgres 16，本轮 `127.0.0.1:65088`，维护数据库 `lexicon_test`；SQLx 为各数据库测试创建独立库。
- `tsz-lexicon-domain-redis`：Redis 7，本轮 `127.0.0.1:65089/0`；显式设置 `TEST_REDIS_URL` 和 `REDIS_URL`，不走 fallback。
- 容器均带 `task=lexicon-domain-refactor` 标签；没有挂载其他任务卷。端口可能随重启变化，重启后必须重新检查。
- 收尾已停止这两个任务容器，未删除卷；可用 `docker start tsz-lexicon-domain-pg tsz-lexicon-domain-redis` 恢复。

最终代码验证（退出码 **0**）：

| 命令 | 结果 |
| --- | --- |
| `SQLX_OFFLINE=true cargo clippy --locked --all-targets --all-features -- -D warnings` | 通过，无警告 |
| `SQLX_OFFLINE=true cargo test --locked --lib lexicon:: -- --test-threads=4` | 192 passed |
| `SQLX_OFFLINE=true cargo test --locked --lib openapi::` | 10 passed |
| `SQLX_OFFLINE=true cargo test --locked --test lexicon_handler --test lexicon_v3_lifecycle --test lexicon_audio_asset_references -- --test-threads=4` | 97 + 6 + 16 passed |
| `cargo fmt --all --check`、`git diff --check` | 通过 |

合计 **321 项测试通过**。新增单元模块与 HTTP 场景都属于既有 CI target，没有新增未登记的测试 target。

生成物验证：在任务隔离维护库运行现有迁移并 `cargo sqlx prepare -- --all-targets --all-features`；`.sqlx` 无差异。最终代码另执行 `cargo sqlx prepare --check -- --all-targets --all-features`，退出码 0。执行 `SQLX_OFFLINE=true cargo run --locked --all-features --bin export_openapi`，并用解析后的 JSON 递归对照证明仅 description 变化。

本地日志：
- `/tmp/lexicon-b1-close-final.log` / `.exit`
- `/tmp/lexicon-b1-close-sqlx.log` / `.exit`
- `/tmp/lexicon-b1-close-sqlx-check.log` / `.exit`
- `/tmp/lexicon-b1-close-openapi.log` / `.exit`

没有把仅含注释、运行 0 项的 `lexicon_v3_relation_consumers` 当作验证证据。

## 第二批进行中

### 隔离基线

- 后端：`/Users/darwish/Dev/tsz-core/tsz-rust-lexicon-domain-batch-2`，`feat/lexicon-domain-batch-2`，fetch 后最新 `origin/main` = `cbf2c2632da65962fe96d3284fece2bec08dfa99`。
- 前端：`/Users/darwish/Dev/tsz-core/tsz-lexicon-domain-batch-2`，`feat/lexicon-domain-batch-2`，fetch 后最新 `origin/main` = `3a1bd05`。当前仅调查、尚无前端代码改动。
- 原第一批 worktree 及其他任务工作区未修改。

### 实际差距

详情见 `design.md` 第二批章节。可复用入站引用采集/判定、来源链接和 focus_node 导航；缺口是草稿与发布校验边界、具体节点发布状态、默认关闭草稿候选，以及独立成分/句中搜索分页。学习端现有练习占位与词表 mock 不能提供真实消费验证证据。

### 已完成子项：关联搜索弱一致分页

- 删除 `related_search_dataset_version` 和 outbox 行数/重试耦合，游标只携带查询身份及复合排序键/已返回数量。
- 保留签名校验、管理员绑定、查询参数绑定和确定性 keyset 排序。
- 以本页查询剩余数量判断 next_cursor；total 为已返回数量加当前剩余数量的弱一致估计，不再以首页冻结总数提前结束。
- HTTP 回归覆盖：跨页新增目标、已返回目标保存草稿、正常继续/结束、后续目标归档、不同查询/分页大小/管理员及无效游标拒绝。
- 不改 DTO、method/path、OpenAPI、数据库 schema 或 SQLx 宏查询，无新增迁移/缓存生成物。未加旧游标或数据兼容分支。

### 本轮验证

任务专属容器（标签 `task=lexicon-domain-batch-2`），不访问其他任务实例或远端数据库：

- `tsz-lexicon-b2-pg`：Postgres 16，`127.0.0.1:63526`，维护库 `lexicon_b2`；SQLx 测试自动创建独立测试库。
- `tsz-lexicon-b2-redis`：Redis 7，`127.0.0.1:63745/0`；同时显式设置 TEST_REDIS_URL / REDIS_URL，不走默认 fallback。
- 编译缓存使用 `/Users/darwish/.cargo-target`，启动时无其他 cargo 构建进程。
- 本轮收尾已停止两个任务容器，未删除数据卷。恢复时可 `docker start tsz-lexicon-b2-pg tsz-lexicon-b2-redis`，再核对端口，不沿用历史端口假设。

首次新增 HTTP 测试在查询约束断言失败：测试预期 422，既有 GET 契约实际 400；修正测试断言，未改变既有错误响应。

最终命令退出码 0（`SQLX_OFFLINE=true`，数据库命令使用上述隔离连接）：

| 命令 | 结果 |
| --- | --- |
| `cargo test --locked --test lexicon_handler related_search_cursor_survives -- --nocapture` | 1 passed |
| `cargo test --locked --lib lexicon:: -- --test-threads=4` | 192 passed |
| `cargo test --locked --test lexicon_handler -- --test-threads=4` | 98 passed（包含新增场景） |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | 通过 |
| `cargo fmt --all --check`、`git diff --check` | 通过 |

不重复计数，合计 **290 项**。日志：`/tmp/lexicon-b2-pagination-final.log`，退出码：`/tmp/lexicon-b2-pagination-final.exit`。旧发布草稿目标等回归通过只保护尚未修改的路径，不代表目标引用规则已实现。

### 未完成（阻断第二批验收）

1. 统一草稿/发布引用规则及影响分析，允许草稿编辑后在发布前处理破坏性引用；补齐各种引用业务结构及节点定位验证。
2. 引用修复入口的实际闭环与缺失来源节点反馈；现有跳转可复用，但未做本轮 UI 验收。
3. 关联词/成分/例句候选默认发布、主动展开草稿，已发布词条新增词义的具体依赖标识，以及与保存/发布目标解析一致性。
4. 成分和句中候选分页的弱一致稳定键改造，不能将本轮关联搜索修改泛化为全部搜索完成。
5. 上述变更需要的契约同步、前端测试和真实联调。

本轮代码保留为未提交改动；没有提交、推送、PR、部署或真实业务数据修改。
