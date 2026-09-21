# 实施进度

## 状态

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
