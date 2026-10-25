# 阶段 D 发布演练准备方案（历史基线）

进展补记：2026-10-25 奖励门禁修复已合入，恢复点已通过本机 PG18 隔离验证，随后真实正常发布 `65c1679` 已验收，见[阶段 D 演练证据](evidence/2026-10-25-stage-d-manual-deployment.json)。当前统一操作入口是[部署 runbook](../../.agents/skills/deploy/references/runbook.md)；下文保留准备时的事实和未完成状态，不作为现行命令或版本依据。真实回退尚未演练，自动 CD 未启用。

核实日期：2026-10-25（现场采集约 04:55–05:01 UTC）。范围：后端 `tshb-test`；只读核实、下载制品校验和本地文档。未部署、未执行迁移、未创建测试账号、未备份业务数据、未修改服务器配置、未启用自动 CD。

## 结论与当前阻断

**准备完成，奖励开关误阻断已在本地修复，但当前候选仍不能直接进入演练。** 准备阶段曾实际只读复现 `smoke_login_rewards_must_be_disabled`；原候选 `d15b637` 仍含该限制。随后按用户授权，仅修改本地控制器并完成隔离补验，尚未提交、推送或发布。

现场 `LOGIN_REWARD_ENABLED=true` 保持不变。登录、refresh、logout、草稿词表创建/修改均不调用独立的 `/api/v1/me/coins/login-reward` 领取接口，因此本地修复移除“必须关闭奖励”的预检条件，保留配置身份/漂移和资金副作用守卫。奖励开启、策略已生效、worker 正常运行的隔离环境中，对照账号实际领取 1 枚币；HTTPS 鉴权和业务烟测未改变资金记录，有资金副作用时清理守卫拒绝删除。详见[修复验收证据](evidence/2026-10-25-stage-d-reward-gate-fix.json)。修复合入后必须重新固定候选 SHA、main CI、artifact ID 和回执，本文的 d15b637 仅保留为已核实候选记录。

第二项前置工作是写前恢复点。当前已验证的最近完整 dump 是 2026-09-08，过旧，且本轮仅验证 checksum，不能认定为当前数据库恢复点或 PITR 能力。测试机磁盘与内存不适合直接照抄 1 GiB tmpfs 的恢复示例。新恢复点的生成、传输、隔离恢复都属于后续另行授权的工作。

## 精确候选与已部署基线

