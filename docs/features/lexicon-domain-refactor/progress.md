# 实施进度

## 2026-09-22：第三批本地实施与验收

> 后续按用户“开始本地验收”恢复环境并保持运行：Admin `http://127.0.0.1:13003`，API `127.0.0.1:18585`，隔离 PG/Redis 重新分配为 `60975` / `60976`。恢复前两仓源码 fingerprint 与后端二进制哈希均匹配原验收记录。新浏览器会话复核普通发布者/未授权编辑者权限、当前第 4 次发布和保留草稿通过；结果见私有验收目录 `resume-acceptance-result.json`。下文“已停止”是前次收尾历史状态。

- 用户授权准备方案并开工。两仓从最新 main 建立 `codex/lexicon-domain-batch-3` 独立工作区；前端基线 `1255d71`，后端基线 `465d639`。
- 第二批剩余改动已合入后端 PR #184 / 前端 PR #318。实查测试服务器，API 为 `465d639`；本会话已将 Admin 更新至 `1255d71`，精确 CI run `35691479752` attempt 1 成功，制品 verify、Nginx、HTTPS 200、未登录 API 401 通过。未重跑完整业务验收。
- 第三批方案见 [batch-3-design.md](batch-3-design.md)：已核实权限、发布事务、历史指针切换、数据库唯一约束和前端消费者；明确了可复用部分及新增工作。
- 两项决策已获用户确认：独立发布开关、普通管理员默认关闭；普通发布者暂时不得发布他人草稿。进入实施。
- 第三批两仓实现与约定验收已完成。独立发布授权、显式原子批次、新版本回退及配套 UI/契约已落地；没有提交、推送、开 PR、合并或部署第三批。
- 普通管理员默认无发布权，由超管授予；普通发布者仅可发布本人草稿，超管例外。编辑归属不扩大，已发布词条归档/恢复也检查发布权。
- 批次按显式选择 1–50 条执行，保留 revision/lifecycle 与幂等保护；支持循环依赖及正文/via-phrase 的批内新版本引用，失败没有部分发布。回退新增 publication，保留草稿内容、revision、节点墓碑和 draft_based_on。

### 第三批 PR 交付授权

用户在本地页面验收后明确要求提 PR，授权两仓必要的提交、独立审查、推送和创建配套 PR；不包含合并或部署。本节之后的交付结果以实际提交和配套 PR 为准。

### 第三批验证证据

- 用户随后要求在当前内置浏览器直接验收：使用新建本地超管“第三批本地验收”，通过页面授权普通测试账号、刷新确认授权持久化，再撤回恢复原状态。使用新合成循环依赖词条 `bthreeuiomega` / `bthreeuisigma`，一条测试草稿故意不完整时，页面拒绝整批；数据库确认两条均无 publication 和当前指针。修复测试内容、刷新重新选择后，页面一次发布两条，数据库确认两条发布及完整循环引用。随后在 omega 保存不同草稿，通过历史页面回退，产生第 2 次 publication，刷新后原草稿仍显示，数据库逐字段核对 forms/meanings/revision/draft_based_on 不变。fixture 准备与修复用本地 API，受测授权/批次/回退命令由当前页面操作；没有把 fixture API 操作当作页面验收。当前页面与服务保留。记录及截图：私有验收目录 `interactive-current-tab-acceptance.json` / `.png`。

