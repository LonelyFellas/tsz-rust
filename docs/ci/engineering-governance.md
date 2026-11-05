# 工程治理接力记录

2026-11-05，目标：在保留 Rust、产品行为、HTTP/wire 和数据库模型的前提下，分批解决已有工程问题。
四批本地实施与验证已结束。用户随后授权审查、提交、push、开 PR，并修复 CI 至通过；
当前进入交付阶段，合并与部署未授权。四批共用本文和同一工作区。
既有 CI 优化结果直接引用 [optimization-plan.md](optimization-plan.md)，不重复实施或重跑历史实验。

## 工作区与接力

- 工作区：`/Users/darwish/Dev/tsz-core/.worktrees/backend/engineering-governance`。
- 分支：`codex/engineering-governance`；基线：`fb0575d019e6eb98d722ee85279805deeb034a56`。
- 已用 Codex `create_worktree` 创建，再将这个刚创建、尚未实施的工作区放入父级规则指定目录。
  未迁移任何既有任务工作区。Codex `attach_worktree` 对规范目录返回“not a managed worktree”；
  Git 工作区有效，后续命令必须显式指定上面的绝对路径，不能使用失效的初始 App 附件路径。
- 后续批次复用全部未提交修改；先确认前序已结束、没有遗留编译进程，再成为唯一写入者。
  不 reset/stash，不新建另一份任务工作区，不因本地差异重新征求实施授权。

## 第 1 批：磁盘与编译稳定性

状态：首次验收未通过；现已撤回不当门禁并落地可持续的定向回收工具。磁盘治理仍有明确保留范围，
不宣称所有缓存问题已解决。历史编译故障的原命令、SHA 不完整，不能声称已复现或全部修复。

### 已有能力和本次改动

- 已在主线：Rust 1.98.0 固定工具链，dev/test 行号调试信息、依赖无调试信息；原生范围感知
  pre-commit 和 fmt-only pre-push；`ops/ci_metrics.py` 可记录真实退出码和耗时。
- 首次复制旧任务 `local_cargo.py` 并接入 pre-commit 的做法未通过验收：50 GB 常量没有峰值测量依据，
  会误阻增量提交，也不能治理缓存增长；其配置解析与 Python 3.11 依赖没有必要。
  已删除该脚本/孤立测试，恢复原生 pre-commit 和 hook 测试，去掉原 CI 耦合。旧任务工作区保持原样。
- 复用首次清理的归属/摘要/占用/锁判断，新增 `ops/local_build_cache.py` 的只读 `plan` 和显式 `apply`。
  不解析 Cargo 配置，不自动淘汰，不加入 commit/push 路径。CI 仅在临时目录运行其安全回归测试。
- 现存 worktree 不自动意味着缓存必须保留：具备 tsz 集成测试 fingerprint、唯一 dep-info 归属、
  本仓可重建测试源码和 native executable/object 格式，且无活跃使用、非保护对象时，可以精确回收。
  不能仅凭 mtime 或无进程删除。工具不删除 worktree、dep-info、fingerprint、库、主服务或源码。

下文 Cargo 命令已更新为当前可执行的原生命令；前四批实际曾通过旧 `local_cargo.py` 包装运行，
它没有改动 Cargo 参数/检查。本次只验证工具/hooks，不重跑原输入未变的 Rust 检查。
首次原始命令和测量保留在回执中，不把历史命令当作当前操作指南。

### 归属与定向回收

清理前共享 target `74,552,948 KiB`（71.0992 GiB），其中 `debug/deps` 约 66.97 GiB：
无扩展名文件 34.95 GiB、对象文件 12.79 GiB、rlib 12.02 GiB、rmeta 6.10 GiB。
缓存存在两个 rustc fingerprint 身份和多个 profile，检查到的 tsz-rust fingerprint 的 rustflags
均为空；不能仅凭这些身份推断每个产物的工具链版本或删除资格。源码、Git、registry/git 依赖源码
与 target 分别对待；发现另一个 `registration-region-check` 项目产物，也保留。

