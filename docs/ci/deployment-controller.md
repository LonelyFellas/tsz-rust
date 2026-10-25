# 测试服发布控制器：阶段 C 本地验收

## 当前交付边界

阶段 C 实现 `ops/deploy_test.py` 的 `plan/apply/status/resume`、持久化发布事务、故障恢复及正式 HTTPS 入口验收。当前只完成本地隔离测试；**未对 `tshb-test` 执行新控制器的发布或恢复，未启用自动 CD**。提交、推送、PR、实际发布和后续手动演练分别需要对应授权。

现行实际发布入口仍是 [部署 runbook](../../.agents/skills/deploy/references/runbook.md)。阶段 D 在独立授权的手动演练通过后，再切换统一入口并移除重复执行路径。本说明中的命令用于解释接口，不代表执行真实发布的授权。

本次实现基于 `462c0ef6646df1fb38fd6fc9344d84f62fd4e50b`。阶段 B 已提供精确 CI 制品与隔离 smoke；本阶段复用其 manifest、基线采集、迁移区间计算和证据格式，不增加自动部署 workflow，不缩减已有 CI 检查。

## 模块职责

| 模块 | 职责 |
| --- | --- |
| `ops/deploy_test.py` | 校验计划、原子保存 intent/结果、按顺序执行步骤、处理中断恢复 |
| `ops/deployment_transport.py` | GitHub 精确来源门禁、固定 `tshb-test` SSH 传输；工具源码与二进制分别处理 |
| `ops/deployment_host.py` | 远端锁、状态、备份、安装、候选进程、正式服务验收、恢复和清理 |
| `ops/deployment_auth_smoke.py` | 真实 HTTPS/CookieJar 鉴权验收及正式 MainPID/摘要/监听核验 |
| `ops/deployment_business_smoke.py` | 独占合成用户、草稿词表持久化验证、精确清理和只读残留复查 |
| `ops/deployment_manifest.py` | accepted manifest v1/v2 校验、创建和备份恢复 |

## 输入与来源门禁

下载内容放在每次新建的空目录中。工具从受信任源码目录执行，不从下载目录导入 Python 模块。bundle 包含：

| 文件 | 含义 |
| --- | --- |
| `tsz-rust` | GitHub CI 产出的原始 Linux release 二进制 |
| `tsz-rust.manifest.json` | 阶段 B 的制品 manifest，绑定 SHA、tree、CI run、生产 attempt、构建 profile 和二进制摘要 |
| `artifact.json` | GitHub 对精确 artifact ID 返回的元数据 |
| `release-smoke-receipt.json` | 最新成功 verification attempt 的阶段 B 验收回执 |
| `deploy-info.json` | 同一制品在隔离 Linux 环境执行 `deploy-info` 的内嵌迁移清单 |
| `migration-receipt.json` | 存在新增迁移时，针对实际已部署基线的阶段 B 演练回执 |

计划绑定上述文件路径、二进制和回执摘要、工具源码摘要、部署 UUID、已部署基线、完整迁移区间及恢复策略。文件应保留到事务终结，不能在中途替换工具或计划。

GitHub 门禁要求受信工具所在 checkout 为干净 `main`，HEAD/tree 与候选一致；候选必须仍是 GitHub 当前 main。CI 必须是同一仓库 main 的 `push` 或 `workflow_dispatch`，最新精确 run/attempt 已完成且 `success`。门禁重查精确 artifact ID、有效期、来源和最新 smoke 回执原始字节；拒绝 PR 制品、过期制品、失败或未知 CI、旧回执及来源漂移。

生产 attempt 与 verification attempt 分开记录，允许仅重跑失败 smoke 后消费之前已成功构建的制品。部署前另从精确 Git 提交读取迁移 SQL、计算 SHA384，与计划区间对账，避免仅相信下载的 `deploy-info.json`。

## CLI

下面命令均从受信工具 checkout 根目录执行；`--state-dir` 是全局参数，必须放在子命令前。省略时使用 `~/.config/tsz-rust/controller`。

生成计划会访问 GitHub 和测试服的只读基线/预检，不上传文件、不停服、不执行迁移：

```bash
python3 ops/deploy_test.py --state-dir /absolute/state-dir plan --bundle /absolute/bundle --output /absolute/plan.json
```

有新增迁移时追加 `--recovery-policy /absolute/recovery-policy.json`。审批文件格式见下一节。当前阶段 C 不执行真实 `apply` 或 `resume`；只有后续获得发布或事故恢复授权后才使用相应接口：

```bash
python3 ops/deploy_test.py --state-dir /absolute/state-dir apply --plan /absolute/plan.json
```

查看状态只读本地文件，不访问服务器：

```bash
python3 ops/deploy_test.py --state-dir /absolute/state-dir status --deployment-id <deployment-uuid>
```

