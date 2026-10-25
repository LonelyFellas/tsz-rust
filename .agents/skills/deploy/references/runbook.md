# tshb-test 后端发布：控制器操作手册

唯一发布入口为精确候选携带的 `ops/deploy_test.py`。授权、执行者与硬门禁见 [deploy 技能](../SKILL.md)；完整接口/状态见 [控制器说明](../../../../docs/ci/deployment-controller.md)。本手册不再维护独立的 SSH/rsync/systemctl/manifest 写入步骤。目标为 `LonelyFellas/tsz-rust` 当前 GitHub main 的成功 CI 制品，服务固定为 `tshb-test` 的后端；服务器无源码 checkout，不编译。

2026-10-25 的[真实正常发布证据](../../../../docs/ci/evidence/2026-10-25-stage-d-manual-deployment.json)已通过。**真实回退尚未演练**，自动 CD 未启用；每次发布仍须重新执行门禁，历史成功不代替现场。

## 1. 先辨认新发布还是原事务恢复

- 新发布须有本次后端授权。只读准备只运行来源查询、备份适用性核对和 `plan`；不运行 `apply`/`resume`。
- 恢复已有事务先找到原 source checkout、plan、deployment UUID 和本地 `state.json`，使用同一 `--state-dir`。不另开 UUID，不重跑 `apply`，不改变工具或计划来绕过摘要核验。
- 默认本地状态目录为 `~/.config/tsz-rust/controller`；事务状态为 `<state-dir>/<deployment-id>/state.json`。可指定其他目录，但同环境发布应复用同一目录；远端 owner 锁仍是所有调用者共用的互斥边界。
- 本地 active、旧手册的 `~/.config/tsz-rust/deploy.lock` 或远端 `/opt/tsz-rust/deploy.lock` 存在时，先只读定位 owner/state/现场，不删除或抢锁。旧 `state.env` 事务不由新控制器自动接管，必须另作受信工具与现场恢复方案。

下面代码只说明执行方式，阅读不授予写入授权。每个 shell 块独立 fail-fast；跨调用只传已保存的文件和绝对路径，不依赖前一 shell 的变量或 `source state.env`。

## 2. 准备独立干净的 main checkout

从目标仓库根目录运行以下**本地**准备。新建临时 clone，不改变原工作区分支或用户文件。输出的绝对路径作为本次 session 的固定来源；中断恢复复用它。

```bash
set -euo pipefail
python3 -I - <<'PY'
from pathlib import Path
import json, os, subprocess, tempfile
origin = subprocess.check_output(['git', 'remote', 'get-url', 'origin'], text=True).strip()
source = subprocess.check_output(['git', 'rev-parse', '--show-toplevel'], text=True).strip()
session = Path(tempfile.mkdtemp(prefix='tsz-backend-deploy-'))
checkout = session / 'source'
subprocess.run(['git', 'clone', '--shared', '--no-checkout', source, str(checkout)], check=True)
subprocess.run(['git', '-C', str(checkout), 'remote', 'set-url', 'origin', origin], check=True)
subprocess.run(['git', '-C', str(checkout), 'fetch', 'origin', 'main'], check=True)
subprocess.run(['git', '-C', str(checkout), 'switch', '-C', 'main', 'origin/main'], check=True)
sha = subprocess.check_output(['git', '-C', str(checkout), 'rev-parse', 'HEAD'], text=True).strip()
tree = subprocess.check_output(['git', '-C', str(checkout), 'rev-parse', 'HEAD^{tree}'], text=True).strip()
assert not subprocess.check_output(['git', '-C', str(checkout), 'status', '--porcelain'], text=True).strip()
with (session / 'session.json').open('x') as output:
    json.dump({'checkout': str(checkout), 'git_sha': sha, 'git_tree': tree}, output, indent=2)
os.chmod(session / 'session.json', 0o600)
print(session)
PY
```

保留源仓库对象（clone 使用共享对象库），不清理该源 checkout/cache。把 session 路径记入主任务记录；下载、计划和回执放在 `source` 之外，保持部署 checkout 干净。不得将下载目录作为 Python 导入目录或从中执行工具。

## 3. 固定精确 CI、制品与 bundle

以下准备在新 source checkout 中执行，或使用其工具的绝对路径。只选择 `.github/workflows/ci.yml` 对应的 `CI`，同仓 main 的 `push`/`workflow_dispatch`；按 created_at/id 选择该 SHA 最新 run，再核对最新 attempt completed/success。失败、无记录、过期、skipped/neutral、运行中、GitHub 查询异常或 main 前进均停止；不要在本地自己编译候选，也不要把 PR 的绿色结果当作 main 证据。

