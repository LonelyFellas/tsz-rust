# 后端测试服自动部署

## 目标与授权

用户已批准实现、交付并启用 `tshb-test` 后端自动部署：main 精确 CI 成功后发布对应 Linux 制品，正常发布不逐次人工确认。基线为 `2f836b616016665f57bd523830672c4863cd1578`，任务工作区 `codex/backend-auto-deploy`。本文件维护唯一实施记录。

不增加音频生成、试听或逐音色验收，不在服务器构建，不重复运行整套 CI，不改变前端发布、业务配置或正常业务数据。

## 实现

- 新增 CI 完成触发的部署 workflow 和 `ops/deploy_ci.py` 接线，复用 `deploy_test.py` 的 plan/apply、远端锁、来源校验、验活、事务恢复及精确 fixture 清理。
- 候选绑定 main SHA、CI run/attempt、原始制品和成功验证回执；旧候选提前退出。发布串行执行，不取消已开始的事务。
- 从当前已验收版本到候选比较输入，明确的文档、规则及测试变更只核实在线状态并跳过发布；源码、迁移、构建/发布工具及未知路径保守发布。不把跳过的候选记录成已上线。
- 新增固定 `forward_only` 测试服策略，自动执行针对实际已部署版本的隔离升级与持久化回读验证，不要求逐版本恢复审批文件。该策略不授予数据库 down；账本未变时仍可恢复旧二进制，已迁移而发布失败时保留失败事务与证据。
- Actions 保存计划、bundle、控制器状态与结果；远端事务同时保留原始计划和已有动作记录。恢复针对原事务，不重新 apply 或抢占未知 owner。
- 一次性配置后端仓库 SSH Secrets 与专用发布 key。日常运行只消费现有 CI 产物，部署结果显示实际版本、状态和耗时。

## 验收

关键验证覆盖触发来源、过时候选、重复/纯测试发布、制品及回执身份、迁移升级与失败不执行 down、事务中断/恢复和状态保留；复用已有控制器边界回归。正常 hooks、精确提交独立审查、PR/main CI 均执行。

真实验收使用一次自然触发的自动发布，核对正式进程/磁盘/manifest 摘要、HTTPS 鉴权、草稿持久化和本批 fixture 清理。迁移失败与异常恢复在隔离环境验证，不在测试服注入数据库故障。

## 状态

自动触发、迁移策略及事务保存已实现。269 项部署工具回归通过；后续归档完整性与候选摘要增量的 30 项检查、镜像准备增量的 12 项检查通过。已下载真实 CI 验证回执，核实压缩包摘要、结构与清理结果。原生 pre-commit Clippy 已通过。

真实 Linux CI 制品 `16478dd6` → `2f836b61` 的隔离升级通过，覆盖迁移 `20261115120000` 与已有草稿持久化回读；环境为 PostgreSQL 18.6 / Redis 6.0.20。未执行 down 或调用语音供应商，测试服写入为零。临时容器、网络、下载制品及原始材料已清理，独立复查资源 owner 无残留。该证据只覆盖合成 fixture 的升级，不代表真实数据库恢复或最终发布版本已验收。

四项仓库 SSH Secrets 已配置，专用发布 key 登录验证通过，临时私钥与 known-hosts 材料已清理。首次发布前服务器基线为 `55579dc77112176c4b3c74081c1801f58ff2a5d7`、PostgreSQL 18.6、175 条迁移。

