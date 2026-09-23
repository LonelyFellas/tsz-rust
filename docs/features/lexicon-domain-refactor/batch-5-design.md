# 第五批：跨链路收口

状态：2026-09-23 必要前后端实现与本地验证完成；未提交、推送、PR、合并或部署。第四批线上全部业务组合的人工验收不由本轮本地验证替代。

## 基线与边界

- 本次重新 fetch 后，后端 origin/main 为 `3053d09265573c9ec0b42c53ea718f023f47ce5a`，前端为 `e77738a9c5e688f4a1bd645b288c5d8eba776e78`。这是本轮开始时核实的基线，不宣称它们永远是最新 main。
- 两仓分支均为 `feat/lexicon-domain-batch-5`，工作树位于 `/Users/darwish/Dev/tsz-core/.worktrees/{backend,frontend}/lexicon-domain-batch-5`。验证对象为上述 SHA 加本批工作区差异。
- 第四批已配套部署至 tshb-test（用户交接）；旧文档的“未部署”仅描述当时检查点，已补记当前状态。
- 不恢复“发现并关联词条”，只保留 voice-editor 原有选文、标注、关联；不清理、迁移或接管旧工作区及服务，不清理共享缓存，不延用历史数据清理授权。
- 不新建学习系统。`WordSentenceWritableV3 → EnglishTextV3 → RichTextVariantV3` 无 `audio_assets`；只验证实际音频持有者（词条发音编辑器、语法结构变体）。

## 已有能力、真实缺口与实施

| 链路 | 已有能力 | 本轮措施与证据 |
| --- | --- | --- |
| 第四批 | 独立草稿/发布/历史、宿主可见性、混合批次、权限和原子性 | 先在新隔离环境通过共享例句 24 项、第四批专项 9 项；最终完整 handler 再覆盖这些路径 |
| 词条并发 | revision、本地输入保留、明确保留/放弃、在途请求失效保护 | 只补两侧字段差异，不重做会话逻辑；新增回归证明继续输入会使旧比较请求失效，不覆盖输入或偷偷采用旧响应 |
| 共享例句并发 | 后端 revision 检查；失败保留输入 | 补刷新比较、明确保留/放弃和最新 revision 重提；获取失败、未做选择、再次冲突均不能自动保存；放弃输入须二次确认 |
| 音频保护 | 草稿/发布结构化引用、七天宽限期、FOR SHARE / SKIP LOCKED、失败保留资产行 | 持资产排他锁后，用新语句快照再次查引用，避免先删对象再由 FK 拒绝删行；补历史非当前版本的保护、回退原资产 ID、试听 URL 与草稿不覆盖断言 |
| 音频缺失 | resolveUrl 已将请求失败显式抛给编辑器 | 补 404 失败不会上传/确认替代资产的回归，不增加静默替换或 TTS 降级 |
| 同形词 | 检测复用入口、独立建条确认、annotation 冲突、稳定节点 ID | 按本轮用户确认补独立 `homograph_reason`，同原型组事务内强制校验并写审计；保留归属、数字标注、签名 token 与幂等规则 |
| 性能 | 列表分页、入站引用、显式批次 | 测量 50 条循环依赖的最大批次和真实列表/引用 handler；没有瓶颈证据，不改查询架构 |
| 契约 | 严格 OpenAPI、wire 类型和 runtime schema | 同步新增请求字段、生成快照与 schema 来源 SHA；响应形状不变，没有数据库迁移 |

## 同形说明设计（本轮用户确认）

采用现有同原型组：同 kind、英语、同方言规范化原形、未归档。普通词形碰撞和更宽的前端提示不扩大强制说明范围。

- `CreateAdminWordV3Input.homograph_reason` 为可选文本；同原型组另建时必填。提供时 trim 后 1–500 个 Unicode 字符，不含控制字符。
- 新字段不替代数字 annotation，也不是词条详情/候选展示字段。写入 `audit.admin_actions` 的 `lexicon.entry.create.v3` metadata，与创建、旧标注更新、幂等结果同事务提交。
- 规范化后计算幂等 hash；相同请求重试返回同一词条，不同说明复用 key 返回 `idempotency_conflict`。
- 既有 annotation 冲突先返回分组。前端在该分组弹窗同时填写数字标注和文本说明；冻结重试保持原说明/key/body，分组与 surface 更新保留说明。
- 缺失必填说明或非法说明返回 `400 invalid_request_body`、`field=homograph_reason`。直接 HTTP 请求同样受限；拒绝时旧条标注与新条均不发生部分写入。
- 普通词形命中无说明仍可在既有签名确认后创建；测试显式验证这一边界。