- 前端全量：**194 文件，2914 passed，2 个原有 skipped**；收尾当前版本回退与批次 UI 定向 **41 passed**。全仓 typecheck/lint 通过，最后 UI 小改动再次定向核对类型、lint 和相关组件。
- 后端 HTTP：`lexicon_handler` **110 passed**、`lexicon_v3_lifecycle` **6 passed**、`shared_sentences` **24 passed**、`admin_profile_handler` **12 passed**；库测试 **308 passed**。clippy all-targets/all-features `-D warnings`、SQLx prepare --check、fmt 与 diff check 通过。
- 新增风险验证覆盖：循环依赖和幂等重放；漏选目标拒绝；直接正文与 via-phrase 指向批内新 publication；反向选取顺序并发只有一批成功，另一批版本冲突；批内来源替换但批外草稿仍保护目标；晚阶段故障整体回滚；撤权与归属；回退来源、新发布记录与草稿保留。
- 新迁移在额外空隔离库完成 up/down/up；已有回退历史时 down 拒绝，整段 deployment undo 无部分执行，历史不被删除。生成 SQLx 缓存仅更新管理员治理查询。
- 浏览器使用独立 Chrome context，正式登录与本地 OTP Mock，未注入 token、未使用 page.route。真实链路为 Admin `http://127.0.0.1:13003` → API `http://127.0.0.1:18585/api/v1` → 本任务 PG/Redis；词库、词性和 TTS 前端 mock 关闭，未配置付费语音服务。
- 浏览器通过：超管管理页授予普通账号发布权；普通账号在列表显式选择两个循环依赖草稿并一次发布，刷新仍为已发布；历史内容回退产生第 3 次发布，返回与刷新后的草稿内容和 revision 保留；最终二进制重建后正式重新登录，当前第 3 版也成功回退为第 4 版，草稿继续保留。未保存内容保护有既有组件回归，本轮不声称遍历了所有业务组合。
- 独立复核发现并修复：正文/via-phrase 的批内预分配 ID 不得查询尚未插入的 publication；同名 surface 全批排序锁与确认须在 entry 行锁之前；当前发布版本也应可通过界面重新发布为新版本。增量复核已闭合，无剩余阻断。
- 最终日志：`/tmp/lexicon-b3-front-final.log`、`front-types-final2.log`、`front-lint.log`、`ui-final.log`、`back-final.log`、`lib-final3.log`、`clippy-last.log`、`sqlx-final.log`、`rollback-guard.log`，除第一个完整路径外其余同为 `/tmp/lexicon-b3-` 前缀。
- 真实浏览器记录、截图、构建与 fixture 存于私有目录 `/tmp/tsz-lexicon-b3-local`；包含账号/会话的文件不入仓库。fixture 均为本轮新建合成数据，第二批工作区、服务、共享 5433/6379 及远端数据未修改。
- 失败记录未删：最初 lib 命令缺少隔离 DATABASE_URL，补齐后又发现部署回撤测试硬编码旧迁移最高版本，已对齐；前端全量两次因旧向导 fixture 缺新授权而等待不可见按钮，保留采样后仅终止本任务 worker，修正 fixture 后全量通过；旧激活断言改为新版本回退断言。另一次复验复用已轮换的测试 refresh cookie 触发防重放保护，仅停在登录页；改为正式重新登录后当前版本回退通过。未将这些失败记为通过。

### 交付边界

- 两仓当前均为 `codex/lexicon-domain-batch-3` 的未提交改动；原主工作区保留。
- 两个过渡版本组合不支持新的完整发布流程：旧 /activate 已移除，新 /rollback、批次和授权端点不在旧 API。后续交付需按方案受控配套部署，不能沿用第二批的部署授权。
- 不涉及第四批共享例句独立版本、不实现学习端、不做大型词库性能基准。未运行全项目 E2E；本轮真实浏览器只验证上述关键路径。
- 本任务隔离容器为 `tsz-lexicon-b3-pg` / `tsz-lexicon-b3-redis`（标签 `task=lexicon-domain-batch-3`），本轮使用 PG `127.0.0.1:52143`、Redis `127.0.0.1:52144`；恢复时须重新核对端口。本轮收尾仅停止已核实身份的第三批 API、Vite 和这两个容器，隔离数据、工作区及日志保留。
- 以下为第二批及更早检查点的原始记录，不代表本节之后的当前状态。

## 状态

**本轮基线已核对：后端 `453bf13`（PR #183）、前端 `294886a`（PR #316）；分页子项已合入并由用户确认部署，不重复开发。当前正在实现剩余候选/引用功能，整批仍未完成。以下旧章节保留历史证据，当前进展见文末“句中发现与本轮验收收尾”。**

**第一批已通过 PR #181 合入并部署（用户确认），合并提交 `cbf2c2632da65962fe96d3284fece2bec08dfa99`。第二批关联搜索分页已通过 PR #182 合入并部署（用户确认），合并提交 `c33b8b2fd32be7fcba711a1de6138929fd848879`。续作新增成分/句中候选稳定键分页，进入独立 PR 交付；整批仍未完成。第三至第五批未开始。**

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
4. 成分和句中候选分页的弱一致稳定键改造：续作已实现，验证与剩余边界见下节；不能据此标记整批完成。
5. 上述变更需要的契约同步、前端测试和真实联调。

以上为 PR #182 合入前的历史记录；其关联搜索分页现已合入并部署。

## 第二批续作（未完成整批）

### 工作区与缓存

- 后端：`/Users/darwish/Dev/tsz-core/.worktrees/backend/lexicon-domain-batch-2-completion`，基线 `c33b8b2`。
- 前端：`/Users/darwish/Dev/tsz-core/.worktrees/frontend/lexicon-domain-batch-2-completion`，基线 `3a1bd05`。
- 两仓均先 fetch，再从各自最新 origin/main 创建 `feat/lexicon-domain-batch-2-completion`；旧第二批目录已不存在，只剩 Git 登记，未清理登记或修改旧任务资源。
- Cargo 沿用用户级 `/Users/darwish/.cargo-target`，未 clean。前端安装自己的 node_modules，共享 pnpm store（642 包全部复用，未下载）。