首次仅删除 [精确回收回执](evidence/2026-11-05-local-build-cleanup.json) 列出的 15 个 arm64 Mach-O
集成测试二进制。每项由 tsz-rust 测试 fingerprint 和 dep-info 的唯一绝对迁移路径证明归属；
测试源码仍可从本任务基线重建。三个历史来源均已不存在且未登记为 Git worktree：

- `/Users/darwish/Dev/tsz-core/.worktrees/backend/issue-279-login-throttle`
- `/Users/darwish/Dev/tsz-core/.worktrees/backend/registration-ip-tests`
- `/Users/darwish/.codex/worktrees/question-bank-workbench-backend/tsz-rust`

删除前重新核对 SHA-256、inode、大小、mtime、非符号链接/非硬链接、dep-info 摘要、来源不存在、
无 Cargo/rustc/rustdoc/clippy-driver、lsof 无目标文件占用，并持有三个现存 Cargo 锁。
mtime 仅作文件未变化检查，不作为废弃判断。仅 unlink 清单里的文件，未递归删除目录。

清理后共享 target `73,304,536 KiB`（69.9087 GiB），减少 `1,248,412 KiB`（1.1906 GiB）。
主仓 target 始终 `9,603,544 KiB`；两项运行中后端服务文件 SHA-256 未变。
保留 dep-info、fingerprint、所有对象文件/依赖库、所有现存任务缓存、业务数据、源码、未提交修改，
以及无法证明归属的文件。没有对 `tsz-lexicon-domain-batch-2` 做任何写入或清理。
今后回收仍须重新核实归属和占用，不能把本次清单或旧路径不存在当作长期授权清单。
首次“保留所有现存任务缓存”是当次保守选择，不是用户禁止回收可重建产物的长期规则。

### 编译失败证据与验证

本机历史 Cargo fingerprint 保留的原始错误包括：

- `error communicating with database: Connection refused (os error 61)`：SQLx 编译期数据库环境错误。
- `no field \`learning_memory_enabled\` on type \`AppState\``、`mismatched types`：源码/类型错误。
- `field \`update\` is never read`、`very complex type used. Consider factoring parts into \`type\` definitions`：lint 错误。

原始消息、次数和样本文件保存在回执中。没有找到磁盘不足、链接器崩溃或 LLVM 错误的现场证据；
原命令与 SHA 未知，不用当前通过结果冒充历史故障的原路径回归。没有连接未知数据库来重现连接失败。

| 检查 | 结果 |
| --- | --- |
| 首次旧门禁/hook/CI 工具测试（门禁脚本与测试现已撤回） | 当时 32 项通过，不替代修正验收 |
| `sh -n .githooks/pre-commit` | 通过 |
| `cargo fmt --all -- --check`、`git diff --check` | 通过 |
| `SQLX_OFFLINE=true python3 ops/ci_metrics.py run --name local-clippy-lib -- cargo clippy --locked --offline --lib --all-features -- -D warnings` | 通过，20.181 秒，target 增加 36 KiB |
| 同命令暖构建（metric 名为 `local-clippy-lib-warm`） | 通过，0.394 秒，target 增量 0 KiB |

这是复用现有依赖缓存的局部构建，不是从零冷构建。未运行全量 Rust 测试、数据库集成、真实用户联调、
PR/CI 或部署；本批未改 Rust/SQL/API。后续按实际改动补齐相关验证，不重复本批有效检查。

### 首次验收修正：生命周期与实际回收

本机 Cargo 1.98 自带 `reference/config.html` 的 cache 说明确认：自动清理目前仅跟踪 Cargo home
的 registry/git 下载缓存，尚未跟踪 build artifacts。共享 target 复用依赖，但不同工具链、profile、
feature、编译输入/工作区仍留下不同 hash 的产物；`incremental=false` 只避免增量缓存，不回收旧测试
可执行文件和残留 `.rcgu.o`。首次 `debug/deps` 中约 34.95 GiB 无扩展名文件、12.79 GiB 对象文件
就是主要调查方向，不能把共享 target 当作自动有界缓存。

