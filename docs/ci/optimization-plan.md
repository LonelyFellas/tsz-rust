# Rust 后端 CI/CD 对标评估与优化方案

评估日期：2026-11-03（Asia/Singapore）。本项目基线：`4e13630c01aebc4833457e47605d5fe337c024e4`。
当前阶段：第 1 批优化已随 #414 合并，main `8d60adf` CI 全绿，墙钟 16m10s；第 2 批的 PR #415（`7a45450`）全绿，单次 PR 墙钟 13m55s；继续在该 PR 验证第 3 批的静态文档路由复用。本文是 CI 优化的唯一主记录，复用
[集成测试分片设计](integration-sharding.md)和[部署控制器说明](deployment-controller.md)，不另建发布体系。

## 2026-11-14 第 3 批：静态文档路由复用与无依赖健康检查

沿用未合并的 PR #415，直接对照成功提交 `7a45450` / PR CI
[43575425865](https://github.com/LonelyFellas/tsz-rust/actions/runs/43575425865)，
墙钟 13m55s，admin / identity / learning 的执行阶段各约 9 分钟。

开源实现核对见 [Svix 的集成测试统一入口](https://github.com/svix/svix-webhooks/blob/bb44d9a55f2bd409cb502d8bdab6b2e28710846c/server/svix-server/tests/it/main.rs)
与 [Kanidm 对独立测试二进制成本的说明](https://github.com/kanidm/kanidm/blob/e201a7240d2c5fbf6668336aa988c22d5c2681d5/server/testkit/tests/DO_NOT_ADD_MORE_TESTS_HERE)。
本批先处理有直接测量的共性成本，暂不重组 103 个 target 或缩小 PR 用例范围。

代码显示 `router(state)` 在 swagger feature 下每次调用 `ApiDoc::openapi()`，
而多个 handler helper 每次请求都重建 Router。使用临时 manifest 注册探针，
不连接数据库/Redis，debug/all-features 下连续构建 40 个 Router 用时 9.942s，
单独构建 40 份 OpenAPI 用时 9.712s。该本地单次观测支持优化这个重复工作，
不能作为 GitHub runner 墙钟的直接预测。复用后同一探针构建 40 个 Router 用时
0.552s（包含首次文档初始化），同时独立生成 40 份 OpenAPI 仍为 9.695s；未通过
跳过 schema 生成或更换输入来改变对照条件。

方案：只将无 AppState 的 SwaggerUi 路由存入 `OnceLock<Router<AppState>>`。
utoipa-swagger-ui 10.0.2 的 Axum 适配器已将文档放在 Arc 中，Router clone
不会深拷贝 schema。业务路由、鉴权/本地化/请求 ID 中间件与每实例状态仍在缓存之外
重新装配。发布默认 feature 不包含 swagger；独立 `ApiDoc::openapi()` 调用仍是新文档，
用于快照/契约验证与导出。

`healthz` 与 Swagger 静态资源测试改为 Tokio 测试，使用指向关闭端口的惰性依赖，
验证这些公开静态端点无需活数据库/Redis；原响应、请求 ID、静态资源与完整 OpenAPI
快照断言保留。新增两套不同语言 AppState 的交错请求，验证不同 Router 的状态和每请求
request ID 隔离。就绪检查及所有业务数据库/并发用例继续按原路径运行。

本地同环境对照（执行墙钟，不含编译）已通过：

| target | 旧版 | 文档路由复用后 | 保留用例 |
| --- | ---: | ---: | ---: |
| account_security_handler | 13.02s | 6.91s | 25 |
| admin_permissions_handler | 9.64s | 4.83s | 19 |
| learning_runs_handler | 94.99s | 23.42s | 39 |

原 83 个业务用例不变；all-features health 的 3 个用例（含文档快照与状态隔离）通过，
默认 feature 的 2 个 health 用例也通过，分别执行 0.35s / 0.04s。
完整 PR CI 的执行清单应保留历史 1372 个用例并增加 1 个隔离回归；所有分片、契约
快照与原门禁均需通过。最终结果记录在 PR，避免仅为回填数据再触发一次全量 CI。

## 2026-11-14 第 2 批：分片再平衡与登录 Router 复用

基线为 #414 合并后的 main `8d60adf`，成功运行
[43571328212](https://github.com/LonelyFellas/tsz-rust/actions/runs/43571328212)，
墙钟 16m10s。紧邻的合并前 main 运行
[43566354447](https://github.com/LonelyFellas/tsz-rust/actions/runs/43566354447)
为 16m24s。首次 #414 PR 的 13m33s 不能作为稳定收益；累计生命周期计时包含并行
等待，不能称作 CPU 时间，也不能直接相加推导墙钟。

分页 job 本次编译 234s、执行 665s；identity 编译 166s、执行 634s。
按完整执行报告中的 `target_timings`，两次相邻 main 的串行执行分布为：

| target | #413 main | #414 main |
| --- | ---: | ---: |
| 分页用例（lexicon_handler 过滤） | 58.9s | 58.4s |
| wordlists_handler | 233.2s | 236.5s |
| question_bank_handler | 204.7s | 205.6s |
| shared_sentences | 162.8s | 164.5s |
| auth_login_handler（identity） | 108.0s | 148.6s |

本批保持 8 个 matrix job，把完整 `question_bank_handler` target 从分页分片移到
platform，把 `shared_sentences` 移到 lexicon-other；分页保留原用例与 wordlists。
原逻辑模块归属仍是 lexicon，Cargo locked/all-features 配置和独立依赖服务一致。
按 #414 的已有样本，新分页执行工作量为约 295s，而非原 665s；移入的两个分片需
重新实测编译与执行，不能将历史 target 耗时直接等同新 job 耗时。

identity 的两个 IP 预算用例包含 101/205 次请求，原 helper 每次重新构建完整 Router。
复用本文件已有并发登录用例的做法，在每个测试中构建一次 Router，逐请求 clone，
继续执行完整中间件、真实密码校验和真实 PostgreSQL/Redis。保留全部 14 个用例、
请求参数、循环次数、限流阈值、错误与数据库断言，各用例独享 Router/AppState。

执行审计已用 #414 的成功 main 报告重放新分片归属：103 targets / 1372 cases / 8 shards，
执行结果（含忽略集合）多重集完全相同。增加原归属残留/双归属的拒绝验证；本地登录
对照使用任务专属 PostgreSQL 16 / Redis 7 容器，连接全部显式指定，不使用共享实例。
本地同命令 `cargo test --locked --all-features --test auth_login_handler -- --show-output`
在修改前后均为 14 passed / 0 failed / 0 ignored，libtest 执行墙钟 55.53s → 8.47s
（排除编译，非 GitHub runner 性能承诺）。受影响 Python 检查 70 项通过。
本批未调整 timeout、编译缓存、模板克隆白名单或分支保护。

首轮 PR CI [43573896120](https://github.com/LonelyFellas/tsz-rust/actions/runs/43573896120)
在 handler-b 的模板准备阶段失败（job `132383899310`，`template connections remain open`），
尚未开始执行用例。`source.close().await` 发送 Terminate 后，PostgreSQL 仍可能短暂保留
backend；原 helper 立即读取 `pg_stat_activity` 并断言为零，存在退出竞态。
改为最多等待 2s、每 20ms 检查一次，SQL 错误直接失败，超时仍拒绝使用模板，不终止
任何会话。模板等价/隔离验证增加真实活动连接的拒绝、连接未被终止（ping）及关闭后
排空验证，作为每次 prepare 的运行检查。本地通过临时 manifest 注册真实 helper binary，
在隔离 PostgreSQL 16 下连续完成 3 次 prepare / live-session 拒绝 / drain / cleanup，
每次模板等价、数据/DDL/序列隔离、dirty/checksum 拒绝及连接排空检查均通过。

## 2026-11-14 第 1 批（已合并 #414）

根因证据来自成功 main 运行 [43557475052](https://github.com/LonelyFellas/tsz-rust/actions/runs/43557475052)
（`#410`，attempt 1，wall 22m18s）。该次 `lexicon-handler` job 独占 1303 s
（编译 223 s、执行 1036 s），其余五个集成分片在 470–650 s 后空闲；`lexicon_handler`
一片 194 个用例，按 SQLx 生命周期统计累计 4121 s 用例耗时、migration 744 s（每用例 3.8 s）。
同一批 job 各自冷编译（`cache-targets: false`），`Dependency audit` 的
`cargo install cargo-audit` 占 198 s。

| 批次 | 改动 | 目标 |
| --- | --- | --- |
| P0 | `lexicon-handler` 与 `lexicon-handler-b` 共享同一可执行文件，按用例名交替二分；`batch5` 固定在 profiling 片 | 关键路径约减半 |
| P1 | 两个 lexicon 分片复用 identity 已验证的迁移模板克隆，无逐用例白名单，审计要求每个用例恰好克隆一次 | 再减 migration 累计耗时 |
| P3 | 缓存 `cargo-audit` 二进制；`ci_run_summary` 报告 900 s 墙钟预算 | 省机时；超预算可见但不改门禁 |

未实施：`cache-targets: true`。单个 debug/all-features target 通常 3–8 GiB，超过仓库
10 GiB Actions 缓存额度，且会淘汰现有 registry 缓存，恢复/解压成本也落在关键路径上。
需要进一步压缩墙钟时，先看本批实测的分片与 phase 列，再在「更大 runner」与「再拆一片」
之间按真实成本选择，不用未验证的缓存替换已验证的机制。

验收：PR CI 必须自证每个集成用例恰好执行一次、克隆审计完整、两个 handler 分片都通过；
实测的分片墙钟、编译与 phase 统计记录到 PR，不预先宣称达到 15 分钟。本批未测量远端
结果前，不修改既有 timeout，也不放宽任何门禁。

## 2026-11-05 工程治理第 2 批

本批属于[同一工程治理任务](engineering-governance.md)。基线 `fb0575d019e6eb98d722ee85279805deeb034a56`，
仅本地实施与验证，未提交、推送、重跑或触发远端 CI。实际分片说明路径为
`docs/ci/integration-sharding.md`。既有 Router 复用、unit/doc 依赖缓存、七分片再平衡、Quality/audit
并行、严格文档快路径、私有 SQLx 插桩、identity 模板与执行审计全部复用。

### 近期失败分类

只读检查最近 35 次 CI 运行及最近 10 次 failure 的 job；下表列出有原始 job 日志支持的代表问题。
汇总 check 的连带失败不重复计为根因；日志中故意注入并被测试捕获的 panic 也不算失败。
latest attempt 成功不覆盖历史失败，特别保留 `41574741920 / attempt 1`。

| 分类 | 精确 run / attempt 与原始证据 | 当前状态 |
| --- | --- | --- |
| 迁移版本冲突，非 SQLx 离线元数据 | [40991311944 / 1](https://github.com/LonelyFellas/tsz-rust/actions/runs/40991311944/attempts/1)，job 123854130189：`_sqlx_migrations_pkey`，重复版本 `20261030010000`；多个分片在迁移准备阶段失败 | `24f549e` 已修正版本；当前 main 全量通过，不重复修改 |
| 回退测试 fixture 与新表依赖不一致 | [41208450239 / 1](https://github.com/LonelyFellas/tsz-rust/actions/runs/41208450239/attempts/1)，job 124558249744/124558249786：`learning_question_audio` 外键阻止 down，以及 fixture 缺表 | `14e6610` 已对齐回退 fixture；当前 main 通过 |
| 路由/权限契约断言 | [41004376922 / 1](https://github.com/LonelyFellas/tsz-rust/actions/runs/41004376922/attempts/1)，job 123895814537：退役 question-bank 路由与 policy/OpenAPI 集不一致 | 后续主线已通过相同单元检查；不修改产品契约 |
| 语音回归断言 | [41185072193 / 1](https://github.com/LonelyFellas/tsz-rust/actions/runs/41185072193/attempts/1)，job 124490239427：两个 speech_generated 用例失败，包含 `SpeechVoiceNotFound`；不是编译错误 | 后续主线 platform 已通过；本批不改语音行为 |
| 测试准备/超时边界 | [40461681990 / 1](https://github.com/LonelyFellas/tsz-rust/actions/runs/40461681990/attempts/1)，job 122047518765：`audit_failure_and_deadline_after_reward_roll_back_everything` 失败 | `91709ee` 已将 query timeout 限于受测操作 |
| 计时完整性审计 | [40814493015 / 1](https://github.com/LonelyFellas/tsz-rust/actions/runs/40814493015/attempts/1)，job 123252279235：`router pilot changed the request inventory or scope` | `1e2a904` 已将真实音频审核请求纳入计数，审计继续严格 |
| job 执行预算 | [41574741920 / 1](https://github.com/LonelyFellas/tsz-rust/actions/runs/41574741920/attempts/1)，job 125715518564；[41559848390 / 1](https://github.com/LonelyFellas/tsz-rust/actions/runs/41559848390/attempts/1)，job 125667569873：约 20 分钟取消，不能误记为断言失败；前者最新 attempt 2 成功 | #380、handler 预算调整及 #384 再平衡均已在主线；不再放宽 timeout 或增加重试 |

上述近期失败样本没有独立的编译/链接、SQLx 离线缓存缺失或 Clippy 根因。第 1 批发现的本机历史
SQLx 连接/字段/类型/lint 错误另见治理记录，缺少原命令和 SHA，不能混作 GitHub CI 当前故障。
未找到本批需修复的现存断言失败；未解决的是 handler 长尾及 runner 波动，不能将所有变慢归因于缓存。

### 当前关键路径和最小补丁

以下均为 attempt 1、完整成功、相同现有七分片机制；PR/main 源码不同，不作为严格性能对照。

| run | handler 编译 / 执行 / job | identity 执行 | unit/doc job |
| --- | ---: | ---: | ---: |
| [41590951618](https://github.com/LonelyFellas/tsz-rust/actions/runs/41590951618)（main f036a94） | 227 / 949 / 1218 s | 748 s | 231 s |
| [41634254288](https://github.com/LonelyFellas/tsz-rust/actions/runs/41634254288)（PR 37184db） | 210 / 866 / 1115 s | 380 s | 215 s |
| [41640669021](https://github.com/LonelyFellas/tsz-rust/actions/runs/41640669021)（main fb0575d） | 228 / 979 / 1251 s | 746 s | 176 s |

最新 handler job `125933129463`：4 vCPU、AMD EPYC 7763、image `20261026.354.1`、Rust 1.98.0。
源码依赖缓存命中，下载 384180914 B（约 366 MiB），仍需 228 秒编译；命中不等于复用了 target。
该 job 的 cache restore 为 7 秒，unit/doc restore 为 11 秒，两者 post cache step 均为 0 秒；
阶段是 GitHub step 的秒级时间，不能把 post cache 错算作测试或遗漏后声称净收益。
unit/doc 继续原有受限依赖缓存，不扩展到 SQLx 私有 patch 或 release。缓存开销和机器差异仍须
随实际 job 解释，不由一次 wall time 保证收益。nextest、增加 runner、重试和迁移业务断言均不采用。

最新逐例计时中两个最慢 body 为：

- `sentence_formatting::sentence_formatting_negotiates_reads_and_protects_legacy_writes`：48.753 s。
- `spelling_markup::markup_negotiation_preserves_cached_publish_history_and_lifecycle`：42.296 s。

两者此前没有进入既有 `with_test_router` scope。补丁仅将这两个测试的原 body 放入同一 AppState
作用域，并调用既有 `assert_single_router`。保持每次真实 HTTP 请求、能力协商、缓存回放、历史读取、
发布/回滚、数据库重读和全部原断言；无跨测试 Router 缓存，无产品 src/API/SQL/迁移改动。
这些 body 时间包含并发资源竞争，不可相加当作 CI 墙钟收益。

### 本地对照与验收

Apple M5 Pro / macOS arm64 / Rust 1.98.0；共享 target，`jobs=2`、incremental=false，
沿用现有 dev/test profile、all-features、SQLX_OFFLINE=true，没有新增 profile/flags 或关闭断言。
独立 PostgreSQL 16.15、独立 Redis，显式传 DATABASE_URL/REDIS_URL/TEST_REDIS_URL；
音频为既有 MemoryAdapter，不调用 OSS 或付费服务。两个用例在相同实例上串行运行，
每次 SQLx 用例保留独立数据库及原迁移路径。前后保留二进制用于复测，计时排除编译。

| 顺序 | 两用例进程墙钟 | 结果与可比性 |
| --- | ---: | --- |
| baseline 1 | 19.014 s | 2 passed；与约 2 秒的 Python 工具测试短暂重叠，仅预热，不用于独占对照 |
| baseline 2 | 17.501 s | 2 passed；无其他任务 Cargo/rustc 或本任务工具测试并发 |
| candidate 1 | 2.870 s | 2 passed，libtest 内部 1.73 s；含进程启动成本 |
| candidate 2 | 1.625 s | 2 passed，libtest 内部 1.61 s |
| baseline 3（保存的原二进制） | 16.582 s | 2 passed；证明不只是后测受益于数据/OS 预热 |

candidate 两次都只有各 1 次 Router 构造，句子格式测试 40 个独立 request ID、拼写标记测试 33 个。
两个源码文件移除新增 scope/统计检查后与 HEAD 原文逐字节相同，原测试 body 和断言未变。
编译分别为 65 秒和 22.18 秒，均复用本机已有依赖缓存；后者不是纯代码加速证据，不能把它计入
测试执行收益。以上仅为小范围本地测量，Linux 私有 SQLx 插桩及整体 CI 净收益**待 PR CI 验证**。

验证命令（本地连接变量必须指向任务独立服务）：

以下命令已在第 1 批复验后更新为原生 Cargo；历史实际执行使用的空间门禁包装已撤回，
不改变当时 Cargo 参数或测试结果，本次没有重跑这些 Rust 检查。

```bash
SQLX_OFFLINE=true cargo test --locked --offline --all-features --test lexicon_handler --no-run --message-format=json
# 从 compiler-artifact 消息取得本次 lexicon_handler executable，分别执行：
<executable> negotiat --test-threads=1 --show-output
<executable> batch5::scoped_router_ --test-threads=1 --show-output
<executable> --list
<executable> --list --ignored
python3 -B -m unittest ops/test_ci_test_shards.py ops/test_ci_test_modules.py ops/test_ci_identity_profile.py
GITHUB_SHA=fb0575d019e6eb98d722ee85279805deeb034a56 python3 -B ops/ci_test_shards.py verify <exact-main-inventory-directory>
SQLX_OFFLINE=true cargo clippy --locked --offline --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
git diff --check
```

- 两个修改用例及两个既有 scope 回归通过：覆盖不同 AppState、嵌套 scope 恢复、Swagger 和逐请求鉴权撤销。
- Python 分片/发现/私有插桩工具回归 31 项通过。测试输出中故意构造的 FAILED 用于验证审计拒绝路径，unittest 总结果为 OK。
- 前后 `lexicon_handler` 的全部 190 个用例及 ignored 清单逐项相同；七分片调度/过滤代码未改。
- 从 run `41640669021 / attempt 1` 下载精确 inventory artifact ID：13233986225、13233695931、
  13233508786、13233156855、13233013488、13232689317、13232632916；按当前 target 清单和源 SHA
  独立复算原审计通过：100 targets、1317 cases、7 shards（1314 ok、3 既有 ignored，无重复/漏跑）。
  这是主线执行证据；本地候选只执行受影响用例，不将主线清单冒充候选全量运行。
- 最终 all-targets/all-features Clippy 通过（65 秒），fmt 和 diff 检查通过。未改运行时 SQL，
  无需重新生成 `.sqlx`、OpenAPI 或迁移；未运行候选全量 Rust/CI/部署验收。
- 批次 `tsz-ci-batch2-20261105-ewo2ph5g` 清理完成：核对 PG data-dir 和 Redis dir 后停止两项
  专属服务，确认 PID 消失、端口关闭，再删除专属目录中的数据库、缓存、原始日志、inventory 下载、
  baseline/candidate 二进制副本；目录复查不存在。保留共享 Cargo target 和既有服务，未写远端资源。
  本批全部写入/编译结束后交给第 3 批，不承诺 15 分钟 CI SLA。

## 结论与范围

本项目的主要短板是测试准备成本、重复编译、分片长尾和性能证据闭环；部署来源验证、
独立测试环境和执行完整性审计已经较完善。推荐先优化现有机制，再依据对照结果选择 nextest。
不把增加 runner、迁移到 nightly 或启用自动部署作为默认答案。

目标是缩短 PR 的完整反馈时间并控制 runner 消耗，不改变生产 API、SQL 迁移或业务行为，
不删除回归断言、不增加 ignored、不以重试掩盖失败，不降低已有覆盖率门禁。
没有现成的跨项目“合理分钟数”：公开仓库运行只能用于理解工程取舍，不能作为同负载基准。

## 本次交付结果（2026-11-03）

代码合并：[#381](https://github.com/LonelyFellas/tsz-rust/pull/381)、
[#384](https://github.com/LonelyFellas/tsz-rust/pull/384)，实现主线为
`200b69bfa07a8e24089bfe8ebaaf600af96515ce`。草稿 #382/#383 的全部提交收拢到 #384，未丢弃。
以下是已观测结果，不是未来运行时长保证；后文保留原方案和分阶段记录，以本节及最终 PR 验收记录为准。

| 阶段 | 已交付结果 | 验证与取舍 |
| --- | --- | --- |
| P0 | 精确 run/attempt 的阶段耗时、最长 job、末端反馈与未加权 runner 成本；重跑复用 job 单列；失败诊断与成功清单分离 | 历史真实重跑复算和新 CI artifact 均验证。created-to-start 不是纯排队时间，最长 job 也不冒充完整因果 DAG |
| P1 | 分页及14个已测慢 handler 的单测试/单 AppState Router 复用 | 保留真实请求与全部断言。分页真实CI约57～59秒，原历史样本约9～12分钟；本地代表handler15.90→2.84秒 |
| P2 | unit/doc 编译依赖缓存；不缓存workspace crates，仍仅受信main push且job成功保存 | 同SHA冷/暖为331→162秒，暖恢复约8秒且cache-hit=true；包含恢复/保存开销。保留试点，不推广到私有SQLx patch或release |
| P3 | 将三个完整target由other移到pagination，保持七个runner和原执行审计 | 新代码PR中other执行约422秒，基线约969秒；两分片269条结果逐项相同。nextest本地338.85→268.68秒但未迁入门禁，优先利用空闲runner |
| P4 | Quality与audit并行计算、原Quality聚合仍严格；保守纯文档PR路径保留所有必需check名；main始终全量 | 失败/取消/跳过、分类失败和重命名均有定向验证。真实文档路径及最终main结果附最终文档PR；CD仍复用现有控制器 |

### 实际CI样本与局限

有效反馈时间从该attempt开始计到原有被测job结束，不含计时汇总job本身。
runner-min 为各job执行时间之和，不是实际账单分钟，也不是并行墙钟。

| 样本 | 有效反馈 | runner-min | 说明 |
| --- | ---: | ---: | --- |
| [P0 PR基线 41581738758](https://github.com/LonelyFellas/tsz-rust/actions/runs/41581738758) | 1352s / 22m32s | 103.567 | 优化前相同测试范围 |
| [整合代码PR 41584882046](https://github.com/LonelyFellas/tsz-rust/actions/runs/41584882046) | 998s / 16m38s | 104.050 | 端到端16m58s；该样本反馈约缩短26%，冷缓存总runner时间基本持平 |
| [基线main 41584055949](https://github.com/LonelyFellas/tsz-rust/actions/runs/41584055949) | 1208s / 20m08s | 98.867 | main包含release构建及制品验收 |
| [整合main 41586678323 attempt 1](https://github.com/LonelyFellas/tsz-rust/actions/runs/41586678323/attempts/1) | 1263s / 21m03s | 114.533 | 冷缓存，完整成功；不是整体加速的证据 |

主线handler环境记录均为4 vCPU、runner image `20261026.354.1`、Rust1.98.0，但baseline为
AMD EPYC 9V45，candidate为EPYC7763。未改代码的admin执行也由237秒变为522秒。
这些样本执行资源不完全相同，不能把某次总耗时变化完全归因于本次代码。稳定低于15分钟尚未证明；
当前最长任务在handler和identity间变化，后续应按runner环境继续观察，而不是由单次结果声称SLA。

代码PR下载七份inventory后，与P0基线逐项核对：**100个发现target及1316条用例名称/状态完全一致**，
没有漏跑、重复或新增ignored；其中一个target原本没有用例。此项独立核对并非只相信绿色汇总。

### 缓存与发布资格

缓存实测使用同一main SHA、同一run的unit/doc定向重跑（attempt 2），没有重跑未变的整套集成测试。
冷job `125753731411` 为331秒，暖job `125761125130` 为162秒；测试均成功，暖cache-hit=true。
编译日志的Compiling行数从298降至1，确认暖路径复用了依赖编译结果。
缓存ID `9920888176` 的ref为`refs/heads/main`，大小655052704字节；编译依赖缓存不证明测试成功，
所有Rust命令仍实际执行。失配规则由fingerprint回归覆盖，冷miss路径实测通过；不对GitHub做故障注入。

定向重跑会增加workflow attempt，不能把上一attempt的制品验收回执视为最新回执。
因此缓存验证后已单独完成release smoke attempt 3：状态passed，artifact ID `13208880794`，
producer attempt 1、verification attempt 3，source SHA仍为200b69b，cleanup.complete=true且remaining为空。
继续遵守原控制器的来源门禁。最终文档合并后仍要求一轮完整main CI，最终状态附该PR及交付总结。

### 保留与不采用

- 不缩减251条/2008候选规模，不用直接填表替代发布契约；已有Router复用已证实可减少重复工作。
- 不引入nextest生产依赖：本地兼容与性能试验已完成，但现有分片再平衡更直接，保留原进程模型和审计。
- 不引入merge queue或新的制品发布工作流：未观察到队列是主成本，且现有部署来源绑定当前CI，需要保持。
- 不开启自动CD，不宣称真实服务器回退已验收。37项工具隔离恢复/事务/迁移证据回归通过，
  真实恢复仍须另行批准具体现场与数据损失边界；这不在本次代码推送授权内。

独占本地Postgres/Redis、匿名卷和原始测试日志已清理；仅保留脱敏指标、清单与实验记录。
源码无生产API/SQL/迁移变更。独立审查捕获的重跑重复计费、文档matrix检查缺失两项问题均在相应推送前修复。

## 1. 证据与可比性

### 本项目已观察到的事实

| 样本 | CI 墙钟 | handler 执行 | other 执行 | 分页单测执行 |
| --- | ---: | ---: | ---: | ---: |
| [main 41484420432](https://github.com/LonelyFellas/tsz-rust/actions/runs/41484420432)，11-03 00:44 | 15m36s | 11m48s | 10m19s | 9m09s |
| [main 41566621505](https://github.com/LonelyFellas/tsz-rust/actions/runs/41566621505)，11-03 10:31 | 23m08s | 18m41s | 15m53s | 11m52s |
| [PR 41567459522](https://github.com/LonelyFellas/tsz-rust/actions/runs/41567459522) | 24m04s | 18m02s | 15m47s | 9m31s |

两次 main 的 handler 都运行 188 个用例。累计 body 从 2267s 增至 3645s，累计 migration
从 419s 增至 649s；累计时间包含并发，不能当作墙钟或相加计算提速。两个版本之间只有少量
回退守卫测试及迁移 down 变更、handler 超时预算调整。广泛变慢提示环境波动或共同成本，
不证明一定是 runner CPU，也不足以排除代码影响；必须用固定源码的对照实验验证。

较慢 main 的分页测试：空数据 1.529s、内容准备 398.952s、发布准备 276.073s、
查询及断言 33.352s。约 95% 用于准备数据。当前实现为 251 条真实词条、2008 个候选，
每条发布前进行音频检查和确认。该测试没有进入已有的显式 Router 复用 scope，普通请求会
重新构造完整应用 Router。重复构造的存在已由代码确认；其占总耗时的比例尚未测量。

最新 PR 中缓存恢复约数秒，各集成分片仍编译约 2～4 分钟。缓存恢复快只能说明恢复开销小，
不能证明编译缓存有效；当前 `cache-targets: false` 禁用了 target 缓存。

### 外部项目：学习机制，不建立虚假的排行榜

评估源码固定在：Quickwit `aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de`，
Vaultwarden `82fc47e83e39c5d09e85a6f259c8ab51166735d1`，
Meilisearch `fccb3537c728d30cb52ef0fb4e00ce239713ec30`。这些是评估时主线快照，
不声称与前面抽样运行的提交完全一致。

| 项目 | 做得好的地方 | 可比性与不宜照搬之处 |
| --- | --- | --- |
| Quickwit | PR 用 nextest + REST 测试；tests/lints 并行；编译缓存；完整测试和覆盖率在 merge queue；发布包、镜像分工作流 | 约 11～15 分钟是常规 CI 样本，不包含另一条完整测试链路；不能作为全量合并延迟。nextest 配置允许两次重试，本项目不默认继承 |
| Vaultwarden | 固定工具链与 MSRV 矩阵，多 feature 组合，统一 RUSTFLAGS、专用 ci profile、构建缓存；发布镜像按架构构建并生成 attestation | 4m36s/8m31s 是具体 Build 样本；数据库 feature 编译测试不等价于本项目 SQLx 逐例创建数据库的回归范围；无需为了模仿而新增本项目不承诺支持的平台/MSRV |
| Meilisearch | Linux/ARM/Windows 与 enterprise 矩阵；macOS、近全 feature 等重组合放定时/手动；release 编译也是测试门的一部分 | 是更大型产品矩阵。此前 73 分钟是包含重跑间隔的总历时，不能称作一次测试执行；所查重跑约 36m30s，也不是所有 jobs 重新全跑 |

Quickwit 的 merge queue trigger 在 PR 上写入“队列资格”成功状态，真正 full test 在
merge_group SHA 上运行并回写状态。只有配置了对应队列保护才能成为完整门禁；本次未读取
它的实际仓库保护规则，不能仅凭 YAML 宣称所有合并都受约束。本项目不复制“先写成功”的模式。

Vaultwarden 的 `profile.ci` 关闭 debug assertions，并对开发构建中的 argon2 单独优化。
这说明耗时比较还受到编译语义和热点依赖配置影响。本项目已经在 CI 关闭调试符号，不能把
关闭 debug assertions 作为无损提速照搬；若未来分析证明某 CPU 热点来自未优化依赖，才单独
评估 dependency opt-level，并验证与生产语义一致。

## 2. 我们已有与不足

| 维度 | 现状与判断 | 处理 |
| --- | --- | --- |
| 并发取消 | 已按 PR/ref 取消过期 CI，分片各有 Postgres/Redis，fail-fast=false | 直接复用，不重复建设 |
| 完整性 | 按 SHA 核对 target、用例、ignored 和实际执行，恰好一次，缺报告失败 | 必须保留；调度更换不能只检查退出码 |
| 门禁 | main 保护启用管理员约束和 strict；必需检查为 Quality、Unit and doc tests、Integration (lexicon/admin/identity/platform) | 任意改名或路径跳过均影响保护；初期保持名称及语义 |
| 测试成本 | 大批 fixture 经多轮真实请求准备；Router 复用只在最大批次试点；大分片失衡 | 优先测量和扩展已有安全复用；不先减少 fixture 尺寸 |
| 调度 | 分片间并行，分片内各 target 顺序执行，单 binary 内由 libtest 并行 | 不是全串行；nextest 的收益在跨 binary 调度，须实测资源和隔离 |
| 编译 | 所有 job 禁用 target 缓存；每个分片分别编译 | 有潜在收益。历史提交 `648d310` 将其作为加固的一部分，不能当作漏配直接撤销 |
| 采样机制 | identity/handler 在私有 workspace 修改 SQLx 测试源码并独立编译；identity 还带模板库优化 | 有证据保护，但维护成本高。临时 instrumentation 不应未经评估继续扩大；不能把模板优化当纯计时直接删掉 |
| 可观测性 | 已有 ci_metrics、cache-hit、SQLx 阶段和批次采样 | 缺统一的排队/执行/成本/冷暖缓存汇总；失败时 runner 在写最终报告前抛错，成功后才上传 inventory，失败证据不充分 |
| 快速反馈 | Quality 必须等待 dependency-audit，之后才 fmt/Clippy | 有非必要等待，但目前不在最长关键路径；优化能更早暴露错误，不应声称会缩短 24 分钟总耗时 |
| 文档 | integration-sharding.md 仍写六份报告、learning 随 pagination，实际为七分片；Router 旧说明为 243 请求，当前采样含音频请求为 343 | 更新已有文档，明确历史记录和当前结构，避免下次沿用错误假设 |
| 制品来源 | main 才构建，固定 SHA/tree/run/attempt/artifact ID/digest；下载原制品隔离验证，服务端不编译 | 强项，保留；外部镜像发布方案不是我们的必需升级 |
| 部署恢复 | 持久化事务、owner 锁、来源复核、候选/正式鉴权、业务持久化和清理都有实现与正常发布证据 | 真实故障回退尚未演练；应完善恢复证据，不等于立刻启用自动 CD |

部署控制器 `ops/deployment_transport.py` 明确绑定 `.github/workflows/ci.yml`、工作流名 `CI`、
最新精确 main run/attempt 和 smoke 回执。因此“拆成 ci.yml + release.yml”不是纯整理文件。
当前 release 构建与测试并行，下载制品的 smoke 只依赖构建；最终可部署性由完整 CI 成功门禁控制。
提前产出 artifact 不等于提前获得部署资格。这一设计可复用。

## 3. 推荐架构

近期继续保持一个受信 CI 工作流：

```text
PR
  现有质量/审计门 ─┐
  unit + doc       ├─> 现有必需检查与完整执行审计
  integration      ┘      （不省略业务回归）

main 精确 SHA
  同等完整 CI ───────────────────┐
  release build -> 原制品隔离 smoke ├─> 可部署资格
                                  ┘
明确授权 -> 既有 deploy_test.py -> 候选/正式/业务/清理验收
```

源码、测试服务、临时数据不跨 job 共用；允许复用的仅为经过限定的编译结果。
PR 缓存和测试 artifact 不能获得 release 来源身份。正式发布仍走精确 main CI 制品。
nightly/manual 可以用于诊断、资源扫描或未来增加的平台组合；现有业务回归不迁出 PR。

## 4. 实施顺序与依赖

### P0：基线、失败证据和文档一致性（小改动）

复用 `ops/ci_metrics.py`，补充统一的脱敏摘要，记录 SHA/tree、run/attempt、事件、
runner image/CPU/核数、工具链、缓存状态、排队/准备/编译/执行/上传耗时、每个 job 的运行时间。
计算并展示最大关键路径及未加权 runner-minute 总和；不同机型的实际计费另列，不混用。
不构建独立 dashboard 服务，不新增常驻监控。

失败报告采用独立诊断 artifact，并标明 incomplete；`always()` 只用于保存证据，不能让
残缺报告通过原来的完整执行审计。继续返回原失败退出码，禁止 continue-on-error 掩盖测试失败。
修正现有分片文档，不改历史证据数值。

验证：缺失/重复报告、错误 SHA、意外 ignored、子进程失败都继续失败；失败前采样可以读取；
不记录连接串、token、用户数据。复用现有相关脚本回归测试，补测试只覆盖新增分支。

### P1：分页与 handler 的 fixture / Router 成本（优先收益，复用 + 小改动）

1. 在分页和慢 handler 样本测量 Router 构造次数、构造耗时、HTTP 请求耗时、fixture 各阶段，
   区分嵌套时间与并发累计时间；不能把整段 HTTP 时间全归因于 Router。
2. 首选沿用 `with_test_router` 的显式单测试、单 AppState 生命周期；所有原 HTTP 请求、
   中间件、鉴权、音频审核、真实发布和 251/2008 判据保持。验证 buffered 并发调用仍在 scope 内，
   若使用 tokio::spawn 则另外检查 task-local 传播，不能默认继承。
3. 保留不同 AppState、嵌套 scope、账户撤销、请求 ID 和 Swagger 的已有回归；新增验证仅针对
   这次扩大复用范围的实际风险。不要把 Router 缓存跨用例或跨数据库。
4. 若 Router 复用收益不足，才考虑一次准备真实发布 prototype 后批量构造等价 fixture。
   这会改变测试路径，需单独明确 publication snapshot、surface projection、音频回执、引用与
   revision 的等价性，并保留真实写入链路覆盖。不以直接填最终表替代整个发布契约。

验收：相同业务断言/候选集、持久化与隔离均通过；报告分页、handler、全流程三个层级的变化。
只缩短分页不能宣称总 CI 同幅下降，因为当前最长的是 handler，随后可能是 other/identity。

### P2：受限的编译缓存实验（新验证，不直接全量启用）

先以普通 unit 或非采样 integration job 做单变量实验，试用只由受信 main 成功运行保存的
编译依赖缓存；PR 只读取，不保存。release 继续原构建策略，避免同时改变部署信任边界。
复用现有 fingerprint，并核对 OS/架构、rustc、features、profile、flags、SQLx 元数据以及
action 自身 key；cache 只用于提速，不用于证明测试成功，缓存失败必须可干净重编译。

identity/handler 的 `CARGO_TARGET_DIR` 在独立 profiling state 下，SQLx 有 path patch。
普通 `target/` 缓存不覆盖它们。若进一步优化，应显式隔离 instrumentation/template 版本、
patched 源和私有目录映射；不要把它们与普通 SQLx 产物无条件共享。

验收：分别测冷缓存、暖缓存、失配/不可用缓存；包括下载解压/上传保存和磁盘峰值，验证源代码、
迁移、锁文件、编译 flags 变化确实重编译所需部分。只在净墙钟改善且正确性守卫不变时保留。
先不用“全量编译一次再分发”：它可能把当前并行编译变成统一屏障，增加传输并延后全部测试。
只有缓存实验后仍有明显重复成本，再以同 run 精确 artifact 做受限实验并评估总路径。

### P3：nextest / 分片调度试点（条件执行）

依赖 P0 的逐 target 数据和 P1 后的新耗时分布。先选择无 SQLx patch/template 的
`lexicon-other`，单独比较当前 libtest 与固定版本 nextest；不要同时改分片数、缓存和 fixture。
保留旧路径作为对照，默认 retries=0，保持 doc tests 独立执行。

nextest 是每用例一个进程：进程内静态互斥、MASTER_POOL 初始化、全局 semaphore 与缓存不再
具有原来的共享范围。逐项核查 SQLx 数据库登记/清理、Redis key/DB、端口、文件、外部 IO、
连接峰值和并发限制。job 级独立 Postgres/Redis 不等于同 job 内的用例已经隔离。
对真实资源约束显式限流或分组，不能用测试重试“修复”竞争。

适配发现清单与结果输出，核对 target+case、ignored、重复/漏跑和 SHA；机器可读格式需固定
工具版本并选择稳定可支持的输出，不将 stdout 正则原封不动套在新 runner 上。
总清单与旧 runner 一致且故障路径可验证后再替换。instrumented identity/handler 后续单独评估。

若进程启动、数据库初始化/清理或资源争用抵消收益，就保留 libtest，只做有测量依据的静态
再平衡；不为采用工具而强行迁移。哈希均分并不保证耗时均分。

### P4：CI 分层与 CD 成熟度（后续，不在第一轮重构）

- 纯文档快速路径：使用严格白名单；CI 脚本、构建配置、依赖、migrations、.sqlx、schema、
  fixtures 等一律走完整检查。必需检查始终出现，明确标记“文档变更不适用”；分类失败跑全量。
  先保持 main 完整验证，不让 PR 的快速路径直接生成可部署资格。
- Quality 等待 audit：如要提前 lint 反馈，将计算与最终聚合职责分开，仍让现有 Quality 名称
  同时要求 audit 与 lint 成功。这是反馈速度优化，只有它进入关键路径后才影响总耗时。
- 采样维护：计时与 identity 模板功能先解耦，建立原 SQLx 路径的对照；再决定深度计时转为
  手动/诊断运行还是保留常规。没有测量就不承诺去掉 instrumentation 会快多少。
- merge queue：目前不是必需依赖。只有合并吞吐和重复重跑确实成为主要成本，再检查 GitHub
  套餐/规则支持、merge_group 触发和全部检查映射。不能仅复制 Quickwit 的资格状态写法。
- CD：保留现有控制器；先在隔离环境验证旧制品恢复、迁移拒绝和中断续跑，再单独规划获授权
  的真实恢复演练，写明写入、数据损失与停止条件。正常发布成功不等于恢复能力已验收。
- 镜像 attestation 是 Vaultwarden 可借鉴的长期制品供应链能力，但本项目当前精确 GitHub
  artifact/API/digest 校验已存在；不为追赶功能数量引入容器发布体系或自动部署。
- 如果将来拆分 release workflow，必须同步 transport 的来源选择、run/attempt 关联、smoke
  回执和来源拒绝测试；不能把它作为 YAML 整理顺手完成。

## 5. 实验与完成标准

第一目标是改善已确认的成本，不承诺“改完 5 分钟”。暂定正常代码 PR 完整反馈的中位数
不超过 15 分钟为方向性目标；是否能成立由固定条件样本决定。先报告分布范围与中位数，
样本不足时不报貌似精确的 p95。不同事件、冷暖缓存、尝试编号和失败/取消不能混合计算。

对照流程：固定 baseline/candidate SHA、相同测试清单、相同 runner 标签及服务镜像、相同
编译参数；交错安排 A/B，先各三次作为试点采样建议（不是测试数量配额）。若波动吞没差异，
扩展必要样本或标为不确定。统计包含失败/取消，成功样本耗时与失败率分别列出，不能只挑快的成功运行。
达到有意义的改善且无正确性回退即结束实验，不为了样本数量额外耗费 CI。

每阶段独立提交/交付时保留完整质量门；不能同时调整缓存、测试 runner、fixture 和分片导致
无法归因。若耗时无改善或复杂度明显增加，撤销该试点，不把所有备选都实施。
建议常规方案不增加 runner 数；需要增加时单列总 runner-minute 变化及预期收益，重新评估。

实施时按改动选择以下已有检查，不要求纯文档评估运行它们：

```bash
python3 -m unittest ops/test_ci_test_modules.py ops/test_ci_test_shards.py ops/test_ci_workflow.py
python3 -m unittest ops/test_ci_identity_profile.py ops/test_ci_fingerprint.py ops/test_ci_metrics.py
```

真实执行必须在独占、可销毁的 Postgres/Redis 中设置连接信息后运行；不得使用共享库：

```bash
python3 ops/ci_identity_profile.py build lexicon-pagination
python3 ops/ci_identity_profile.py run lexicon-pagination --report target/ci-shards/lexicon-pagination-report.json
```

全量结果收集后沿用 `GITHUB_SHA=<精确提交> python3 ops/ci_test_shards.py verify <报告目录>`，
预期清单完整、恰好一次、无新增 ignored、错误/缺失报告非零退出。nextest 阶段要先实现同等
审计适配，不能跳过此验收。P1 的 targeted 测试通过只说明局部正确，最终仍需完整 CI。

性能证据记录源码和环境、前后耗时、失败率、runner 成本和局限。数据/对象/缓存使用独占
fixture，结束/失败/取消均清理并复查；不创建真实 OSS 对象，除非后续专项明确需要并满足
项目创建前权限与全版本清理规则。只保存脱敏指标，不保存测试凭据与业务原始数据。

## 实施记录（2026-11-03）

- 修正七分片文档；保留现有成功报告和完整执行审计，失败时另存标记为 incomplete 的诊断，不把诊断当成功清单。
- 集成任务保存显式白名单环境、缓存结果和现有编译/执行计时；诊断不含连接信息。分页在单测试、单状态作用域复用 Router，保留全部请求和断言，记录构造时间、按类型的请求数、唯一请求 ID、累计 HTTP 响应时间及原有阶段墙钟。并发累计时间不可与阶段墙钟相加。
- 独占本地 PostgreSQL 16 / Redis 7 的分页用例通过：45.19 秒，251 条、2008 候选、1309 个唯一请求 ID、1 次 Router 构造；内容 16.140 秒、发布 21.924 秒、查询断言 4.641 秒。累计 HTTP 响应时间 86.527 秒含并发，不能与墙钟相加。本机结果不是 CI 对照。两个既有不同状态/嵌套与 Swagger/撤销隔离测试通过。最大批次测试通过，343 个唯一请求 ID、1 次 Router 构造，本机 10.21 秒；仍不能用于推算 CI 提速。
- P0 尚未实现全工作流排队/上传/每 job 墙钟、关键路径和 runner-minute 的统一汇总；已有产物仅覆盖集成任务，不宣称完整基线。P1 尚无固定源码 CI 对照，不能宣称收益。
- P2 缓存和 P3 nextest 暂不实施；既有 release 来源、测试门禁及部署流程不变。

### main 首次运行的预算回归

main `0f7d4fa` 的运行 `41574741920` attempt 1：`lexicon-other` 编译 217.847 秒，
执行从 03:56:31 UTC 到 04:12:33 UTC，被整 job 的 20 分钟上限取消；当时已运行到
`wordlists_tips`，此前没有失败断言。其余分片、Quality、Unit/doc 和 release smoke 通过，
聚合因取消结果正确失败。第 2 次运行已在修复前启动，结果不替代预算修复。
将 `lexicon-other` 与 handler 一样限定为 35 分钟，保留其余分片 20 分钟上限与全部测试/审计；
预算变化仅避免完整回归在末尾被截断，不作为性能改善证据。

### P0 后续：全运行汇总

新增 `ops/ci_run_summary.py`，读取精确 attempt 的 jobs/steps，在所有原 CI 门及 release smoke
结束后生成独立诊断摘要。成功、失败、取消均计入；重跑仅统计当前 attempt，不把上一次复用的
job 混入，区分 PR source head 与实际 workflow SHA。展示准备/其他、编译/lint、测试、缓存、
上传与收尾分类，并汇总最大 job、最后结束的 job、测得反馈时间及未加权 runner minutes。
API 时间戳不能证明真实排队或完整 DAG 因果关键路径，因此仅报告 created-to-start 区间，
缺值保持 unknown；这不包含汇总 job 自身，也不当作账单分钟。诊断 job 没有更改原门禁，
收集失败不替代任何业务检查结果。历史 PR 41573045659 复算为1023秒、96.367 runner minutes。
已用并行、失败、跳过、未知时间与重复/跨attempt身份场景定向验证；真实新CI产物待PR验证。

### P1 后续：慢 handler 的范围受限 Router 复用

基于 main run 41579347068 的 slowest_cases，选择其中位于 lexicon_handler 主文件、单 AppState、
没有 spawn 的14个慢用例，进入已有 with_test_router scope。原HTTP请求、SQL写入、鉴权及
全部断言保留，增加每例仅构造一次Router和请求ID唯一性断言；未推广到不同状态或子任务场景。
本地独占Postgres16/Redis7：排序/分页原用例15.90秒，修改后2.84秒（59请求）；14例并行9.08秒全部通过。
这些是同机局部观察，不推算GitHub整分片收益。既有两例跨状态/嵌套及Swagger/即时撤销回归通过。
各测试批次独占容器与匿名卷均已删除并按owner复查，无服务器或OSS写入。真实PR/main数据待验证。

### P2 试点：unit/doc 编译依赖缓存

只在unit-doc启用target缓存，显式排除workspace crates和安装的bin；使用新的独立prefix避免
复用旧registry-only命中而永远不保存target。保留原fingerprint、锁文件、Cargo重建判定以及
仅同仓main push成功保存的规则；PR/workflow_dispatch只读。release和SQLx patched分片保持原策略。
冷缓存PR验证正确性；main成功后建立受信暖缓存，再以相同main SHA的workflow_dispatch测暖路径。
若下载/解压/保存开销抵消收益，或出现失配，就撤销本试点。当前仅准备代码，尚无暖缓存收益证据。
不为了试验允许PR写入main可见缓存，不缓存数据库、报告或通过状态。

### P4 试点：保持 Quality 门禁，提前质量反馈

Quality检查与dependency-audit并行启动；原必需检查名Quality保留为显式聚合，二者均success
才通过，失败、取消和跳过均不放行。所有Clippy、fmt及工具回归保持，schedule仍仅运行audit。
这主要减少lint错误反馈等待，不宣称缩短当前handler主导的总CI。源码/制品门禁不变。

### P4 试点：严格文档白名单

仅pull_request、非空改动且所有路径在docs/ci或docs/features下并以.md结尾时，运行时测试
才标为不适用；使用精确base/head三点diff、--no-renames，源码移动成文档也会因删除旧源码走全量。
解析/读取/git错误默认全量，main push和手动运行始终全量。保留audit、fmt、工具回归与原Quality
聚合，文档PR省略Clippy、unit/doc和integration；必需检查名称保留，聚合显式要求成功分类及
unit/doc的skipped状态；integration保留matrix实例并在步骤级标记不适用，返回success，
以生成所有受保护的分片check。文档模式服务image为空，不启动容器。普通代码路径的跳过仍失败。没有修改仓库保护规则或部署来源。
已验证白名单/混合改动/真实Git重命名/错误回退和真实聚合shell的失败闭合；真实文档PR待验收。

### P3 实验与选择：先复用空闲分片，不迁移 runner

固定nextest0.9.151、4路并发、零重试，在独占PG16/Redis7上使用与libtest相同的18个可执行
文件做lexicon-other对照：libtest338.850秒，nextest268.680秒，本地约20.7%改善。逐例比较
JUnit与原清单，266成功/2原有ignored/268唯一用例完全一致；实验容器和匿名卷已清理。
这是单次本地结果，不是GitHub收益保证。脱敏记录见evidence/2026-11-03-ci-experiments.json。

同时真实CI41581738758显示other的wordlists_handler、question_bank_handler和shared_sentences
分别214.963/191.113/146.303秒，合计552.379秒；分页已约一分钟。这给出更直接的调度收益：
把这三个完整target移至现有pagination分片，保留libtest、七个runner、服务隔离、全部用例和ignored
审计，不引入进程模型变化。静态估算other可转移约9.2分钟到空闲分片，但需实际CI验证编译开销和
总关键路径。本地重组后的4个target/143个用例清单校验通过（141成功、2原有ignored），命令墙钟228.612秒，
独占容器及匿名卷已清理；该结果不代替GitHub整体收益。nextest保留为已完成的兼容性/性能评估，本轮不投入生产门禁；不是因其无收益，
而是当前已有runner空闲且静态分配预期改善更大、无需增加执行器/报告适配维护面。

## 6. 最终验收边界

本次代码阶段与实验结论已合并main，详见文首结果表。最终文档PR用于验证真实文档快速路径：
必须生成原有Quality、Unit and doc tests、Integration (lexicon/admin/identity/platform)检查，
明确标记不适用的运行时步骤，不启动集成服务容器；不能依赖缺失check或管理员绕过合并。
随后最终main必须重新运行全部运行时测试、清单审计和精确制品验收。该提交无法在自身内容中预知
合并后的run ID，最终结果记录在文档PR和本次交付总结，不通过不断追加“验证自己”的提交制造循环。

真实恢复演练的准备沿用既有runbook和唯一控制器plan/apply/status/resume。先以无新增迁移候选
验证二进制恢复，再分别评估迁移拒绝与人工恢复；故障触发方式、环境、窗口、精确版本与写入范围
须单独明确。成功判据为旧版进程/摘要/账本/鉴权/持久化复验、精确清理及独立复查全部通过；
来源或账本漂移、外部业务引用、恢复失败进入manual_required并保留证据。工具测试和正常发布成功
不能替代这项现场证明。本任务没有执行服务器部署、故障注入或真实数据回退。

## 来源

- [Quickwit CI](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/.github/workflows/ci.yml)、[nextest 配置](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/quickwit/.config/nextest.toml)、[完整测试触发](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/.github/workflows/full-tests-trigger.yml)、[完整测试](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/.github/workflows/full-tests.yml)、[发布包](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/.github/workflows/publish_release_packages.yml)。
- [Vaultwarden 构建](https://github.com/dani-garcia/vaultwarden/blob/82fc47e83e39c5d09e85a6f259c8ab51166735d1/.github/workflows/build.yml)、[编译 profile](https://github.com/dani-garcia/vaultwarden/blob/82fc47e83e39c5d09e85a6f259c8ab51166735d1/Cargo.toml)、[镜像发布及 attestation](https://github.com/dani-garcia/vaultwarden/blob/82fc47e83e39c5d09e85a6f259c8ab51166735d1/.github/workflows/release.yml)。
- [Meilisearch 测试矩阵](https://github.com/meilisearch/meilisearch/blob/fccb3537c728d30cb52ef0fb4e00ce239713ec30/.github/workflows/test-suite.yml)。
- [nextest 逐测试进程模型](https://nexte.st/docs/design/why-process-per-test/)、[分片与构建复用](https://nexte.st/docs/ci-features/partitioning/)、[rust-cache 语义](https://github.com/Swatinem/rust-cache)。
- 运行样本：[Quickwit](https://github.com/quickwit-oss/quickwit/actions/runs/41535065584)、[Vaultwarden](https://github.com/dani-garcia/vaultwarden/actions/runs/41536066804)、[Meilisearch](https://github.com/meilisearch/meilisearch/actions/runs/41457755190)。
- 本地依据：[CI](../../.github/workflows/ci.yml)、[分片执行](../../ops/ci_test_shards.py)、[SQLx 采样](../../ops/ci_identity_profile.py)、[计时](../../ops/ci_metrics.py)、[指纹](../../ops/ci_fingerprint.py)、[来源门禁](../../ops/deployment_transport.py)、[分页/Router](../../tests/lexicon_handler.rs)。