### 本次已实现

- 成分搜索移除 generation、可用词条集合与 offset 绑定，使用稳定节点 keyset；不重做 PR #182 的关联搜索。
- 句中候选同样改为稳定节点 keyset，自动发现首屏可交给选中片段接口继续翻页。发布 ID 不属于分页身份。
- 保留查询/方言/片段绑定，不兼容旧 offset 游标。无关更新和归档不会使游标失效；total 为当前可见计数，不控制 next_cursor。
- API 字段形状不变，无迁移，无 SQL 查询变化；更新 OpenAPI 游标及 total 说明并同步前端生成物。

### 验证环境与中间失败

- 新建独立容器 `tsz-lexicon-b2-completion-pg` / `tsz-lexicon-b2-completion-redis`，标签 `task=lexicon-domain-batch-2-completion`。未复用或改动旧第二批容器及共享 5433/6379。
- 本轮 PG `127.0.0.1:57225/lexicon_b2`，Redis `127.0.0.1:57230/0`；SQLx 自动建测试库，显式设置 TEST_REDIS_URL 与 REDIS_URL。收尾已停止这两个本任务容器，未删除数据卷；恢复后须重新核对端口。
- 首次 HTTP 全量为 97 passed / 2 failed：新测试未使用同形词确认 fixture；旧测试仍假设成分搜索依赖 generation。修正 fixture，并将后者的 generation 缺失断言放到仍返回该元数据的句中发现接口。
- 新句中测试第二次因遗漏必填 include_drafts 得到 422；补齐合法请求，未改变产品校验。两项定向重跑通过，192 项 lexicon 单测及 clippy 通过。
- 最终后端验证：`lexicon_handler` 99 passed、`sentence_target_discovery_contract` 3 passed、`--lib lexicon::` 192 passed；clippy all-targets/all-features `-D warnings`、fmt、diff check 通过，退出码 0。日志：`/tmp/lexicon-b2-completion-final.log` / `.exit`。
- OpenAPI 已重导，解析对比证明去掉 description 后与基线完全一致；前端 runtime bundle 的 `_source` 指向本次后端工作区，SHA256 为 `38932a97455223f421bbeb7b2bf5a636cc92f2e119d54194c5037260e5573922`。同步更新契约测试中明确要求维护的输入 hash canary；首次同步后该 canary 失败已保留日志，未放宽断言。
- 最终前端验证：api-client 398 passed，typecheck、lint、diff check 通过，退出码 0。日志：`/tmp/lexicon-b2-completion-contract-final2.log` / `.exit`。未运行全前端测试或浏览器真实联调。
- 此子项后端共 294 项、前端 398 项通过，不重复计数。当前成果只证明分页与契约说明同步，不证明下列剩余引用规则。

### 仍阻断第二批验收

1. 草稿保存/发布引用边界仍未切换；不能孤立删除保存守卫。节点墓碑、发布引用完整身份及共享例句即时生效路径需要共同验证。
2. 草稿候选仍未覆盖已发布词条新增节点；发布/草稿目标不能共用只按 entry_id 索引的内容 map，否则互相覆盖。保存侧已有 probe 回落草稿能力，应复用。
3. 前端默认关闭草稿、主动展开及具体节点依赖标识、来源节点缺失反馈尚未实现。
4. 上述引用/候选契约同步、UI 与真实联调尚未完成。当前旧行为回归通过不代表新规则验收通过。

实现阶段收尾时，本续作没有提交、推送、PR、部署、旧数据兼容代码或远端数据修改。后续交付仅涵盖本分页子项及前端契约说明同步，不代表整批完成；实际提交、独立审查及 CI 状态以配套 PR 为准。

## 本地浏览器手工验收（2026-09-21）

仅验收已实现的成分分页链路，不代表第二批整体验收完成。

### 环境

- 使用上述两仓任务工作区。后台入口 `http://127.0.0.1:13001`，Vite 代理至 `http://127.0.0.1:18483/api/v1`；相关词库、词性、TTS 和本地语音 mock 关闭，未配置付费语音服务。
- 重启本任务专属 PG / Redis 容器后，实际地址为 `127.0.0.1:49751/lexicon_b2` / `127.0.0.1:49752/0`，非历史测试端口。本次仅在该隔离库执行迁移、创建验收管理员及测试词条。
- 后端运行的是本次工作区构建后复制到 `/tmp/tsz-lexicon-b2-completion-local/tsz-rust` 的独立二进制，仍复用共享编译缓存。服务保持运行，未重启其他任务服务。
- 版本、二进制哈希、PID、日志及私有本地凭据存于 `/tmp/tsz-lexicon-b2-completion-local/`，不提交凭据。主服务健康、就绪、前端代理登录及真实搜索探针均正常。
- 登录及业务数据编辑通过 Chrome 可见页面完成。computer-use 点击封装报错后使用系统鼠标/键盘操作配合截图；没有通过 API/SQL 批量造词或注入浏览器会话。