已检查本机安装的 `cargo-sweep`：支持 age/toolchain/maxsize/stamp，但这些条件不证明 tsz 归属、
工作区/服务未在用或受保护对象排除，故没有直接调用其删除模式；`/Users/darwish/Dev/worktree-gc`
当前不存在。没有再做 Cargo 配置解析/通用缓存平台，只把已验证的精确清理步骤保留为仓库工具。

修正前立即测量共享 target **74,050,032 KiB（70.6196 GiB）**，按本工具的实际边界分类：

| 类别 | GiB | 处理 |
| --- | ---: | --- |
| 有测试归属、源码和未使用证据的候选 | 23.9433 | 本轮精确选择其中 16.8409；其余 7.1024 保留供按需回收 |
| 当前活动工作区测试产物 | 1.5032 | 保留 |
| 测试归属未知 / 当前源码不齐 | 0.3946 / 0.2976 | 保留，不猜归属 |
| 共享依赖库、主库、主程序及其他非集成测试产物 | 44.4810 | 不在本工具回收域，保留；未声称都在用或都可删 |

本轮清单涉及七个精确 owner：首次的三个退役路径，以及四个现存工作区：

| 现存 owner（均在 `.worktrees/backend/`） | 文件数 | 回收 GiB |
| --- | ---: | ---: |
| `repo-health-cleanup-20261024` | 1112 | 10.4976 |
| `dubbing-center` | 572 | 3.7552 |
| `jwt-dependency-audit-20261024` | 9 | 1.2085 |
| `ci-cd-assessment` | 144 | 1.3168 |

另三个退役 owner 共 143 个残留对象文件、0.0629 GiB。合计 **1,980 个精确文件、16.8409 GiB**。
执行前后重新验证 plan、文件 SHA-256/inode/mtime/大小、dep-info/fingerprint 摘要、源码存在、
非软/硬链接及本用户归属；持有三把现有 Cargo 锁，检查编译进程及 lsof 的文件/cwd 占用，
完成摘要复核后再次检查占用。只删除计划中测试可执行文件和对应对象文件，发生部分失败输出已删清单。
重新运行这些旧任务测试可能需要重新编译/链接；共享依赖和源码保留，生产运行不受影响。

清理后共享 target **56,391,028 KiB（53.7787 GiB）**，主仓 target 仍 **9,603,544 KiB**。
独立枚举复查：除清单外其余 **72,619 个文件**的 inode/大小/mtime 全部不变，四个现存 owner
Git 状态和差异摘要不变，第二至四批源码及两项受保护服务 SHA-256 不变。
精确路径/摘要、单元 dep-info/fingerprint 证据、分类及回执合并进原回收 JSON 的 `acceptance_correction`。
这不是“磁盘问题全部解决”：新测试二进制仍会增长，剩余非工具范围没有得到删除资格。

持续使用方式：任务结束/空间复查时，从**不属于待清理 owner 的目录**运行工具，先看只读盘点；
不需删除 worktree 才能回收。省略 `--worktree` 时只盘点，不产生任何删除候选。复核归属/影响后，
重复 `--worktree` 精确选择 owner，保存清单，再显式执行：

```sh
python3 ops/local_build_cache.py plan --target-dir /Users/darwish/.cargo-target
python3 ops/local_build_cache.py plan --target-dir /Users/darwish/.cargo-target \
  --worktree /absolute/reviewed-owner > /tmp/reviewed-cache-plan.json
python3 ops/local_build_cache.py apply /tmp/reviewed-cache-plan.json
```