1. 从 `session.json` 读取固定 SHA/tree，通过 GitHub API 查询最新 CI 和该 run 的 artifact 元数据。
2. 精确找到 `release-smoke-<sha>-attempt-<verification-attempt>` 回执，要求唯一、未过期。在**新建空目录**下载，仅作为 JSON 数据读取。回执必须 passed、cleanup.complete=true，8 项检查全部 passed。
3. **以回执中的 `artifact_id` 和 `ci.run_attempt` 选择二进制**，不要把 verification attempt 当作生产 attempt。重跑失败 smoke 可以消费之前成功构建的旧 attempt 制品。artifact 元数据须唯一、未过期，name 为 `tsz-rust-x86_64-linux-gnu-<sha>-attempt-<producer-attempt>`，workflow_run 与 SHA/run 对齐。
4. 二进制及随包 manifest/sha256 下载到另一个新空目录；核对精确 SHA/tree/run/生产 attempt、二进制大小与 SHA256。使用 source checkout 的 `ops/release_artifact_manifest.py` 和 `ops/release_artifact_smoke.py` 的 `verify_artifact_record`，不导入下载目录中的代码。回执的 source/ci/artifact_id/digest/verification attempt 必须与制品对应。
5. bundle 最终保留下表文件。原始下载目录与归属记录保留至事务结束；不覆盖现有 bundle，不能给旧计划换文件。

| bundle 文件 | 来源及约束 |
| --- | --- |
| `tsz-rust` | CI 原始 Linux x86_64 release/default-features 制品，按其 manifest 校验 |
| `tsz-rust.manifest.json` | 原 CI 制品 manifest，SHA/tree/run/生产 attempt 与固定候选一致 |
| `artifact.json` | GitHub 对回执中精确 artifact ID 的最新原始元数据 |
| `release-smoke-receipt.json` | 最新成功验证 attempt 的原始回执字节，不重排 JSON 后替换 |
| `deploy-info.json` | **同一已校验制品**执行 `deploy-info` 的输出；该命令在加载配置/连接数据库前返回 |
| `migration-receipt.json` | 有新增迁移时针对实际已部署基线生成的隔离升级/恢复回执，无新增迁移时不要求 |

Linux 制品不能直接在 macOS 执行。按宿主选择原生 Linux，或已缓存不可变 app 镜像的独立 `network none` 容器读取 `deploy-info`；只挂载单个已校验二进制为只读，不传 `.env`、数据库 URL 或业务网络。创建前记录唯一 owner 和精确容器 ID，执行后验证退出码、解析 JSON，并按 ID/归属删除复查。不能仅从源码拼造 `deploy-info.json`。控制器还会从精确 Git 提交读取迁移 SQL 并对账。

控制器的 `check_plan`/`GithubGate` 在 plan、apply 入口及停服前承担最终来源门禁。准备脚本的判断不代替这些检查。不得手工修改受信工具来允许任意 SHA 或降低校验。

## 4. 只读基线、配置与恢复点

计划前确认 SSH `tshb-test` 可连接且主机密钥校验开启。基线由 `ops/deployed_baseline.py` 复用采集：正式 manifest、磁盘与运行进程摘要、PID 稳定、无部署锁、只读完整迁移账本。摘要未知、manifest 缺失/损坏或不匹配时停止，不补造来源。

由执行者在 apply 前完成并记录以下证据；**控制器不会自动证明所有这些外围条件**：