恢复会观察远端并可能停服、清理本次 fixture、回退及重启，属于写操作：

```bash
python3 ops/deploy_test.py --state-dir /absolute/state-dir resume --deployment-id <deployment-uuid>
```

CLI 只有 `accepted` 返回 0；`recovered` 表示发布失败但已恢复旧版本，仍返回 1。`blocked`、`manual_required`、非法计划或未知现场也返回 1。不能用命令执行成功替代检查事务状态。

## 状态与顺序

本地 `<state-dir>/<deployment-uuid>/state.json` 保存完整 plan、`plan_sha256`、`phase`、`status`、`pending`、`completed`、`mutation_started` 和带时间的事件。JSON 以临时文件、fsync、原子替换持久化，文件权限 0600。每个动作先写 intent，完成后再写结果。

本地事务使用 writer flock；`<state-dir>/active/owner.json` 阻止同一状态目录并发发布。远端使用 `/opt/tsz-rust/deploy.lock` 的 owner、事务目录 `/opt/tsz-rust/deployment-transactions/<deployment-uuid>` 和 operation flock。危险子命令继承 operation 锁 fd，避免调用端消失后恢复流程与仍在执行的动作并行。远端锁同时约束使用其他本地状态目录的调用者。

前向步骤固定为：

1. `backup`：备份旧二进制与 manifest，验证两者一致，fsync 文件与目录后才记录完成。
2. `upload`：接收精确候选并核对 SHA256。
3. `stop`：最后复核 main/CI/基线，持久化 mutation 边界后停正式服务。
4. `install`：原子替换二进制，撤掉旧 accepted manifest。
5. `candidate_start`：在 `127.0.0.1:18383` 启动带部署 owner 的 smoke 进程。
6. `candidate_verify`：候选健康、迁移账本与 HTTPS 鉴权验收。
7. `formal_start`：结束候选，启动正式 systemd 服务。
8. `formal_verify`：正式进程、摘要、监听、readiness、HTTPS 鉴权复验。
9. `business_verify`：通过正式 HTTPS 创建、重新读取、修改并再次读取独占草稿词表，随后精确清理。
10. `accept`：所有验收及清理完成后，写入 v2 accepted manifest。
11. `unlock`：按 owner 释放锁，保留事务证据。

main 只在 mutation 边界之前跟踪。停服前 main 或基线变化会阻断；一旦开始变更，事务固定候选身份，避免恢复时被后续 main 提交改变目标。

| 状态 | 意义 |
| --- | --- |
| `running` | 前向动作执行中，可能存在未确认的 `pending` 动作 |
| `recovering` | 已进入恢复，不再继续前向安装 |
| `accepted` | 新版本、正式验收和临时数据清理全部通过 |
| `recovered` | 发布失败，旧版本、账本、manifest 和鉴权已复验 |
| `blocked` | 尚未变更正式运行版本，已终止并释放本次锁 |
| `manual_required` | 无法证明归属、清理或恢复安全；保留锁和定位信息 |

释放 active/远端 owner 锁采用原子 rename 到事务归档位置，避免逐文件删除在中断后留下缺 owner 的锁。过早中断导致 owner 不完整时保守阻断，不自动抢锁。

## 中断与迁移恢复

断线或超时不代表动作没有执行。`resume` 校验原计划和工具身份、取得同一 owner 的 operation 锁后观察真实现场，不重放未确认的前向步骤：

- 尚未变更正式版本：终止事务，清理本次锁。
- 已落盘且身份完整匹配的 accepted/recovered 终态：只完成释放锁。
- 其他已变更现场：停止本次候选/正式进程、清理本次 fixture，再根据账本和策略恢复旧版本。
- owner 不同、工具改变、账本不一致、恢复失败或未知现场：保留 `manual_required`，禁止猜测性继续。

候选通过环境中的部署 UUID 标记找回，即使创建进程后 PID 尚未落盘也不会按名称批量杀进程。恢复先核精确账本；只有新增版本为审批区间前缀、历史 checksum 不变、迁移都成功时，才可能执行有边界的 down。旧二进制和 manifest 在账本恢复后还原，再做正式 readiness/鉴权复验。

**阶段 B 的合成数据演练不授权测试服真实数据 down，也不证明 PITR 已就绪。** 非空迁移区间必须提供独立批准的恢复策略：

- `verified_down`：演练已通过 bounded down 和旧制品回读；真实环境恢复另有明确批准。控制器仍执行实际账本和数据守卫。
- `manual`：升级链路已验证，但 down 被原子拒绝或明确采用人工恢复。失败后若已迁移，控制器停在 `manual_required`，不自动 down。