`apply` 会重新发现并比较完整计划，输入或占用改变就拒绝，不按旧清单盲删。缺 lsof、未知锁布局、
保护路径 `tsz-lexicon-domain-batch-2`、活动编译、打开的文件/cwd、路径链接、非原生文件等均拒绝。
库、主程序、dep-info/fingerprint、未知归属和源码不齐的测试不在其删除范围。没有自动计划任务，
没有 hook 空间阈值，也不修改全局配置。缓存大小没有硬上限，工具是明确范围的生命周期回收入口。

验证：`python3 -m unittest ops/test_local_build_cache.py ops/test_rule_doc_hook.py ops/test_pre_push_hook.py`
**16 项通过**，覆盖只读默认、精确删除、篡改/陈旧计划、占用变化、锁冲突、保护路径、归属歧义、
软/硬链接、其他仓库 owner 及观测失败；原生 hook 的失败退出仍保留。`sh -n .githooks/pre-commit` 和
`git diff --check` 通过，`python3 -m unittest ops/test_ci_workflow.py` 16 项通过。CI 仅新增临时目录里的清理安全测试，不在 CI 清理真实缓存。
本轮未运行 Cargo 编译，也未重跑第二至四批 Rust 验收。失败定位继续使用已有 `ci_metrics.py`，
记录真实命令/退出码/工具链/target/磁盘增量和错误原文；历史故障缺原命令/SHA的未验证项保持。

## 第 2 批：CI 耗时与失败治理

状态：本地实施与验证完成；候选整体 CI 耗时及 Linux 私有 SQLx 插桩路径待 PR CI 验证。
本批已确认第 1 批完成并释放资源后接管；重新 fetch 后主线仍为 `fb0575d`，完整保留第 1 批改动。

