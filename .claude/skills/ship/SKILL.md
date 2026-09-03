---
name: ship
description: 审查并安全提交、推送 tsz-rust 后端改动：运行 Rust、SQLx、数据库契约与 API 质量门，取得用户确认后提交，对精确 commit 做独立 pre-push code review，再正常推送并创建 PR。用户在 tsz-rust 仓库说「push」「提交并推送」「ship」「开 PR」时使用。
---

# Ship tsz-rust 后端改动

将当前后端工作区安全地送到 feature branch 和对 `dev` 的 PR。分支模型是 feature → `dev` → `main`；部署不属于本技能，dev 合入 `main` 并通过 `main` CI 后使用 `deploy`。

## 红线

- 不直接提交或推送 `main` 或 `dev`。
- 用户确认前不暂存、不提交、不推送。
- 不绕过 `.githooks`，禁止 `--no-verify`、改写 `core.hooksPath` 或等效手段。
- 精确 commit 未通过独立 pre-push code review 时不推送；实现者自查不算独立审查。
- 不使用普通 `--force`，不覆盖远程历史。
- 测试失败、SQLx 缓存不一致、未接受的 P0–P2/正确性/安全问题均阻断推送。

## 时间预算

中小改动（十个文件以内）从开始到 PR 链接控制在 30 分钟上下。超时的来源只有三个：审查扇出太宽、修一轮再审一轮、全量测试重复跑。下文每一步都写了对应的刹车；碰到必须超时的情况（真有 P0–P2）才放开。

## 1. 建立安全基线

1. 检查状态、完整 diff、当前分支、remotes 和近期提交，保留所有无关或用户已有改动。
2. 先 `git fetch origin`，再比较本地与 `origin/dev`。若历史分叉，停止并说明，不重写远程。
3. 当前在 `main` 或 `dev` 时，从正确的 `origin/dev` 基线创建 `feat/<slug>` 或 `fix/<slug>` 分支，并携带工作区改动。
4. 确认 `git config core.hooksPath` 为 `.githooks`；不正确时报告并恢复仓库约定，不能借此跳过钩子。
5. GitHub `origin` 是 PR 与 CI 的权威源；`gitee` 是独立镜像，只有用户明确要求时才同步。
6. 当前分支已有开着的 PR 时，先查它有没有在你不注意时被合并（`gh pr list --head <branch>` + `git fetch`）。已合并就先 rebase 到新 `origin/dev`，**再**开始审查——审查的是最终 SHA，不是马上会变的那个。

## 2. 后端专项审查

根据 diff 给出 ✅、⚠️、❌ 或 ⏭️，并修复明确问题：

| 维度 | 必查内容 |
|---|---|
| 行为与契约 | HTTP 状态、RFC 9457 Problem Details、序列化字段、OpenAPI、幂等重放、revision/并发冲突、调用方兼容性 |
| 数据完整性 | 事务边界、锁与隔离级别、唯一/外键约束、并发竞态、历史数据、归档语义、失败原子性 |
| SQLx 与迁移 | query 缓存、up/down 配对、迁移顺序、已有生产数据兼容性、回退后旧二进制兼容性 |
| 安全 | 认证授权、cookie/token、输入解析、SQL/命令拼接、CORS、secret/env、敏感日志与错误泄露 |
| 测试质量 | 正常、400/401/403/404/409/422、权限、边界、并发、事务回滚、幂等、迁移与集成契约 |
| 可维护性 | 模块边界、错误映射、重复查询、N+1、阻塞操作、无界结果集、死代码和未更新调用点 |

「行为与契约」里有一条本仓库特有：**给响应 DTO 加字段不是纯增量。** admin 前端把 V3 admin word 系列响应（含 sentence-targets resolve）灌进 fail-closed 的 runtime contract，后端先上线会让对应接口在 UI 整体报错。凡是加了响应字段，commit/PR 正文必须写「部署顺序：前端先 `sync:openapi`」，`docs/frontend-integration.md` 加一节。

纯文档改动可跳过不相关运行时维度，但仍检查契约一致性和链接。不要把前端的 typecheck、lint、coverage、SEO 或 UI 规则带入后端流程。

## 3. 运行后端质量门

代码、配置、SQL 或契约改动默认运行：

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