- 写前数据库恢复点按 [PostgreSQL 备份恢复规范](../../../../docs/postgresql-backup-restore.md)验证，检查权限、hash、快照时间、候选/现网账本及兼容性、恢复工具与容量；记录是否覆盖当前风险。恢复环境必须隔离业务网络，并为数据、索引和 checksum 校验临时空间留足容量，不机械照抄示例中的固定 tmpfs 大小。备份不在 bundle 内，不能误删。controller 的 `backup` 只复制当前二进制和 manifest，不生成数据库备份。
- 无新增迁移也需确认旧二进制可读取本次写入格式。只有实际账本仍等于基线才按 binary/manifest 恢复；该流程不自动恢复数据库或覆盖快照后的正常写入。
- 有新增迁移：先按阶段 B 的 `ops/migration_compatibility.py` 对实际基线做隔离演练，提供 `migration-receipt.json` 和独立批准的恢复策略。候选区间缺少 down，或 up/down 非事务性时，当前控制器返回 `unsupported_recovery_interval`；`manual` 策略不能绕过这一能力限制。`verified_down`/`manual` 及受保护审批文件格式见[控制器说明](../../../../docs/ci/deployment-controller.md#中断与迁移恢复)。策略摘要绑定新旧 SHA、制品、账本/区间和回执；生成 plan 不能自行授予真实数据 down/PITR 权限。
- 如本次授权要求“新增迁移或恢复点不适用即停止”，必须先停止报告，不用更宽策略继续。有 down 文件、合成数据演练成功或数据库备份存在都不等于真实数据可自动 down。
- 保持 `.env` 和业务开关。正式进程、文件及 systemd 的 EnvironmentFiles/WorkingDirectory/相关 Environment 覆盖必须与候选配置一致。`COOKIE_SECURE=true`；`LOGIN_REWARD_ENABLED` 可为 true/false，但身份漂移一律拒绝。
- TLS 的证书/私钥匹配、系统 CA/域名有效，已有 nginx HTTPS `/api/v1/` upstream 指向正式 `127.0.0.1:8383`。域名根 `/healthz` 可能是前端，正式健康只读本机后端精确 JSON。不改 nginx、不降级 Cookie、不使用 `-k`。
- 检查服务器空闲磁盘能容纳旧备份、incoming、新安装临时文件和日志，临时端口 18383 无冲突；Python bcrypt、psql/Redis 等能力可用，失败就阻断，不现场安装包或清共享缓存/历史备份。
- 保存 `.env`、nginx、web/admin manifest 指纹及预选正常实体的精确 ID/摘要；不打印内容、DSN、密码、Cookie、token 或私钥。并发正常业务可能改变数据，样本变化需先核实，不凭变化猜测删除。

生成 plan（将占位路径替换为本次持久记录的绝对路径）：

```bash
set -euo pipefail
python3 /absolute/session/source/ops/deploy_test.py --state-dir /absolute/controller-state plan --bundle /absolute/session/bundle --output /absolute/session/plan.json
```

新增迁移时在 `plan` 后按批准策略追加 `--recovery-policy /absolute/recovery-policy.json`。`--state-dir` 是全局参数，必须放在子命令前。缺省时使用默认目录；后续所有命令复用相同目录。plan 不创建远端锁、不上传/停服/迁移，但会查询 GitHub、只读数据库及执行 TLS 预检。它只读检查不是发布授权。

核对 plan 的 environment、UUID、source/ci、制品/回执摘要、baseline、迁移区间、recovery_policy、tools 摘要和绝对文件路径；保存原始 plan、恢复点适用性及授权边界。apply 前 GitHub main 或基线已变化则重新准备，不能只更新 JSON 中的 SHA。已部署同 SHA 时控制器返回 `already_deployed_verify_existing`：仅按受信来源只读核对 manifest/进程/账本，后续完整 smoke 的临时数据操作需属于已有验收授权，不盲目重部署或伪称完成本次发布。

## 5. 使用 apply 执行新发布

只有本次后端发布及必要测试数据/恢复范围已明确授权、plan 和外围恢复点门禁全部通过才执行：

```bash
set -euo pipefail
python3 /absolute/session/source/ops/deploy_test.py --state-dir /absolute/controller-state apply --plan /absolute/session/plan.json
```

完整顺序由控制器管理，不把下表拆成另一套远端命令：

| 阶段 | 控制器负责的结果 |
| --- | --- |
| acquire | 本地 writer/active 与远端 owner/operation 锁；原 plan/tool 身份落盘；不抢锁 |
| backup → upload | 当前二进制/manifest 成组校验和 fsync；精确传输及 SHA256 核验 |
| stop → install | 停服前最后 main/CI/基线校验；持久化 mutation 边界后停正式服务，撤旧 manifest，原子替换 |
| candidate_start → candidate_verify | loopback 18383、smoke-only 禁 worker；健康/账本及 TLS 鉴权、合成账号清理 |
| formal_start → formal_verify | 停候选，检查配置后启动 systemd；正式 PID/exe/磁盘摘要/8383/HTTPS 鉴权通过 |
| business_verify | 独占随机账号，草稿创建、修改与新请求回读持久化；精确清理和复查 |
| accept → unlock | 清理与正式进程门禁通过后创建 v2 accepted manifest；最后同 owner 解锁 |

每个动作先保存 intent，再保存结果；state 文件不含明文凭据。停服前 main 前进会阻断；mutation 开始后固定候选完成验收或恢复，不追新 main。`accepted` 是唯一成功状态，不能仅靠命令退出、服务 active 或某段 JSON 的 SHA 判断。

后台运行保留命令句柄、真实退出码与 owner；不要启动第二次 apply。正常跟进约每 60 秒报告有变化的进度；不通过不断查询 CI 或重复 SSH 写操作判断进展。控制器失败会先观察现场并恢复/阻断；原动作仍持 operation 锁时不并行恢复。

## 6. status、resume 与人工阻断

`status` 只读原本地事务，不连接服务器：

```bash
set -euo pipefail
python3 /absolute/session/source/ops/deploy_test.py --state-dir /absolute/controller-state status --deployment-id '<deployment-uuid>'
```

`resume` 是原事务的**写入恢复入口**，可能停进程、删本次合成数据、有界 down、还原 binary/manifest 及重启。核对原恢复授权、UUID、state/plan/tool 身份和现场后执行：

```bash
set -euo pipefail
python3 /absolute/session/source/ops/deploy_test.py --state-dir /absolute/controller-state resume --deployment-id '<deployment-uuid>'
```

上例引号内的 UUID 占位符必须替换为真实 UUID。不得修改已保存计划、owner、工具或状态来伪装原事务；工具变化或丢失、归属不明、旧 state.env 及异常现场须保留证据并提出具体恢复方案，所需新工具/扩大数据影响另行授权。

| 状态 | 判定与后续 |
| --- | --- |
| `accepted` | 新版验收和清理通过；恢复若只缺解锁，先核对实际身份再完成原锁释放；不重新发布 |
| `recovered` | 旧版、原账本/manifest 和 HTTPS 鉴权已复验；发布仍失败，CLI 返回 1；核对清理和解锁 |
| `blocked` | 尚未改变正式运行版本，原事务终止；查明原因后重新准备，不复用旧计划前进 |
| `manual_required` | 归属、配置、账本、清理或恢复不能证明安全；保留锁/state/backup，停止有风险写入并报告 |
| `running` / `recovering` | 不等于成功；先确认原命令是否仍运行，不启动并行恢复 |

恢复先停止本次候选与正式进程、清理本次 fixture，再核账本：账本等于基线才直接还原旧二进制/manifest；仅对已批准 `verified_down` 的精确区间，在历史记录一致、新增版本为候选区间前缀且数据守卫通过时才有界 down，账本恢复后才换回旧版。`manual` 或守卫拒绝时不自动 down，也不自动向现网恢复 dump。

**真实回退尚未演练**。正常发布与隔离恢复测试不自动授权测试服故障注入。accepted 后要求回退属于新的回退计划，不能篡改终态让 `resume` 撤销已验收发布。任何未完成恢复/清理不得报成功，不能为了移除锁删除正常数据或修改保留策略。

## 7. 独立验收、清理与保留

发布命令完成后，以原 plan/UUID 独立只读核实真实服务，不能只转述 state：

- 正式 manifest 验证、运行与磁盘 SHA256、systemd MainPID、唯一 loopback 8383 监听、health/ready 精确 JSON、配置身份一致；正式进程没有 smoke-only。候选 owner 进程/18383 监听均退出。
- 候选与正式各完成无 Cookie refresh 401 Problem Details、login 200、Secure/HttpOnly/SameSite=Lax/Path=/api/v1/auth、refresh 轮换、logout 204 且 Cookie 清除。真实正常演练中的结果须与状态步骤一致。
- 草稿创建→新请求读取→修改→新请求读取，revision、内容及备注持久化。只用本次独占合成账号，不使用共享凭据；不发布词表、不领取登录奖励、不购买音频。
- 精确 fixture UUID、create_key 和已保存词表 ID 的实体及专属依赖无残留：账号/角色/会话、词表/词项/library/幂等登记、资金/音频/outbox/审计及上传引用。入站外键与资金守卫必须通过；另连只读会话复查，核实精确 Redis 账号键不存在。不删除共享 IP 限流桶，不清空共享库/Redis。
- 配置/前端/nginx 指纹、预选正常实体样本无意外变化；发生引用/资金副作用或无法证明归属时停止风险删除，保留必要 IDs 和阻断信息。清理不是撤回/归档或等宽限期。
- 实际迁移账本与计划一致、备份组匹配原基线，保留状态/plan/工具来源、download bundle、owner 释放回执和精确 backup。检查 root 属主，不把本机 UID/GID 传给远端运行文件。
- 两端 owner 锁已释放；临时探针、凭据材料、测试资源按归属清理并独立复查。运维备份、已验收恢复点、事务审计和既有日志保留，不能纳入临时测试删除范围。

完整报告写明：实际旧/新 SHA、tree、CI run 与生产/验证 attempt、artifact ID/摘要、accepted 时间、事务 UUID、备份/恢复点及适用性、检查与未验证项；下载/上传/总耗时、server build=0、压缩包和二进制字节数；停服时间若只按步骤时间估算须标明上界。真实回退未发生时明确写“未演练”，不以正常成功代替。

本手册不创建自动 CD，也不改变现有 PR/main 两轮 CI。后续自动化只能调用同一控制器，并须有独立授权和同等门禁。