### 手工测试数据（保留供继续验收）

| 词条 | ID | 当前状态/用途 |
| --- | --- | --- |
| harbour | `01a0c2ef-0968-7bd2-9d52-562711ec6c75` | 已发布；名词、1 个词义、8 个同拼写独立原形及1个复数，专用于放大稳定节点候选集合，不作为真实词典内容 |
| harbour club | `01a0c2f7-a9e2-70a3-b177-40b0409b605b` | 已发布；其词义成分引用 harbour；刷新后仍显示保存/发布状态 |
| club | `01a0c314-f423-7161-b631-eead93d8f0a9` | 草稿；已保存 club/clubs，用于第一页与第二页之间的无关词面写入 |

隔离库初始缺少短语词性，已通过「系统设置 → 词性配置」页面新增名词性短语及可数名词短语细分类。没有复制远端字典或修改共享环境。

### 已观察结果

1. 页面创建、词形/发音/释义录入、必填失败提示、保存草稿、发布检查与发布走通；harbour 与 harbour club 发布后重新加载仍显示已发布。
2. harbour club 的成分选择器可查到 harbour，展开原形/复数及对应词义；选中并保存后刷新页面，成分仍为绿色已关联状态。未建 club 时其候选显示「没有匹配的词条」。
3. 手工增加独立原形并重新发布 harbour 后，成分选择器真实出现「加载更多」。保持第一页打开，在另一标签页创建并保存无关 club 词形草稿，再回来加载更多：未出现游标失效提示，原有词义选中状态保留，末页加载更多按钮消失。
4. 另一次浏览器 Network 采证中，通过页面点击触发首/末两页。用 Network 的 Copy response 复制响应到 `ui-page1.json` / `ui-page2.json`，只读核对为 50 + 14 = 64 个不重复的 `(entry_id, pos_id, base_form_id, matched_variant_id)`；首页带游标，末页 total=64、truncated=false、不带 next_cursor。这是实际页面请求，不是重发脚本或 mock 响应。
5. 操作中误加的一个空派生词行未保存；已通过刷新和丢弃本任务本地编辑备份恢复，随后 harbour club 发布成功。

### 首轮手工验收的覆盖边界

- 首轮没有逐项做跨页归档、排序键后插入及重新发布目标过程中的第二页浏览器复验；跨页归档的后续补测见下节。
- 首轮未做句中自动发现分页浏览器验收；后续完成的是真实接口验收页验证，不是正式业务页面验收。
- TTS 播放、学习端、未实施的引用规则与草稿候选新行为不在通过范围；不能据此声明所有分页并发场景或第二批整体验收通过。

## 补测收尾：跨页归档与句中分页（2026-09-21）

### 句中分页：真实接口联调通过

调查确认 `V3SentenceTargetDiscovery.tsx` 当前只有组件及测试，没有挂载到正式业务页面；不能把成分选择器当作句中自动发现页面。使用工作区之外的独立验收页 `http://127.0.0.1:13402/`，在页面内正常登录、点击发送真实后端请求；代理仅允许登录与 sentence-targets/resolve，未 mock、未注入会话或批量写入词条。

- `The harbour is calm.`，common，page_size_per_range=20：点击自动发现首屏，再以响应的区间和游标切换 selected_segments 续页，四次 HTTP 200，条数为 20 + 20 + 20 + 4；累计 64 个唯一稳定节点键，每页重复数为 0，末页无游标。
- 保留游标后分别改变方言、改变完整句子文本、使用非法游标，均返回 HTTP 400 / invalid_query / field=cursor。
- 保留归档前首屏的游标，通过正式后台归档匹配目标，再手动续页，返回 HTTP 200、0 条、total=0、无游标。独立验收页中途遇到一次 token 401；该临时页面没有自动 refresh，重新正常登录后沿用原游标成功，未将 401 记作分页通过。
- 证据：`sentence-normal.png`、`sentence-dialect-rejected.png`、`sentence-text-rejected.png`、`sentence-cursor-rejected.png`、`sentence-archive-next-reauth.png`，位于 `/tmp/tsz-lexicon-b2-completion-local/`。

### 成分跨页归档：正式页面流程通过

