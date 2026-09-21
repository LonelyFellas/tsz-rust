# 实施进度

## 状态

**第一批内部模型重构已完成并通过验收。第二至第五批未开始。**

- Worktree：`/Users/darwish/Dev/tsz-core/tsz-rust-lexicon-domain-refactor`
- 分支：`feat/lexicon-domain-refactor`
- 基线：`72747b6186562ecf87ac2984a2d1824f6c50fc9e`
- 用户授权五批实施，不写旧格式兼容；未提交、推送、部署或清理远端数据。

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

## 下一批

第二批：沿用现有入站引用实现，统一引用规则与影响分析，补齐实际前端/学习端消费者调查，落实草稿候选与搜索分页调整。第一批没有这方面的功能完成声明，也没有前端 UI 验收声明。