实现由 [PR #425](https://github.com/LonelyFellas/tsz-rust/pull/425) 合并，精确审查提交为 `0006a6f47cffadc9315b4f60f2faebb1bcba5ec4`。审查：2 轮；确认缺陷 1（失败归档遮蔽重试成功归档，已修复）；证据排除误报 0；未解决 P0–P2 为 0；关联回归/回退：未知。修复后的 31 项定向检查通过。

## tshb-test 内网部署接入

公开仓库 `LonelyFellas/tsz-rust` 的 main CI 继续生成并验证 Linux 制品。配置 `TSHB_TEST_VPC_DEPLOY_REPO` 后，该 CI 成功时通过 `workflow_dispatch` 提交 SHA、run ID 和 attempt。私有部署仓库的任务负责下载并校验原始制品，复用现有控制器把制品经内网 SSH 推送到 `tshb-test`。公开 workflow 的日志记录私有 run ID，发布结果看私有 run 的控制器回执。

### 接入与切换

1. 将 [私有部署工作流模板](deploy-tshb-test-vpc.yml) 放到专用私有仓库 `main` 的 `.github/workflows/deploy-tshb-test-vpc.yml`。保持 `main` 为默认分支，只给部署维护者仓库写权限，关闭私有 fork。Runner 以仓库级注册到这个私有仓库，标签为 `tshb-test-vpc`。
2. 在阿里云河源为 Runner 准备与 `tshb-test` 同 VPC 的独立 Linux x64 主机。使用能直接执行现有 Linux 制品的系统（例如 Ubuntu 24.04），安装 Git、Bash、Python 3、`gh` 和 OpenSSH。用专用账号运行 Runner，验证 Actions、GitHub 制品存储和 Docker 镜像仓库的 HTTPS 出站。迁移演练使用 Runner 自己的 Docker，并验证服务账号可运行容器以及四个演练镜像可用。
3. 以 Runner 账号配置 `tshb-test` SSH 别名，`HostName` 填测试服私网 IPv4，采用专用发布 key、`IdentitiesOnly yes` 和独立核验的已知主机密钥。禁用 `ProxyJump`、`ProxyCommand` 和 `ControlPath`。测试服安全组按 Runner 私网来源开放 SSH，保留已有部署账号和远端控制器权限边界。以同一账号运行 `bash ops/check_vpc_deploy_ssh.sh` 检查直连。
4. 私有仓库配置 `TSHB_TEST_SOURCE_READ_TOKEN`，只用 `LonelyFellas/tsz-rust` 的 `Actions: read` 与 `Contents: read`。公开仓库配置 `TSHB_TEST_VPC_DISPATCH_TOKEN`，只用专用私有仓库的 `Actions: write`。两份凭据使用独立的细粒度授权。先用 Runner 的只读凭据查询指定 CI attempt、下载验证回执和二进制 ZIP，核对 GitHub 提供的摘要，确认实际下载端点可用。
5. 验证私有 workflow 可调度到目标 Runner 后，在已授权的发布窗口使用当前 main 成功 CI 的精确候选做首次部署。确认控制器状态、远端事务和实际版本吻合。再设置公开仓库变量 `TSHB_TEST_VPC_DEPLOY_REPO=owner/repo`，以自然触发的 main CI 验收切换。切换前确认公网部署任务和远端事务空闲。

私有仓库的 Actions 证据保留 30 天，包含候选、计划、bundle、控制器日志和状态。Runner 使用 `RUNNER_TEMP` 下的独立目录，服务账号需拥有该目录及文件的删除权限，由 Runner 在 job 边界清理。Runner 中断时按远端事务 ID 与原计划核对现场，再走既有恢复流程。

### 验收和回退

- 只读验收记录精确 SHA、CI run/attempt、私有 run ID、Runner 标签、私网 SSH 直连和制品摘要。过时候选应返回 `superseded`，失败候选应在 SSH 之前停止。
- 真实发布核对 `accepted`、正式进程与 manifest 摘要、HTTPS 鉴权、草稿保存与重新读取、fixture 清理以及 `server_build=0`。
- 从 `github_artifact_download` 日志取各次 GitHub ZIP 下载时间，从事务 `timings` 取 `upload` 阶段耗时，从 `result.json` 取控制器耗时。另记私有 workflow 的排队、仓库检出和证据归档用时。首版在部署任务内下载制品，使用实测记录评估整体收益。
- 跨仓调度报错或回执丢失时，先查私有 run 和远端事务。恢复公网发布时，等私有队列和运行中任务处理完、远端 owner 释放，再清空 `TSHB_TEST_VPC_DEPLOY_REPO`，核对最新 main CI 后重跑公开仓库部署工作流。进入写入阶段的故障按原事务恢复。

## 首次自动发布验收（2026-11-15）

自动 CD 已启用并完成一次自然触发的真实发布；正常版本不逐次人工确认。

| 证据 | 实际结果 |
| --- | --- |
| PR CI | [43713335083/1](https://github.com/LonelyFellas/tsz-rust/actions/runs/43713335083) 成功；PR 的制品构建/验证按设计跳过 |
| main CI | [43714265026/1](https://github.com/LonelyFellas/tsz-rust/actions/runs/43714265026) 的 18 个 job 全部成功 |
| 自动部署 | [43715087536/1](https://github.com/LonelyFellas/tsz-rust/actions/runs/43715087536) 由 `workflow_run` 触发并成功；job 95 秒，driver 80.807 秒，服务器构建 0 |
| 在线 SHA / tree | `de8e84eb29c4d852e161e7c310b3536a37f3c199` / `40fea8d662b9a76292ae928f311026583ffa7bda` |
| 原始制品 | artifact `14160944899`，生产/验证 attempt 均为 1；二进制 70,117,480 字节 |
| 二进制 SHA256 | `95bf110125478f82136ad51cf1d923a0b93dbb134ed5a79e139b7dc398542df9`；正式进程、磁盘与 accepted manifest 一致 |
| 发布事务 | `d00f4bfe-a550-45c0-992c-697cd5b186f7`；原始计划和 manifest 已远端归档，最终 `accepted` |
| 迁移 | 自动采用 `forward_only`，升级 `20261115140000`，账本从 175 条到 176 条；未执行 down |
| 备份 | 已有独立定时备份状态 `passed`，未现场重新导出整库 |
| 正式验收 | 候选/正式 HTTPS 鉴权、草稿创建→新请求读取→修改→新请求读取均通过 |
| 独立清理复查 | 3 份 fixture 的账号、会话、角色、草稿及相关外键/幂等/词项引用和 Redis 账号键无残留；候选进程退出，owner 锁释放 |
| 正常数据与配置 | 发布前保存的 3 份正常词表摘要、`.env`/nginx/Web/Admin 的 8 项文件指纹均未变 |
| Actions 证据 | 19 个文件，保留 bundle、计划、状态和结果；归档 58,003,709 字节，SHA256 `e4cd338c1d307199fa3d02ed4aa54563d254c1a5495495c99b80c718e86bfa5b` |

本次真实迁移升级成功不代表真实数据库回退或 PITR 已演练。没有语音生成、试听或供应商验收。纯文档收尾仅回填本记录，后续 main CI 成功后应只读核验并以 `verified_inputs_unchanged` 跳过发布；实际运行回执以关联 PR 的验收记录为准。