- 全量测试需要本地 Postgres/Redis；先按仓库说明启动依赖。环境确实不可用时报告唯一阻塞，不以快速测试代替全量测试后宣称通过。
- **全量测试只在最终 SHA 上跑一次**；迭代期间只跑受影响的测试二进制（`--lib`、`--test <file>`、契约测试）。全量门禁用后台任务跑，与第 6 步的独立审查并行，别串行等。
- 后台任务的结束通知里的 exit code 是外层脚本的，不是门禁的。日志里自己写 `### exit=N` 标记并据此判断，脚本最后一句 `exit $rc`。
- 做突变检查（改坏代码验证测试会红）后，用 `cp` 还原或还原后 `touch` 文件。`mv` 备份回去会保留旧 mtime，cargo 会沿用突变版二进制，全量测试就在改坏的代码上跑。`git diff` 干净只说明源码对，不说明 target/ 里的二进制对。
- 修改 `sqlx::query!`、schema 或 migrations 时运行 `cargo sqlx prepare -- --all-targets --all-features`，审查 `.sqlx/` diff，并重新执行受影响门禁。
- 修改 API 时验证 `docs/openapi.json` 与实现、测试和前端精简契约同步；不手工伪造生成结果。
- 运行 `git diff --check`，确认无调试代码、临时文件、遗留 TODO、未跟踪生成物或无关改动。

## 4. 用户确认门

汇报审查表、全部命令结果、已修问题、剩余风险和拟用 conventional commit message。存在 ❌ 时先解决；用户明确确认前停止，不提交。

## 5. 提交

只暂存本次目标文件并正常提交，让 pre-commit 刷新并暂存 `.sqlx/`、运行全 feature clippy。使用 conventional commit（单一 type，别写 `docs+test` 这种复合 type），并追加：

```text
Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
```

钩子失败就修复，不绕过。提交后要求工作区干净，并确认 `.sqlx/` 自动变化已包含在 commit 中。

审查后需要修的东西，**只允许一轮**，落成一个 commit：未推送前用 `--amend` 并进上一条修复提交，不叠 `fix review` 小提交。修复只改运行时行为以外的内容（文档、测试、契约文案）时不必再过第 4 步；改了运行时行为则回到第 4 步。

## 6. 独立 pre-push code review

1. 记录 `review_base=origin/dev`、`review_sha=$(git rev-parse HEAD)` 和已通过的门禁结果。
2. 在新上下文运行只读独立 reviewer：调用 `code-review` skill，target 传 `review_base...review_sha`，effort 用 `high`（不用 max：max 会给每条 P3 候选各配一个核实 agent，中小 PR 上等于把审查时间乘三）；该 skill 不可用时创建独立 reviewer 子任务。传入原始目标、base、精确 SHA、仓库规则和测试结果，不传入实现者的审查结论。finder 全部并行启动，不要等一个再开一个。
   `/code-review ultra` 是用户触发且计费的多 agent 云端审查，实现者不得自行发起；确有需要时请用户手动运行。
3. reviewer 只检查 `review_base...review_sha`，不得编辑文件；报告有文件/行号证据的正确性、回归、安全、数据完整性、并发、性能、API/迁移契约和测试缺口。
4. **核实预算**：只给 P0–P2、正确性、安全、数据完整性、并发、契约行为类候选配独立 verifier。P3 的清理/复用/文案/测试风格候选不核实，原样列进报告，处置为「留作后续」；例外只有一种——本 PR 自己新加的行上一条明显写错的契约文案，顺手改掉。
5. P0–P2、正确性或安全发现阻断推送。用证据处理误报；非正确性建议记录不阻断。
6. **修复后的再审查按增量来**：先 `git diff <旧 sha> <新 sha> --stat -- src migrations .sqlx`。为空（只动了文档/测试）就只对 `<旧 sha>...<新 sha>` 跑一个 delta reviewer，并在报告里写明「运行时代码与已审 SHA 逐字节相同」；不为空就只对改动的运行时 hunk 跑完整角度，其余走 delta reviewer。总共最多两轮；第二轮再出 P3 一律留作后续，不开第三轮。
7. 只有当前 SHA 明确无阻断发现时才允许推送，并向用户报告 reviewer、base、SHA 与结论。reviewer 不可用时停止。

## 7. 推送与 PR

1. 正常运行 `git push -u origin <branch>`，让 pre-push 执行全 feature clippy 和快速单元测试；失败就修复并回到独立审查，不绕过钩子。代理掐 SSH 或 HTTPS 时按 `tsz-rust-repo-workflow` 记忆里的判据换传输方式，别猜。
2. 使用 `gh pr create --base dev`（分支已有开着的 PR 时用 `gh pr edit` 更新标题正文）。base 必须是 `dev`：直接对 `main` 开会让 dev 落后 main 一个 squash 提交，得再补一次同步。正文包含目标、迁移/契约影响、**部署顺序**、独立审查结论与留作后续的清单、质量门结果与回退注意事项。`gh` 走代理报 EOF 时用 `env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy gh ...` 直连。
3. PR 的 GitHub CI 会在 Postgres/Redis 服务下再次运行格式、全 feature clippy 和完整测试。CI 全绿才可合并；PR 自动审查是 push 后的第二层，不替代第 6 步。
4. 返回 PR 链接；缺少 GitHub CLI/认证时止步于已推送分支并说明。
