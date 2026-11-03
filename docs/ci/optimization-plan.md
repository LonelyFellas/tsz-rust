# Rust 后端 CI/CD 对标评估与优化方案

评估日期：2026-11-03（Asia/Singapore）。本项目基线：`4e13630c01aebc4833457e47605d5fe337c024e4`。
当前阶段：P0 局部实施和 P1 本地试点验证完成，按用户授权推进 PR/CI 与合并；真实 CI 性能收益待对照。本文是本任务唯一主记录，复用
[集成测试分片设计](integration-sharding.md)和[部署控制器说明](deployment-controller.md)，不另建发布体系。

## 结论与范围

本项目的主要短板是测试准备成本、重复编译、分片长尾和性能证据闭环；部署来源验证、
独立测试环境和执行完整性审计已经较完善。推荐先优化现有机制，再依据对照结果选择 nextest。
不把增加 runner、迁移到 nightly 或启用自动部署作为默认答案。

目标是缩短 PR 的完整反馈时间并控制 runner 消耗，不改变生产 API、SQL 迁移或业务行为，
不删除回归断言、不增加 ignored、不以重试掩盖失败，不降低已有覆盖率门禁。
没有现成的跨项目“合理分钟数”：公开仓库运行只能用于理解工程取舍，不能作为同负载基准。

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

## 6. 当前阶段与完成条件

- P0：PR #381已合并main `b6153c8`；真实运行41581738758的摘要产物已核验，测得1352秒、
  103.567 runner-min，最长handler。重跑复用job不重复计入，诊断不改变门禁。
- P1：14个慢handler范围受限Router复用已完成隔离验证和独立审查，真实整分片收益待候选CI。
- P2：unit/doc受信编译依赖缓存已完成代码/规则验证和独立审查；冷/暖路径净收益待main建立缓存后验证。
- P3：nextest同二进制、同用例、4线程、零重试的本地比较和结果核对已完成；本轮选择现有七分片
  再平衡以利用空闲runner。真实编译开销与墙钟尚待验证，不将估算当结果。
- P4：Quality并行反馈、保守文档快速路径及保护check保留已经实现并做失败闭合验证；真实代码PR
  与最终文档PR仍待验收。merge queue暂不引入：当前没有合并排队成为主成本的证据，先解决已测长尾。

部署控制器的37项事务/恢复策略/迁移证据定向回归通过，覆盖的是现有工具隔离测试，不是服务器
真实恢复证明。现有精确制品与accepted门禁不改动，自动CD不启用。真实恢复演练不在本次“推送main”
授权内；后续演练需单独批准环境、时间窗口、精确候选/基线、写入/数据损失范围及恢复策略。
演练准备沿用现有runbook：先以无新增迁移候选验证二进制恢复，再分别评估迁移拒绝与人工恢复，
通过唯一控制器plan/apply/status/resume记录原事务和owner；故障触发方式须另行明确，禁止现场拼接
手工远端发布步骤。成功判据为旧版进程/摘要/账本/鉴权/持久化复验、精确清理及独立复查全部通过；
来源或账本漂移、外部业务引用、恢复失败进入manual_required并保留证据，不能强行宣布恢复完成。

剩余交付：候选完整CI、分片清单和P0汇总产物核验 → 合并main → 同SHA缓存冷/暖验证 →
最终Markdown验收报告PR验证文档快速路径 → 最终main完整CI和总体总结。既有历史记录中的“待验证”
反映当时状态，以本节及最终交付记录为准。所有源代码通过PR进入main，不直接在main开发或推送。

## 来源

- [Quickwit CI](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/.github/workflows/ci.yml)、[nextest 配置](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/quickwit/.config/nextest.toml)、[完整测试触发](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/.github/workflows/full-tests-trigger.yml)、[完整测试](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/.github/workflows/full-tests.yml)、[发布包](https://github.com/quickwit-oss/quickwit/blob/aa478914ddb7f8d87f3fb14c77b2131c3f5ad2de/.github/workflows/publish_release_packages.yml)。
- [Vaultwarden 构建](https://github.com/dani-garcia/vaultwarden/blob/82fc47e83e39c5d09e85a6f259c8ab51166735d1/.github/workflows/build.yml)、[编译 profile](https://github.com/dani-garcia/vaultwarden/blob/82fc47e83e39c5d09e85a6f259c8ab51166735d1/Cargo.toml)、[镜像发布及 attestation](https://github.com/dani-garcia/vaultwarden/blob/82fc47e83e39c5d09e85a6f259c8ab51166735d1/.github/workflows/release.yml)。
- [Meilisearch 测试矩阵](https://github.com/meilisearch/meilisearch/blob/fccb3537c728d30cb52ef0fb4e00ce239713ec30/.github/workflows/test-suite.yml)。
- [nextest 逐测试进程模型](https://nexte.st/docs/design/why-process-per-test/)、[分片与构建复用](https://nexte.st/docs/ci-features/partitioning/)、[rust-cache 语义](https://github.com/Swatinem/rust-cache)。
- 运行样本：[Quickwit](https://github.com/quickwit-oss/quickwit/actions/runs/41535065584)、[Vaultwarden](https://github.com/dani-garcia/vaultwarden/actions/runs/41536066804)、[Meilisearch](https://github.com/meilisearch/meilisearch/actions/runs/41457755190)。
- 本地依据：[CI](../../.github/workflows/ci.yml)、[分片执行](../../ops/ci_test_shards.py)、[SQLx 采样](../../ops/ci_identity_profile.py)、[计时](../../ops/ci_metrics.py)、[指纹](../../ops/ci_fingerprint.py)、[来源门禁](../../ops/deployment_transport.py)、[分页/Router](../../tests/lexicon_handler.rs)。