## 步骤 → 验证

1. 第四批关键回归 → 独立 PG/Redis，真实迁移与 handler 请求，保护原子性、权限、历史和宿主可见性。
2. 缺口先红后绿 → 差异区域/例句恢复流程测试先失败；同形缺说明的直接创建先得到错误的 201，修复后拒绝且无部分写入。
3. 跨链路补测 → 音频历史保护、最大批次与列表/引用样本、说明审计、幂等、稳定 ID。
4. 集成收口 → 全仓前端测试与类型、受影响 lint、后台构建；后端 fmt/clippy/定向集成；真实浏览器经本批代理访问本批后端和独立存储。

## 最终自动验证

Rust 均指定 `SQLX_OFFLINE=true` 与下述隔离数据库、Redis，测试并发为 2：

| 命令/范围 | 结果 |
| --- | --- |
| `cargo test --locked --all-features --lib` | 293 通过 |
| `cargo test --locked --all-features --test lexicon_handler -- --test-threads=2` | 最终完整重跑 120 通过 |
| `--test shared_sentences` | 24 通过 |
| `--test lexicon_v3_lifecycle` | 6 通过 |
| `--test lexicon_audio_asset_references` | 16 通过 |
| `cargo clippy --locked --all-features --all-targets -- -D warnings` | 通过 |
| `cargo fmt --check` | 通过 |
| `cargo sqlx prepare -- --all-targets --all-features` | 隔离库通过；无 `.sqlx` 差异 |
| `cargo build --locked --all-features --bin tsz-rust --bin seed` | 通过，供真实浏览器验收 |

前端：

- `pnpm test`：193 个文件通过，2904 项通过、2 项原有跳过。随后增补音频缺失断言，其完整 `audioUploadAdapter.test.ts` 8 项通过；补同原型分组消失时不扩大必填范围的边界后，`EntryAnnotationModal.test.tsx` 与 `UnifiedCreateEntryStep.test.tsx` 共 64 项通过，并复跑 admin 类型检查、lint 与构建通过。不把不同轮次统计直接相加。
- `pnpm --filter @tsz/api-client test`：9 个文件、404 项通过，含新增请求字段透传、400 字段错误与严格错误响应解码。
- `pnpm typecheck`：7 个包通过（未变的 ui 包使用有效缓存，其余实际执行）。
- `pnpm --filter @tsz/admin lint`、`pnpm --filter @tsz/api-client lint` 通过；音频补测后再次检查该文件 lint 通过。
- `pnpm --filter @tsz/admin build` 通过。Vite 提示未来 native configLoader 不支持现有无扩展名 import；属于既有构建警告，本轮未顺手修改配置。
- 生成输入：本后端工作区 `docs/openapi.json`；SHA-256 为 `32f2eb74c8fbc6c47576c66f7bba98e05431dfa121540da67f1f5ae35f51db65`。runtime bundle 的 `_source_sha256` 与契约 canary 均匹配。

中间失败均保留其原因，不计为通过：同形新规则使旧成功夹具缺少说明而失败，补明确说明后完整重跑 120 项通过；OpenAPI canary 旧 hash 在重新核对生成输入后更新；首次性能脚本误用列表 envelope、浏览器验收脚本的路径/状态码/按钮选择器错误均已按真实契约修正并复跑。词条在途输入测试最初错误地期待已失效的响应展示差异，核实已有会话保护后保留原实现，改为断言输入与旧基线不被覆盖。

## 性能实测

`lexicon_handler::batch5_list_references_and_maximum_batch_measurement` 构造 50 个真实词条，形成 50 条循环词义关系，使用 API 上限 50 条显式批次全部发布；确认发布数和列表分页总量、实际引用存在。

在本机 debug 构建 / Docker Postgres 上，一次单独实测：