| 项目 | 精确值 / 实测结果 |
| --- | --- |
| 候选 main SHA | `d15b637ef8b9eed78316bb1032a492619ae0a491` |
| 候选 Git tree | `f9fc6061ff7bf5f922c968ff268252a118db2fac` |
| 候选 CI | [40161887176](https://github.com/LonelyFellas/tsz-rust/actions/runs/40161887176)，main push，attempt 1，13 个 job 全部成功，包括构建和制品 smoke |
| Release artifact ID | `12586389862`，未过期，保留至 `2026-11-24T04:31:59Z` |
| Release 二进制 SHA256 | `63ddfd9fd26d6da07948c5d03282aae28edc6fbe0071a0c6b34a7499358d9b73` |
| Release 二进制大小 | 60,000,488 bytes；GitHub 压缩包元数据大小 25,988,593 bytes |
| 制品 smoke 回执 | artifact `12586658094`；生产 attempt 1，验证 attempt 1；8 项检查通过、cleanup complete |
| Smoke 回执 SHA256 | `9431680d5bffe029f4d6f61898e3516326e42a6f835f140fd1774cdb9b091e8f` |
| 当前部署 SHA | `f4fab9ef579be2381265ed886bddc94aebaf1270` |
| 当前部署 Git tree | `50bbb43e7f0af726bfc6b036487108864652487e` |
| 当前部署 accepted 时间 | `2026-10-24T10:54:00.725328Z`，manifest v1 |
| 当前部署二进制 SHA256 | `30751290881d5b0f25a3f431e18885e862d5429cca54febb01987b533d737c6b`；磁盘、运行进程、下载的旧 CI 制品三者一致 |
| 当前部署旧制品 | CI [40086311339](https://github.com/LonelyFellas/tsz-rust/actions/runs/40086311339)，artifact `12557785282`，attempt 1，未过期，已下载并核验 |
| 迁移区间 | 133 条成功记录逐条版本/checksum 对齐；基线和候选最高版本均 `20261024090000`；新增区间为空 |

候选源码 tree 与阶段 C 最终审查提交 `d2566d911dab17b55a66e7efa05f22c182cf2b30` 完全一致。首尾只读复核确认 GitHub main、现网 manifest/二进制身份和迁移账本未变化。下载的两个 Linux 二进制均只做本地字节校验，未执行。

相对现网的应用差异：词表审核批准及私有响应封装为等价重构；增加只读 `deploy-info` 和部署工具。`migrations/`、应用配置、依赖、DTO/路由/错误契约没有变化，未发现需要前端配套发布或引入新持久化格式的证据。本次只安排后端演练。

## 现场只读核实

| 范围 | 结果 |
| --- | --- |
| SSH / systemd | `tsz-rust` active/running；MainPID `1125651` 在采集期间稳定 |
| 正式配置来源 | `EnvironmentFiles=/opt/tsz-rust/.env (ignore_errors=no)`，WorkingDirectory `/opt/tsz-rust`；相关进程/文件配置身份一致，无 drop-in |
| 业务开关 | `COOKIE_SECURE=true`，`DEPLOYMENT_SMOKE_ONLY=false`，`LOGIN_REWARD_ENABLED=true` |
| 监听 | 正式服务唯一监听 `127.0.0.1:8383`；未发现 18383 候选监听 |
| 健康 | `/healthz` 200 `{"status":"ok"}`；`/readyz` 200 `{"status":"ready"}` |
| 正式 HTTPS | 域名与系统 CA 校验通过，TLSv1.3；证书至 2026-12-12；证书与私钥匹配，只在内存核对 |
| nginx | `nginx -T` 成功；`test.tianshengzhi.com` 的 HTTPS `/api/v1/` 指向 `http://127.0.0.1:8383` |
| Python | 3.10.12，bcrypt 能力存在；未创建凭据或登录 |
| PostgreSQL | 服务端 18.3；只读事务核查，133 条成功迁移 |
| Redis | PING 通过，版本 6.0.16；未写入/清空键 |
| schema | 基线鉴权与完整业务清理所需表字段检查通过 |
| 锁 / 事务 | 无远端 deploy.lock、无新控制器事务目录；本地旧锁和 controller active 均不存在 |

现网 PG 18.3 / Redis 6.0.16 与 CI 制品 smoke 的 PG 18.6 / Redis 6.0.20 同 major/系列、补丁版本不同。CI 不替代现场验收。

## 备份、容量与恢复方案

### 已有证据

- `/opt/tsz-rust/deploy-backups` 有 144 组历史目录，二进制合计约 5.66 GiB。未删除或修改任何备份。
- 最近备份 `/opt/tsz-rust/deploy-backups/20261024T105156Z.Bc8KFM` 的二进制和 manifest 校验通过，但来源为 `14c7505`，**不是当前 f4fab9e**。遍历 manifest 未发现当前部署 SHA 的成组备份。
- 当前 f4fab9e 的原始 CI artifact 可取得且摘要与正在运行的制品一致，但它不能代替演练事务中对当前 binary+manifest 的成组备份。
- `/opt/tsz-rust/backups` 最近可见完整 dump 为 `backend-predeploy-20260908T020626Z-43d5a2ec-fa93-4d9a-975c-ef9fe1d3f7d2.dump`，412,915,825 bytes，SHA256 `211f15015ab5843413d25e81e4d82e07f5b57b14afacad346f02d0cdee8c33e7`，本轮实算与旁置 checksum 一致；未恢复验证。
- 宿主 `pg_dump/pg_restore` 为 14.24，不能用于生成 PG18 的新备份。已缓存 runbook 记录的 amd64 PG18 镜像 digest `sha256:d3e1620b530c944afa6e887d22eb899824da68e19c52024bf98f5220c88a65b2`；本轮仅 inspect，未启动容器，也未重新执行镜像内版本命令。
- 数据库约 2.40 GiB（2,572,072,639 bytes），测试机磁盘剩余约 1.27 GiB，MemAvailable 约 1.74 GiB。文档示例 1 GiB tmpfs 无法据此保证可恢复。当前本机磁盘剩余约 1.50 TiB，Docker 内存约 7.75 GiB，可作为独立恢复验证目标，但涉及真实数据传输，必须明确授权和清理范围。

### 推荐执行方案（本轮未执行）

1. 奖励开关误阻断已本地修复并通过隔离验证；后续另行授权交付后完成独立审查及 CI，再重新固定 main 候选和迁移区间。若引入新迁移，不沿用本文“无 schema 变更”的回退结论。
2. 在获得写前恢复点授权后，用已审核 PG18 工具生成当前数据库恢复点；推荐经受控 SSH 流传到本机任务专用 0700 目录，dump 0600，避免在测试机额外落大文件。连接材料只从现网进程读取并经内存/受限通道传递，不进入 argv、日志或文档。需落实 PostgreSQL 备份 runbook 的精确版本和一致性验证要求。
3. 在本机明确命名、无端口、`network none` 的独立恢复环境验证；按实际库容量预留空间，不采用 1 GiB 限额。比较 schema、账本、关键表计数与确定性摘要；对仍有业务写入的源库，必须使用一致快照或经批准的写入窗口，不能把两个时刻的计数直接作为一致性证明。
4. 临时恢复库/容器/数据副本验证后按精确 ID 清理；批准保留的恢复点作为本次运维备份保留，不归入临时数据误删。无论成功失败均保留脱敏回执。RDS 自动备份/PITR 本轮未查询，不能把它当成既有兜底。
5. 真正发布时，控制器在远端事务目录成组备份当前二进制和 manifest，验证摘要并 fsync 后才停服。保留这份精确备份、状态和归属回执供恢复。

单纯二进制切换约需同时容纳旧备份、新 incoming、安装临时副本，按当前尺寸约 180 MB，加日志和余量；当前磁盘足够这一局部动作，不代表足够完整数据库备份/恢复。不要为本次准备删除历史备份来腾空间。

### 本候选的恢复边界

| 现场 | 处理 |
| --- | --- |
| 取得锁前或停服前发现 CI/main/基线变化 | 不变更正式服务；终止当前计划并重新核实 |
| 停服后、尚未 accepted，账本仍精确等于基线 | 停止本次候选/正式进程，清理本次 fixture，恢复事务内 f4fab9e binary+v1 manifest，重新检查正式健康和 HTTPS 鉴权 |
| 账本出现任何新版本/checksum 漂移 | 本计划不授权 down；进入 manual_required，保留锁和现场 |
| 归属、配置、资产引用或清理无法证明安全 | 保留锁和精确定位信息；不抢锁、不删正常数据、不盲目重跑 |
| 已 accepted 后才要求退回 | 当前 `resume` 不负责撤销 accepted 发布；须新的回退计划与授权，不伪造旧状态触发恢复 |

新 accepted manifest 为 v2，旧版本部署工具只认识 v1。应使用本次受信任新工具处理事务、恢复原 v1，不能拿旧工具读取 v2；应用进程本身不依赖 manifest。当前控制器的 `backup` 仅备份 binary/manifest，不会自动创建数据库备份。

## 后续真实手动演练步骤与验收

先完成上面的阻断修复和恢复点，再取得一次明确的测试服发布授权及维护窗口。建议第一轮只验证正常发布，并对实际出现的失败按控制器恢复；**不主动制造真实测试服停机或数据故障**。专门的中断/失败注入应先安排在独立克隆服务，若要在正式测试入口做，须另列故障点、影响时长和回退条件后授权。

| 阶段 | 动作（获得执行授权后） | 放行证据 |
| --- | --- | --- |
| 固定输入 | 独立干净 main checkout；精确 CI artifact/manifest/元数据/smoke 回执；从同一制品取得 `deploy-info` | SHA/tree/run/生产与验证 attempt/artifact ID/digest 全一致，main 未前进，迁移区间重新核对 |
| 只读计划 | `plan` 检查实际基线、配置、schema、TLS、恢复策略；不取得远端锁 | 计划 JSON 完整，所有 preflight 通过，无既有事务冲突 |
| 事务建立 | `apply` 取得本地/远端 owner 锁，备份、上传并核摘要 | state、owner、backup、incoming 身份一致，备份落盘，停服前最后门禁通过 |
| 候选验收 | 停正式服务，原子安装；候选 18383、smoke-only、固定配置；健康/账本/HTTPS 鉴权 | 只监听 loopback，worker 禁用，精确账本不变，Cookie/refresh/logout 全部通过，合成账号精确清理 |
| 正式入口 | 停候选、启动 systemd；核 PID/exe/磁盘摘要、8383 和 HTTPS；草稿创建→新请求读取→修改→新请求读取 | 配置与进程身份正确；正常 Cookie；内容/revision/备注持久化；未调用奖励或音频付费入口 |
| 清理与接受 | 只删除本次 UUID 归属数据及专属依赖，独立复查 DB/Redis；最后写 v2 accepted manifest | 无 fixture 残留/外部引用误删；资金/上传资产保护通过；manifest 精确匹配；最后释放同 owner 锁 |
| 交付 | 报告实际部署版本、CI、制品、各验收与清理结果 | 任一步未知都不报告成功；不合并其他工作、不部署前端、不启用 CD |

`status` 只读本地事务；`resume` 会停止进程、清理数据及恢复，属于写操作，仅在已有该事务恢复授权下执行。新旧版本之间不存在迁移并不免除业务烟测、备份或配置门禁。

## 本轮证据与明确未验证项

- [脱敏只读核实记录](evidence/2026-10-25-stage-d-readonly.json)
- [阶段 C 控制器接口和状态](deployment-controller.md)
- [PostgreSQL 备份恢复规范](../postgresql-backup-restore.md)
- [现行部署 runbook](../../.agents/skills/deploy/references/runbook.md)

已完成：新旧精确制品下载和 SHA256 校验、候选 main CI/回执、完整迁移账本比较、现网健康/配置/TLS/路由、工具能力/容量/已有备份只读检查、首尾身份不变核验。

补充完成：奖励开关门禁的本地最小修复、开启/关闭及双向漂移回归、189 项工具测试、Rust fmt/Clippy；真实隔离制品在奖励生效条件下通过鉴权、业务持久化、资金无副作用与精确清理验证。隔离策略沿用现有 `tests/coins_login_reward.rs` 的 fixture 方法，仅在独立数据库内模拟提前生效，并在任何 HTTP 请求前恢复策略排期触发器；未修改运行环境的通用保留策略或业务配置。

未完成：本地修复提交/推送/独立审查/CI、新鲜数据恢复点及隔离恢复、正式控制器 plan/apply/resume 全链路、真实发布/故障演练、PITR 验证、统一替换旧发布入口、自动 CD。隔离验收使用真实后端/DB/Redis及本地 TLS 转发，systemd/进程身份使用替身，不等于测试服验收。`release_eligible=false`，本方案不授予任何服务器写入。

本轮下载的二进制与临时探针在完成核验后清理，只保留脱敏事实与方案。未提交、未推送、未创建新 PR。