- 为避免其他 localhost 应用共享同名 Cookie/页面存储，续测入口改为 `http://lexicon-b2.localhost:13001/`，仍连接同一任务后端及隔离库；未改登录实现，不能视为登录异常已经修复。
- 直接归档被 harbour club 当前发布版本引用的 harbour 时，正式页面明确报引用冲突，保持原状态。先验证归档引用源后可归档目标，并随后恢复两者。
- 最终成分页面用例为避免归档引用源使编辑页失效，先通过页面临时清除 harbour club 的 harbour 成分引用并发布。重新打开该词义的 harbour 候选列表，第一页显示「加载更多」，保持这个标签页。
- 在第二标签页通过正式「移入垃圾桶」操作归档 harbour；切回原候选列表，点击「加载更多」成功结束，没有游标失效或加载错误提示，按钮消失。已加载的旧第一页候选仍留在弱一致搜索列表中；关闭再打开搜索，显示「没有匹配的词条」，归档目标不再从新请求返回。
- 本次匹配集来自同一个词条，归档后剩余结果为空。不能把这个手工用例扩大解释为已验证“归档后仍有其他词条的非空剩余页”；该情形的 HTTP 回归证据与手工证据分开。
- 最终完整截图：`archive-final-first.png`、`archive-final-list.png`、`archive-final-before-next.png`、`archive-final-after-next.png`、`archive-final-fresh-search.png`。之前被窗口切换、登录态中断的尝试均未算通过。

### 数据恢复与独立待查项

- 已通过垃圾桶恢复 harbour；重新选择其词义、保存并发布 harbour club，恢复测试成分引用。隔离库只读核对：harbour 与 harbour club 均 active=true、published=true；harbour club 当前发布快照有 1 条成分引用；club 保持 active=true、published=false。
- 恢复是业务状态恢复，不是抹掉测试历史；测试产生的发布版本与审计记录保留。证据 `archive-final-restored.png`、`association-restored-published.png`。
- 记录独立异常：后台多个标签页曾非预期回到登录页，导致保留的候选首屏被打断。用户确认没有其他人登录；根因未确认，不能归因为异地登录或简单声明正常过期。按用户要求暂缓调查/修复，不将独立验收页未实现 refresh 的 401 与该问题混为一谈。
- 这次补测只补已实现分页子项的证据，不代表第二批剩余引用规则已实施，也不是全项目全量回归。补测阶段没有提交、推送或部署。

## 第二批剩余功能本轮

### 隔离与授权

- 两仓任务分支：`feat/lexicon-domain-batch-2-remaining`。
- 后端：`/Users/darwish/Dev/tsz-core/.worktrees/backend/lexicon-domain-batch-2-remaining`，基线 `453bf13`。
- 前端：`/Users/darwish/Dev/tsz-core/.worktrees/frontend/lexicon-domain-batch-2-remaining`，基线 `294886a`。
- 原 completion 工作区仍有运行进程，保持源码、服务和测试数据不变；不清理旧 worktree 登记。前端新 worktree 自行安装依赖，未链接旧 node_modules。
- 本次只实现/测试；未提交、推送、合并、部署或修改远端数据。
- 独立测试容器 `tsz-lexicon-b2-remaining-pg` / `tsz-lexicon-b2-remaining-redis`，标签 `task=lexicon-domain-batch-2-remaining`；PG 本轮 `127.0.0.1:49708/lexicon_b2_remaining`，Redis 本轮 `127.0.0.1:49709/0`。仅本任务隔离库迁移和 SQLx 测试；同时显式指定 TEST_REDIS_URL / REDIS_URL，不使用 fallback。Cargo 保留共享缓存，无 clean。收尾已停止这两个本轮容器、未删除数据，恢复后须重新核对端口；旧 completion 两仓服务仍运行且工作区干净。

### 当前实现（不是整批完成）

- 关联搜索补已发布词条新增草稿词义；排除已发布词义重复进入草稿结果，前端合并同 entry 的结果并保留具体词义状态。
- 关联保存回显 `target_status` 按当前发布的具体 sense 判断；保存允许草稿依赖，关联词单独发布和验证拒绝未发布词义。发布失败无发布记录，目标发布后可用原源 revision 重试成功。
- 成分候选读取发布/草稿独立 map，支持曾发布词条新增节点；相同具体目标前端优先发布快照，草稿依赖标识到叶子，不仅标记整条词条。
- 关联词和成分默认关闭草稿，主动展开；成分切换范围丢弃旧页/游标及迟到响应，不修改已有关联。
- 来源节点缺失给出明确反馈；步骤重定向及继续编辑保留 `focus_node`，同页变更定位参数可重新处理。仅提示，不宣称引用已解除。
- OpenAPI、前端快照和 runtime bundle 同步。本轮 wire 字段形状不变；runtime 来源指向本轮后端，SHA256 `b941455280e03417fc67ef0736d78b3b8a74f37b541a30f08140a372a2bc28c1`，canary 同步更新。

### 验证结果与中间失败