策略 JSON 包含 `kind`、`baseline_sha`、`candidate_sha`、`baseline_version`、`candidate_version`、`approval_evidence_sha256`。审批 JSON 存于受保护发布端的 `~/.config/tsz-rust/recovery-approvals/<approval_evidence_sha256>.json`；文件自身 SHA256 必须匹配文件名，正文包含前五个字段及 `baseline_artifact_sha256`、`candidate_artifact_sha256`、`migration_receipt_sha256`、`migration_interval_sha256`。区间摘要按排序 key、无多余空白的 canonical JSON 计算。放置文件属于落实既有审批，不能由生成 plan 自动授予。

## 正式入口验收和数据清理

预检只解析 `.env`，不执行 shell；以内存摘要核对正式进程与文件中的数据库、Redis、鉴权密钥及 Cookie/登录奖励配置，事务期间变更会阻断。每次 systemd 启动前核对固定摘要，要求唯一环境文件为 `/opt/tsz-rust/.env`、WorkingDirectory 为 `/opt/tsz-rust`，拒绝相关配置的冲突 Environment 覆盖；候选和 down 子进程使用同一份已校验环境。要求 `COOKIE_SECURE=true`、基线鉴权所需表字段存在、TLS 正常，且 Python 能生成 bcrypt 随机凭据。`LOGIN_REWARD_ENABLED` 可以为 true 或 false，保持业务配置；它仍参与进程/文件身份及事务漂移校验。登录奖励由独立领取接口结算，当前鉴权和草稿业务烟测不调用该接口；钱包和资金流水等副作用仍由清理守卫阻断。完整业务清理 schema 在候选完成迁移后才检查；鉴权 fixture 使用实际 schema 的外键与已有非外键引用守卫清理，因此旧基线和 down 后不依赖候选新增表。目标 Ubuntu Python 3.10 使用 libcrypt bcrypt；Python 3.13 移除了 `crypt`，无法生成时预检阻断，不安装包或改用固定共享口令。

正式验收前后核对 systemd MainPID、磁盘和 `/proc/<pid>/exe` 摘要，以及 `127.0.0.1:8383` 的唯一监听归属；拒绝残留 smoke 配置、错误 Cookie 配置和进程变更。HTTPS 验收沿用 Secure/HttpOnly/SameSite/Path、refresh 轮换与 logout 规则。

每个 auth/business/recovery fixture 从部署 owner 派生独占 UUID，创建前先证明 ID/email/phone 均不存在。密码和 bcrypt hash 仅在本次进程内存使用；持久化状态只保存归属字段及 hash 指纹，用于中断后核对，不保存口令、Cookie、token 或连接串。固定凭据仅供独立测试 fixture，正式 host 路径始终显式传入随机凭据。

业务验收只创建一份 draft/custom-item 词表，不发布、不购买音频、不发奖励。创建后用新请求读取，修改后再用新请求确认 revision、内容和原备注持久化。

清理先保存精确词表 ID，在用户行锁、词库 advisory 锁及词表行锁下检查归属和外部引用。用户入站外键采用明确依赖白名单，头像上传、教师申请/认证文件等其他引用在删除前拒绝；发现资金、音频、学习或他人引用等超出 fixture 范围的副作用时也拒绝删除。按精确资源删除后，另开只读事务检查实体和引用无残留，并审计该账号精确 Redis 限流键。共享 IP 限流桶属于正常服务状态，不清空 Redis，也不删除共享 IP key。清理失败不会创建 accepted manifest；恢复仍失败时保留锁和状态。

v2 accepted manifest 记录生产 attempt、verification attempt、artifact ID、二进制摘要和验收回执摘要。旧 v1 继续可校验、备份和恢复。

## 本地验收证据

可重复的工具回归：

```bash
python3 -m unittest discover -s ops -p 'test_*.py'
```

本轮覆盖所有前向步骤故障、动作后 runner 丢失、锁冲突、来源变化、恢复失败后重试、已完成响应丢失、实际 CLI 异常路径及非法 v2 manifest 输入。事务联测实际使用文件、原子状态、锁、备份、安装和 manifest；systemd、SSH、进程、HTTP 与数据库 IO 用替身，不等于测试服发布成功。

另在 Docker internal network 中使用阶段 B 的真实 Linux 制品、PostgreSQL 18.6、Redis 6.0.20 验证 HTTP 持久化、随机账号登录、精确清理、正常 fixture 不变及外部引用阻断：

- [真实业务与清理证据](evidence/2026-10-25-stage-c-isolated-business.json)
- [随机凭据证据](evidence/2026-10-25-stage-c-random-credentials.json)
- [本轮最终验证与清理回执](evidence/2026-10-25-stage-c-validation.json)

这些本地证据均为 `release_eligible=false`。真实 SSH/systemd/nginx 端到端、测试服真实迁移恢复和人工事故处置仍须后续独立授权验证；当前不能以单元测试或隔离业务验证替代。