| 操作 | 样本 | 结果 |
| --- | --- | --- |
| 50 条循环依赖批次发布 | 1 次（非幂等缓存重放） | 983.57 ms |
| 列表分页 | 20 次，每页 20，跨 3 页 | p50 55.02 ms；p95 69.49 ms |
| 入站引用分析 | 20 次，不同目标 | p50 57.36 ms；p95 76.48 ms |

这是跨链路回归样本，不是大词库容量或生产 SLA。没有据此推测慢查询或重构锁架构；实际项目没有提供生产级数值验收阈值。

## 真实浏览器到存储证据

临时验收脚本 `/tmp/tsz-lexicon-b5-acceptance.cjs` 使用 Playwright Chromium，无 page.route/API mock；以本任务本地账号通过页面登录。脚本包含本地测试凭据，不提交或发布。

| 端/依赖 | 实际来源与去向 |
| --- | --- |
| 管理后台 | 本前端工作区 Vite dev，`http://127.0.0.1:13585`，最终验收 PID 42638；不是 production preview 代理验收 |
| 后端 | 本后端工作区重新 `cargo build` 的 tsz-rust，`127.0.0.1:18585`，最终验收 PID 42636 |
| 代理 | `/api/v1` → `http://127.0.0.1:18585/api/v1`，页面登录与词库请求证明真实路由 |
| Postgres | 新容器 `tsz-lexicon-b5-pg`，`127.0.0.1:59603/lexicon_b5`，不使用旧环境业务库 |
| Redis | 新容器 `tsz-lexicon-b5-redis`，`127.0.0.1:59609/0`，显式 TEST_REDIS_URL/REDIS_URL |
| mock | 词库、词性、TTS、local speech mock 关闭；OTP 使用项目原生本地 Mock sender；未配置付费语音或生产 OSS |

实际通过：

1. 浏览器登录 → 真实代理/API 返回 200；辅助 API 只在本任务隔离库建立验收 fixture。
2. 页面打开共享例句草稿后，另一个真实请求修改等级；页面保存得到 409，刷新后展示 B1/C2 差异，输入保留；明确保留再提交返回 200。独立 GET 证实 revision=3、用户选择的 B1 持久化。
3. 页面检测已有原形 → 显式另建确认 → 后端同原型分组弹窗；仅填数字标注时创建按钮禁用，填写区分说明后创建返回 201。直接读取本任务库审计行，确认说明原子保存。
4. 新条使用新 ID；重新 GET 原例句，其标注仍指向原词条 ID，没有按拼写自动重绑。

最终成功 fixture：
- 原词条：`01a0cd93-bdc1-7af3-9726-f75c6fe3f186`
- 共享例句：`55097e27-7fc8-4d0a-ba0c-e4c6a6b4fd35`
- 同形新词条：`01a0cd93-c835-73b0-8204-9c78463371a7`

脚本在 finally 中只停止自己启动的后端/Vite/浏览器；最终已核实 PID 42636/42638 均退出，独立容器和测试数据保留。本批后端验收二进制 SHA-256 为 `7f1c47ad887581e5335a6102d1161882ed40294804aba206a0c66f03367a83eb`。早期验收脚本调试也只在该隔离库创建了少量 fixture，不清理旧业务数据。SQLx 测试库由 `sqlx::test` 隔离管理。Rust 复用用户配置的 `/Users/darwish/.cargo-target`，没有清理缓存；前端依赖安装在新工作区。

## 交付与剩余边界

- 本轮仅本地实施和验证，无提交/推送/PR/合并/部署，无服务器数据操作。
- 新前端同原型创建会发送旧后端 `deny_unknown_fields` 不接受的字段；新后端拒绝旧前端缺少说明的同原型新建。普通无说明的新词与响应形状不变，但不是无缝混部兼容；后续交付需受控停用同原型新建并配套替换。详见 `docs/frontend-integration.md` 第五批说明。
- 未做全站 Playwright E2E、生产 nginx 链路、大型真实词库压测、生产 OSS 实际音频试听，未补做第四批线上全部组合的人工验收。音频历史保护以真实数据库、HTTP handler 和 MemoryAdapter 对象存在性为证据，不宣称生产 OSS 验收完成。
- 音频回收二次检查已在持锁事务内实现；现有并发保存锁测试、孤儿回收、对象删除失败与历史保护通过，未声称穷举每一种 MVCC 调度窗口。