详细失败分类、精确 run/attempt/job/artifact、计时与命令继续维护在
[既有 CI 主记录](optimization-plan.md#2026-11-05-工程治理第-2-批)，不另建 CI 方案。
最近已修复/过期失败不重复修改；编译/链接、SQLx 元数据、Clippy 没有新的 CI 根因证据。
第 1 批缺少原命令/SHA 的历史本机故障仍是未验证项，本批通过不等于替它完成原路径回归。

- 实际新增改动：`tests/lexicon_handler/sentence_formatting.rs`、
  `tests/lexicon_handler/spelling_markup.rs` 仅给最新 main 计时最慢的两个用例复用既有 Router scope，
  加入原有单 Router/request-ID 检查；两个原 body 去掉包装后与 HEAD 逐字节一致。
  本批另更新 `docs/ci/optimization-plan.md` 和本文，共 4 个文件；没有修改 Cargo/profile/生产 src/API/数据库模型。
- 原始问题：main run `41640669021 / attempt 1` 的 handler 编译 228 秒、执行 979 秒，
  两个 body 分别 48.753/42.296 秒。现有优化仍有未进入显式 scope 的重复 Router 构造。
- 同机器、同测试范围、同 profile、本批独立本地 PG/Redis 的定向对照：有效 baseline
  17.501/16.582 秒，candidate 2.870/1.625 秒，均 2 passed；不能外推为整体 CI 降幅。
- 两个 scope 隔离/Swagger/逐请求鉴权回归通过；Python 分片/发现/插桩工具 31 项通过；
  `SQLX_OFFLINE=true cargo clippy --locked --offline --all-targets --all-features -- -D warnings`
  通过，65 秒；`cargo fmt --all -- --check`、`git diff --check` 通过。
- 前后完整 handler 190 用例及 ignored 清单一致；独立复算最新 main 七份精确执行清单：
  100 targets / 1317 cases / 7 shards，1314 ok、3 既有 ignored，无遗漏/重复。
  未声称候选全量运行，分片机制、timeout、安全门和制品来源门均保持。
- 批次 `tsz-ci-batch2-20261105-ewo2ph5g` 的 PG/Redis 已停止，端口关闭，专属数据目录、
  原始日志和测试二进制副本已删除并复查。音频使用 MemoryAdapter，没有 OSS、付费服务或服务器写入。

交接：工作区/分支/基线保持本文开头所列值，本批全部写入及后台编译已结束，未提交或推送。
第 3 批必须保留前两批全部未提交修改，继续作为唯一写入者；只对它的新改动补充验证，
已通过的无关 CI 工具检查不用重跑，整体 CI 留待有授权的后续 PR。

## 第 3 批：公共规则归属与跨模块依赖

状态：本批本地实施与验证完成；三个公共入口已迁移，前后兼容性、定向数据库回归及最终质量门通过。
接管前确认第 2 批聊天已完成、无活动 Cargo/rustc，重新 fetch 后基线仍为 `fb0575d`。
前两批全部修改原位保留，本批不提交、不推送、不部署。

### 已确认问题、依赖与范围

| 原依赖 | 本次归属 | 行为约束 |
| --- | --- | --- |
| 学习任务/计划、学习记忆 → `wordlists::service::hash` | `platform::hash_json` | 保留 serde JSON 字节、结构体字段顺序、数组顺序、SHA-256 原始 32 字节与错误映射 |
| 学习任务/计划、字典及词表各读取入口 → `wordlists::service::page_meta` | `api::page_meta` | 不移动各业务的查询校验；保留分页字段和空页/整页/尾页算式 |
| 自定义音频、词表投币 → `coins::admin_service::coin_error` | `coins::error::coin_error`，后台也从这里引用 | 保留所有状态码、错误码、文案及 `coin_manual_event_once` 特例 |

迁移的三个函数体与 HEAD 逐字节相同；调用点只改导入/路径。账本事务、SQL、账户→任务/词表→
词条/钱包锁序、提交点、请求幂等键均未改动。编译曾暴露字典通过 `reading` 别名引用分页的遗漏
（E0603），已改为直接引用 `api` 并沿原命令回归通过。完整搜索确认生产/测试不再引用旧工具路径。

已在主线并复用：`lexicon::published` 的当前发布读取、排序词条锁和原始快照封装；
`coins::service::apply` 的账户集合排序锁定、savepoint 与账本入口。没有发现需要本批另加入口才能
纠正的重复高风险前置流程，因此不新增流程引擎或 trait 层。`PublishedContentV3` 仍复用原生
forms/meanings、`Dialect` 等内容类型；这符合既有
[边界设计](../features/lexicon-learning-boundary/design.md)，不为去掉 `Draft` 名称大拆 V3 DTO。
若后续要拆分这些原生内容类型，应纳入既定智能词库重构，本批不扩大范围。

### 实际文件

- 公共实现与兼容性测试：`src/api/{mod.rs,pagination.rs}`、`src/platform/{mod.rs,utils.rs}`、
  `src/coins/{mod.rs,error.rs,admin_service.rs}`（仅 `error.rs` 为本批新增源码）。
- 消费方：`src/learning_tasks/{service.rs,plans/mod.rs}`、`src/learning_memory/mod.rs`、
  `src/dictionary/service.rs`、`src/wordlists/{service.rs,custom_audio/mod.rs,interactions.rs,library/mod.rs,management.rs,review.rs,tips.rs}`。
- 既有旧回执 fixture 的哈希引用：`tests/wordlists_tips.rs`；任务主记录：本文。
- 无 HTTP/OpenAPI、SQL/迁移、`.sqlx`、Cargo 依赖或 CI target 清单变化；新增单元检查沿用 lib target。

### 验证与清理

- `SQLX_OFFLINE=true cargo test --locked --offline --all-features --lib boundary_ -- --nocapture`
  在迁移前后均为 4 passed（3 个新增兼容性用例及 1 个已有学习边界用例）。新增用例固定哈希向量、
  分页 JSON，并逐项检查钱包领域错误及通用数据库错误的 HTTP 状态/错误码/文案/content-type。
- 数据库回归使用独占批次 `tsz-boundary-batch3-20261105-dmlzc_a2`，PG `127.0.0.1:51431`、
  Redis `127.0.0.1:51432/0`，显式传入 `DATABASE_URL`、`REDIS_URL`、`TEST_REDIS_URL`；
  运行前核对 PG data_directory 和 Redis PID。所有 fixture/迁移仅在本批实例内执行。
  以下命令编译及运行 81.33 秒，8 passed：

```sh
SQLX_OFFLINE=true cargo test --locked --offline --all-features \
  --test learning_tasks_handler --test wordlists_handler --test wordlists_tips --test admin_coins_handler -- \
  --test-threads=1 \
  eligibility_unknown_fields_preview_and_receipts \
  private_roundtrip_idempotency_revision_and_archive_boundary \
  audio_save_atomic_charge_and_replay_with_two_locales \
  audio_insufficient_balance_and_cost_conflict_leave_no_save \
  legacy_success_replays_with_original_hash_but_blocks_new_tips \
  concurrent_amounts_respect_cap_and_interaction_lock_order \
  concurrent_credit_event_uniqueness_replay_and_atomic_audit \
  dictionary_requires_login_and_reads_only_current_publications
```

运行上述命令须先创建自己的独占 PG/Redis，并设置三个连接变量，不可复用已经清理的历史端口。
本批已在 finally 中停止 PG/Redis、核实两端口关闭，删除并复查整个专属数据/日志目录。
音频使用 MemoryAdapter 和测试 Provider；无 OSS 对象、真实付费服务、共享数据或远端写入。

最终门与交接：

- `SQLX_OFFLINE=true cargo clippy --locked --offline --all-targets --all-features -- -D warnings`
  通过，69 秒；未绕过原生 hooks。
- `SQLX_OFFLINE=true cargo test --locked --offline --all-features --lib -- boundary_ openapi::tests::`
  17 passed（上述 4 项边界及 13 项既有 OpenAPI 契约测试）。
- `cargo fmt --all -- --check`、`git diff --check` 通过；三个移动函数体再次确认与 HEAD 逐字节相同。
- 未运行全量数据库套件、真实浏览器前后端联调、远端 CI 或部署；本批只改变代码依赖归属，
  不把定向通过解释为全量通过。候选整体 CI 和第 1 批历史故障原路径未验证项继续保留。
- 工作区、分支、基线仍为本文开头值；本批全部写入、测试和后台编译已结束。
  第 4 批须复用同一工作区并保留前三批修改，显式指定正确 cwd，沿用共享 target 和 jobs=2（空间门禁已于复验撤回）。
  本批 PG/Redis 已销毁，后续若需数据库测试须重新创建独占环境。

## 第 4 批：学习模块职责与累计验收

状态：本地实施与验证完成；前三批全部修改保留。第 3 批聊天已完成并释放资源后接管，
无活动 Cargo/rustc；重新 fetch 后 HEAD 与 origin/main 仍为本文基线。未提交、push、PR、合并或部署。

### 依赖与实际改动

原问题由调用链确认：`plans/mod.rs` 混合计划/轮次操作、可用性汇总与开练调度；
`service.rs` 同时管理任务及答题结算。一次调整视图或答题流程需要跨读无关业务。
已有 `mastery`、`rewards`、`question`、`source_refresh` 直接复用；第三批公共规则入口保持。
只移动完整职责，不添加流程框架、不改算法、不为缩短函数拆散事务。

| 文件 | 职责与结果 |
| --- | --- |
| `src/learning_tasks/plans/mod.rs` | 计划创建、轮次操作及共享事务辅助；保留原公共入口的 re-export，约 1249 → 640 行 |
| `src/learning_tasks/plans/queries.rs`（新增） | 详情/列表/历史/轮次读取、来源卡片和周期汇总，445 行 |
| `src/learning_tasks/plans/runs.rs`（新增） | 恢复或创建练习、队列选择与题目物化，197 行 |
| `src/learning_tasks/service.rs` | 任务生命周期与旧模式轮次；保留 `service::answer` 入口，约 868 → 634 行 |
| `src/learning_tasks/service/answers.rs`（新增） | 答案回放、作答及原子结算，251 行 |
| 本文 | 第 4 批结果及四批累计验收 |

行数仅描述落点，收益是修改视图、开练、答题时有独立入口，不声称运行性能改善。
提取前后 58 个原有顶层函数的签名/内容在模块路径与格式归一化后相同，所有字符串字面量
（含 SQL、错误文案）逐字节相同；没有丢失/新增业务函数。`summarize` 仍使用调用方传入事务，
来源检查与最后时限检查位置未变。全局锁序、提交点、旧题目/答案快照、来源归档/撤权、幂等、
跨日掌握/复习、重练及奖励原子性保持。原有 `mastery/rewards/question/source_refresh` 文件未改。

### 实际验证

先对第三批完成后的输入建立基线，再执行上述拆分。相同选择的 **21 项**均通过：
4 个学习单元检查、9 个计划/轮次数据库回归、6 个掌握/来源刷新回归、2 个奖励原子性回归。
用例名称/顺序前后一致，无新增 ignored，无弱化断言；既有测试足以覆盖本次风险，不增加镜像实现测试。
包含编译和独占环境准备/清理的墙钟分别 119.472 / 179.726 秒，编译输入不同，不作性能对照。

可执行命令如下；运行前必须创建自己的独占 PG/Redis 并设置 `DATABASE_URL`、`REDIS_URL`、
`TEST_REDIS_URL`，不能使用已经销毁的历史端口：

```sh
SQLX_OFFLINE=true cargo test --locked --offline --all-features \
  --lib --test learning_runs_handler --test question_bank_handler --test learning_rewards_handler -- \
  --test-threads=1 learning_tasks:: \
  coverage_tail_partial_days_and_persistent_receipts \
  completed_historical_answers_remain_visible_and_immutable \
  final_current_question_permission_check_rolls_back_answer_and_cycle \
  deletion_grace_completion_archives_and_cancels_remaining_cycle \
  optional_schedule_gates_start_and_end_and_rechecks_last_answer_time \
  cycle_candidate_lock_wait_cannot_cross_the_last_scheduled_day \
  longterm_successor_concurrency_and_final_answer_rollback expiry_gate_and_old_keys_replay \
  restored_membership_and_public_access_do_not_revive_old_runs \
  mastery_choice_confirmation_is_cross_day_and_snapshots_restore \
  mastery_retry_order_idempotency_and_hidden_answer_contract \
  mastery_daily_caps_the_initial_queue_and_recovers_the_completed_day \
  mastery_refresh_sources_rebinds_republished_choices_without_losing_progress \
  mastery_refresh_sources_restores_current_public_membership_but_never_renews_revoked_access \
  mastery_refresh_sources_preserves_distinct_custom_targets_and_old_snapshots \
  audit_failure_and_deadline_after_reward_roll_back_everything distinct_completed_units_and_concurrent_day_limit
```

- 最终 `SQLX_OFFLINE=true cargo clippy --locked --offline --all-targets --all-features -- -D warnings`
  通过，70 秒；`cargo fmt --all -- --check`、`git diff --check` 通过。
- 当时第 1/2 批未变的工具测试、Router 隔离回归和精确用例审计复用原有效结果；
  本次验收修正另行重验受影响工具/hooks，旧空间门禁结果不再适用于最终代码；
  第 3 批公共工具兼容性及 OpenAPI 检查的输入未改变，也复用原有效结果。
  本轮源码改动已使 Rust 编译结果失效，因此重新运行上述 Clippy 和受影响学习回归；没有重复全量 Cargo。
- 无 SQL/API/schema、Cargo 依赖或测试 target 变化，不需重新生成 `.sqlx`/OpenAPI/迁移或更改分片清单。
- 基线独占批次 `tsz-learning-batch4-20261105-kmw47s85`（PG 53163 / Redis 53164）；
  候选独占批次 `tsz-learning-batch4-20261105-upyi9052`（PG 54384 / Redis 54385）。
  创建后分别核实 PG data_directory 和 Redis PID；finally 停止进程、核实端口关闭、删除专属数据/日志目录，
  再独立复查无残留。没有 OSS、付费提供方、共享数据或远端写入。
- 再核对第 1 批保护的两个后端二进制 SHA-256，与回收回执相同。未修改/清理任何既有工作区。
  本批原始测试日志、临时源码副本及执行脚本已删除；仅保留本文的脱敏结果。

### 四批累计结果与剩余项

| 范围 | 已完成 / 实测结果 | 仍未验证或剩余 |
| --- | --- | --- |
| 磁盘与编译 | 首次 1.1906 GiB；验收修正另回收 16.8409 GiB，最终 53.7787 GiB；撤回 50 GB 门禁，提供显式计划/回收工具 | 保留共享依赖等非工具范围、活动缓存和未知归属；另有 7.1024 GiB 未选候选。历史故障缺原命令/SHA |
| CI | 两个慢用例复用既有 Router；相同范围本地 17.50/16.58 秒降至 2.87/1.63 秒，断言与清单保留 | 候选全量 CI、Linux 插桩、整体关键路径收益待真实 PR CI |
| 依赖 | 哈希、分页、钱包错误映射归位，函数体保留；边界、幂等、并发及回滚回归通过 | 原生词库 DTO 归属与旧 `src/lexicon/service/v3.rs` 大文件留给既有新版词库方案 |
| 学习职责 | 读取、开练、答题三个职责独立，现有算法与事务保持；拆分前后 21 项通过 | 未做全量数据库套件或真实浏览器前后端联调 |

新 PR/main CI **未运行**，没有上线或部署验收结论。新版智能词库属于既有
“平台级 Agent 与新版智能词库深度评估”聊天 `01a1a6f3-aa53-7bc1-92fb-26bb6e1e4a0e`，
本任务不拆旧 V3 实现或实施新产品模型。

工作区/分支/基线仍为本文开头值。评审使用 `git diff` 并同时查看 `git status --short` 中的新增文件
（普通 `git diff` 不包含未跟踪新文件）。本轮全部写入与后台编译结束；后续仅在取得交付授权后
进入 ship，对精确提交做独立审查并正常执行 hooks、PR/CI；合并与部署仍需各自授权。

## 后续批次的安全构建条件

1. 使用本任务工作区、固定 Rust 1.98.0，沿用用户共享 target、`jobs=2`、`incremental=false`；
   不改全局 Cargo 配置，不同时运行多个重型 Cargo 命令，不通过新 flags/profile 随意制造缓存副本。
2. 优先指定 `--lib` 或 `--test <target>`。不需要实时 schema 的检查显式使用 `SQLX_OFFLINE=true`，
   直接使用原生 Cargo；新增查询导致离线缓存缺失时按 dev-env/隔离数据库流程解决。
   不启动后端试探数据库，不使用真实付费服务。
3. 使用已有计时工具定位失败，避免重试同一未知故障：

   ```bash
   SQLX_OFFLINE=true CI_METRICS_PATH=target/local-build-metrics.jsonl \
     python3 ops/ci_metrics.py run --name focused-check -- \
     cargo clippy --locked --offline --lib --all-features -- -D warnings
   ```

   保存失败原文、确切命令、SHA、工具链、target、退出码及 `du -sk` 前后值；不要输出完整环境或连接凭据。
   先区分 Rust/Clippy、SQLx 离线缓存或连接、锁等待、链接/系统资源错误，再决定下一次验证。
4. 空间不足时停止新增构建，重新审查精确可回收清单。不要执行无范围 `cargo clean`，不要仅按 mtime、
   无进程或包名前缀删除共享缓存；受保护/在用产物和未知归属文件保留，现存工作区可重建测试缓存按上文计划/执行流程回收。

后续批次在本文继续更新实际阶段、剩余依赖和验证结果，不再建立另一份治理台账。