- 缺失来源节点回归先红后修复。步骤跳转测试最初误认为落在 forms，实际 fixture 已满足 forms，按现有规则落在 meanings；修正测试预期，未改变步骤规则。
- 后端第一轮：98 passed / 2 failed。新自定义错误码不在 V3 闭合集合，改为复用 `relation_target_unavailable`；另发现回显按词条判发布，已改成按具体 sense。
- 第二轮：99 passed / 1 failed。新增修复用例误将只读关联快照字段回传，修正 fixture 为合法 writable 内容；定向重跑通过。无放宽契约或断言。
- 首轮前端全量 2893 passed / 4 failed / 2 skipped：三项关联选择器/例句编辑器测试仍断言默认 include_drafts=true，按新规则改为 false；一项无关 `V3PublicationHistory` 异步详情断言失败，未修改其实现/用例，同码定向与第二次全量均通过，根因未确定，保留首轮日志。

候选子项检查点验证（均退出码 0）：

| 范围 | 结果 |
| --- | --- |
| 后端 `--lib lexicon::` | 192 passed |
| 后端 `lexicon_handler` / `lexicon_v3_lifecycle` / `sentence_target_discovery_contract` | 100 / 6 / 3 passed |
| 新增发布前验证断言后的定向重跑 | 1 passed，原 100 项之一，不重复计数 |
| clippy all-targets/all-features `-D warnings`、fmt、diff check | 通过 |
| 隔离库 migrate / `cargo sqlx prepare -- --all-targets --all-features` / `prepare --check` | 通过，`.sqlx` 无差异，无新增 migration |
| 后端 OpenAPI 导出及前端同步 | 通过；解析后去掉 description 与基线完全一致 |
| 前端 `pnpm test` | 192 文件、2897 passed、2 skipped |
| 补充“刷新后仍显示具体草稿依赖”的前端用例后，整个词义步骤测试重跑 | 110 passed（此前为 109 项），不是另一次全量 |
| 前端全仓 typecheck / lint；最后新增用例后 admin typecheck / 定向 eslint | 通过 |

日志与退出码：`/tmp/lexicon-b2-remaining-back-checks.*`、`back-final.*`、`back-incremental.*`、`frontend-final2.*`（后三者同为 `/tmp/lexicon-b2-remaining-` 前缀）；前端增量日志 `/tmp/lexicon-b2-remaining-ui-incremental.log`。前端首轮失败保留于 `/tmp/lexicon-b2-remaining-frontend-final.log`，后端中间失败保留于 `handler.log` / `handler2.log`（同前缀）。

以上证明当前子项，不证明下列尚未实施的边界；没有把旧行为回归通过当作第二批验收通过。

### 该检查点剩余范围

当时尚未切换引用影响/草稿删除边界、其他引用类型的发布有效性，以及句中发现契约和真实联调。引用边界现已继续实施，最新状态如下。

## 引用边界续作

**整批仍未完成；未提交、推送、PR、合并或部署。沿用上述 remaining 两仓工作区，旧运行工作区没有改动。**

### 已实施

- 入站发布引用不再只校验 sense。读取不可变来源快照补全成分、正文链接和 via-phrase 身份；固定发布号的草稿成分也纳入影响。
- 新增草稿正文引用采集和 `draft_text_link` 来源类型，直接读取编辑投影，复用原判定/来源描述/节点计数；没有新增引用表或兼容分流。
- 草稿允许删除被引用的词形、词义、词性；保留稳定身份、墓碑、乐观版本、事务与外键保护。前端撤销引用导致的删除/保存禁用，保留结构限制、确认及引用影响提示，不把草稿内失效说成当前发布已受损。
- 单独发布与发布前验证、历史激活、恢复均检查当前发布中的具体出站目标。关联、成分、正文和 context 不允许以未发布节点完成发布。已有批次恢复先在事务内恢复所选项，再校验完整目标，失败回滚；没有实现第三批批次发布/新版本回退。
- 共享例句仍按现有即时编辑及显式 unlink 路径工作，入站影响使用词形/方言/词面规则；正文人工关联使用完整节点身份，不套用共享例句词面等值限制。第四批独立例句版本未扩展。
- 后端闭环回归：保留 sense、删除被引用的具体词形 → 草稿保存成功、当前发布候选不变 → 发布被拦且版本不变 → 来源仅保存修复仍被旧发布引用拦截 → 发布来源修复后目标可发布。验证墓碑、历史快照保持，以及历史激活不能绕过当前节点校验。
- 额外验证内部专用组绑定的词义可从草稿删除，绑定关系清除、节点墓碑保留，但不完整草稿仍不可发布。

### 契约与中间失败

- 新来源枚举已同步后端 OpenAPI、前端类型、runtime schema 和标签，新增解码回归同时验证接受新类型、拒绝未知类型。最新输入 SHA256：`08641a4e6c23e73e753bb0ee62994cb96a780b8c3117adbf79ef05fba038a652`。
- 这次不再是仅 description 更新：旧严格前端会拒绝 `draft_text_link`，配套发布必须按既定停流方案执行。本轮无 migration，SQLx prepare/cache check 无差异。
- 首轮旧行为测试按预期在“保存必须冲突/删除必须禁用”断言失败，已替换为草稿可保存、引用保留、发布拦截及修复闭环；没有删掉引用/墓碑断言来放行。
- 共享例句测试旧 fixture 原来在保存前被守卫拦截，进入真实保存后暴露缺失父子身份/entry_pos 关系行，补齐合法 fixture，未放松生产身份与外键校验。调试日志代码已清除。
- 出站统一检查的初版误将正文人工关联套用例句词面规则，既有正文回归识别后，拆回不同业务判定、共用生命周期检查。
- 前端全量的旧文案断言及一个级联器异步展开立即查询断言失败；同步新提示文案，后者改为等待出现后仍严格断言禁用和不可选。保留失败日志，不把首次失败抹去。

### 验证

- 本轮复用自有容器，恢复后端口实际为 PG `127.0.0.1:60022/lexicon_b2_remaining`、Redis `127.0.0.1:60023/0`；未连接旧 completion、共享或远端实例。收尾仅停止本轮两个容器、保留数据，恢复须重新核对端口；旧 completion 服务仍运行、两仓工作区仍干净。
- 后端 `lexicon_handler` **102 passed**；`shared_sentences` **24 passed**；`lexicon_v3_lifecycle` **6 passed**；`sentence_target_discovery_contract` **3 passed**；`--lib lexicon::` **192 passed**；`--lib openapi::` **10 passed**。重复定向重跑不另计。
- 前端全量 **192 文件、2899 passed、2 skipped**；全仓 typecheck、lint 通过。
- clippy all-targets/all-features `-D warnings`、fmt、两仓 diff check、SQLx prepare 与 prepare --check 通过。
- 最新证据：`/tmp/lexicon-b2-reference-handler-final.*`、`final4.log`（handler/lifecycle/discovery，shared fixture 的失败保留）、`final5.*`（shared/lib/clippy/prepare/export）、`schema-final.*`（OpenAPI/cache check）、`front-final3.*`（全前端）；这些文件均使用 `/tmp/lexicon-b2-reference-` 前缀。

### 该检查点之后

用户已确认继续接入现有例句编辑入口。原先待办的句中节点契约、页面接线及真实浏览器补验已继续推进，当前结果如下。

## 句中发现与本轮验收收尾

**本轮剩余功能实现和约定验证已收尾；按用户要求，第二批整批状态不标记完成。未提交、推送、提 PR、合并或部署。**

### 实现

- `sentence-targets/resolve` 的 `draft_matches` 改为完整词形/词义节点候选，增加 `draft_total`，移除旧 entry-only / pending-only 元数据模型及创建者限定查询。其他管理员可发现目标，但 HTTP 回归确认仍不能编辑他人草稿。
- 已发布词条新增节点可以主动展开；按具体匹配词形、原形、变体及词义可用范围排除已发布的同一组合，不重复把已发布节点标为草稿。
- 复用原稳定键分页，发布与草稿共用页容量、已发布优先，游标绑定 include_drafts 范围；切换范围不可沿用旧游标。原分页算法的弱一致、插入/归档、自动转手动续页回归仍通过，不重新实现已部署分页子项。
- `V3SentenceTargetDiscovery` 接入共用 `SentenceEditor`（现有词义内/例句库编辑区，不新增页面）。自动默认仅发布；手动显式展开草稿。查询不写入，选择只改本地编辑内容，点击“完成”才保存。
- 适配器保留完整稳定身份，词形与词头分开；与手动选择共用标注写入函数。保留原标注、当前词义关联要求、文本/方言变化取消旧结果和请求的保护。
- 新契约和类型、运行时校验、生成脚本根类型清单全部同步；旧草稿结果明确拒绝而非兼容。当前 spec SHA256：`93d87d405f4957c9744b585f3eb07cf55b2e5674f80b91a19183bc2866e116fa`。

### 最终自动检查

全部退出码 **0**，不同层的重复重跑不另计：

| 检查 | 结果 |
| --- | --- |
| 后端 lexicon_handler / shared_sentences / lexicon_v3_lifecycle / sentence_target_discovery_contract | 102 / 24 / 6 / 3 passed |
| 后端 lexicon 单测 / OpenAPI 单测 | 192 / 10 passed |
| clippy all-targets/all-features `-D warnings`、fmt、SQLx prepare --check | 通过；`.sqlx` 无变化 |
| 前端全量 `pnpm test` | 193 文件，2906 passed，2 skipped |
| 前端全仓 typecheck / lint、两仓 diff check | 通过 |

日志及真实退出码：`/tmp/lexicon-b2-close-back.log` / `.exit`、`/tmp/lexicon-b2-close-front.log` / `.exit`。

同步曾因生成脚本仍登记已删除的草稿元数据 schema 而失败；移除对应旧根类型后重新生成成功，未跳过缺失 schema 校验。记录保留于 `/tmp/lexicon-b2-discovery-sync.log` / `sync2.log`。首次收尾格式化命令超时后单独完成格式化，再启动最终检查，没有把未启动的测试算作通过。

### 真实浏览器证据

- 新隔离环境：前端 `http://lexicon-b2-remaining.localhost:13002`，真实代理到本任务后端 `http://127.0.0.1:18584/api/v1`。后端是本工作区构建后复制的独立二进制，版本/哈希/PID 见私有目录 `/tmp/tsz-lexicon-b2-remaining-local/manifest.json`。没有替换旧 completion 服务。
- 本轮实际依赖：PG `127.0.0.1:57198/lexicon_b2_remaining`、Redis `127.0.0.1:57199/0`。维护库开始时词条数为 0；本轮只创建隔离验收账号和合成词条/例句，不触碰共享或远端数据。
- 浏览器正常登录，使用后端原生本地 OTP Mock 通道；词库、词性和 TTS 前端 mock 关闭，不配置付费语音服务。Playwright 新建独立 Chrome 上下文，无 page.route 拦截、无注入登录 token；API 仅用于 fixture 准备和独立读回验证，受测查询/保存/修复/发布均由正式页面点击发起。
- 已验证：自动发现 ledger 的已发布复数目标；主动展开同词条新增草稿词义，显示具体未发布依赖；选择后数据库未提前改变，点击完成才保存。独立 GET 与页面刷新确认具体 sense/form/variant，草稿绑定未误带旧发布号。
- 已验证：正式词形页删除被引用的复数并保存草稿；当前发布版本不变；发布检查返回 409 和 2 条可定位例句引用；在两条例句正式编辑器中清除对应关联、保存，保留 notebook 自身关联；目标随后通过检查并发布，旧发布快照仍保留被删词形。
- 原生页面首次脚本在第二例句的 Radio 控件上选中了隐藏 input，尚未发出该步骤请求；改点可见标签，并核对第一条例句已保存、第二条未变后，从检查点继续，没有重放已完成写入。失败日志/截图保留。
- 证据：`automatic-response.json`、`draft-expanded-response.json`、`reference-publication-rejected.json`、`browser-resume-result.json`（passed=true）；截图 `discovery-published-selection.png`、`discovery-draft-dependency.png`、`discovery-reloaded.png`、`reference-draft-deletion-saved.png`、`reference-publication-blocked.png`、`reference-repair-published.png`，均在上述私有目录。凭据未写入仓库或报告。
- 合成 fixture：ledger `01a0c750-abd9-7cb3-9a93-c51fd1d9676c`、notebook `01a0c750-ace0-7f22-bda5-4cbaebbd04ad` 和两条例句。最终 ledger 已发布删除复数后的版本；两条例句仅保留 notebook 关联，测试历史保留。没有修改旧验收 fixture。

### 交付边界与未覆盖项

- 旧 completion 两仓源码和运行服务保持原样；本轮环境与数据保留供复验，不迁移/清理旧 worktree 或共享缓存。
- 浏览器证明的是上述共享例句闭环；词条来源修复后须发布来源、历史激活/恢复完整目标检查有 HTTP 回归，不宣称每一种组合都做过浏览器操作。
- 未执行全项目 E2E 或大型真实词库性能基准；第三至第五批功能没有顺带实施。
- 新来源枚举、必填 draft_total、草稿候选结构变更需要配套受控发布，旧严格前端不能混用。上述验收阶段未执行交付或远端发布。

### 交付授权补记

用户随后明确要求提 PR，授权本任务两仓提交、独立审查、推送和创建配套 PR。继续复用 remaining 工作区，不合并、不部署，不将第二批整批标记完成；精确提交、独立审查和 CI 结果以配套 PR 为准。

交付独立审查发现并修复：当前草稿内部专用组绑定不依赖当前发布快照，发布/历史激活检查不应把它当作外部入站依赖；仅草稿影响预览收集该类内部绑定，外部引用检查保持完整。新增回归先复现历史激活 409，再验证激活成功且草稿内容、revision 和绑定不变；另重跑外部引用阻断与修复回归。前端同时补齐明确关闭句中发现能力时隐藏入口的回归，例句编辑保留。以上增量交由原独立 reviewer 复查。
