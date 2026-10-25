---
name: deploy
description: 经明确授权，使用 ops/deploy_test.py 将当前 GitHub main 的成功 CI 制品发布到 tshb-test，执行来源门禁、持久化事务、备份、完整验收、恢复与精确清理。准备工作不授权服务器写入。
---

# 后端发布到 tshb-test

通用授权与任务工作区遵循 [AGENTS.md](../../../AGENTS.md)。完整操作只维护在 [runbook](references/runbook.md)，接口、状态与验收边界见 [控制器说明](../../../docs/ci/deployment-controller.md)。执行前完整读取三者；兼容入口和部署 agent 不维护第二套步骤。

## 唯一执行入口

使用精确候选提交携带的 `ops/deploy_test.py`：`plan` 生成只读计划，`apply` 执行发布，`status` 查看本地状态，`resume` 观察并恢复原事务。不拼接另一套 SSH/rsync/systemctl/manifest 命令；不绕过控制器的步骤顺序或门禁。

- Codex 宿主使用已加载的项目 `backend_deploy_runner`；它直接执行，不再递归委派。工具缺失时准确报告限制，不让普通子任务冒充。
- Claude 会话按用户既有约定直接执行相同控制器流程，不以不存在的 runner 阻断；两种宿主遵守同一权限、来源和验收要求。
- 只有明确的后端发布授权才运行 `apply`；`resume` 也可能停服、清理数据、执行有界 down 及重启，须在本事务恢复授权内使用。提交、合并、前端部署和阅读文档均不代替后端授权。
- 已授权的同一事务连续推进；中断后读取原 owner、plan、state 与现场，不重新 `apply`，不因换步骤或等待重复申请已有授权。

## 执行门禁

1. 准备独立干净的 `main` checkout，HEAD/tree 等于当前 GitHub main；原开发工作区不切换、不清理。控制器及直接依赖摘要固定到计划，下载目录与受信工具目录分离。
2. 最新精确 CI run/attempt 必须 completed/success、来自同仓 main 的 push/workflow_dispatch。生产 attempt 和验证 attempt 分开，绑定精确 artifact ID、有效期、摘要与最新通过且清理完成的 smoke 回执；缺失、失败、未知或来源漂移一律阻断。
3. 在服务器写入前核实部署基线、完整迁移账本、配置身份、TLS、资源容量，以及可用的写前恢复点。控制器不自动创建数据库备份，也不判断恢复点是否适用于当前数据与候选；按 [备份恢复规范](../../../docs/postgresql-backup-restore.md) 留下验收和适用性证据。
4. 无新增迁移时，只有账本仍等于基线且旧版兼容当前数据才允许二进制/manifest 恢复。有新增迁移时，先取得实际基线的隔离升级/恢复证据与独立批准的策略；合成演练不授权真实数据 down/PITR。未知现场进入 `manual_required`，保留锁和状态。
5. `apply` 管理本地/远端 owner 锁、动作前 intent、结果落盘、成组备份和原子替换。正式服务停止前再次核对 main/CI/基线，变更开始后固定候选；不抢锁、不手工补写 manifest、不盲目重发。
6. 保持 `.env`、nginx、前端和业务开关不变。`COOKIE_SECURE=true`；`LOGIN_REWARD_ENABLED` 可开可关，但仍参与配置身份和漂移检查。服务器只运行精确 CI 二进制，不同步源码或现场编译。
7. 候选和正式 HTTPS 鉴权、正式进程/摘要/监听、草稿持久化、精确 fixture 清理全部通过才创建 v2 accepted manifest。独立复查正常数据保护、数据库/Redis 无本次残留、配置和前端指纹、候选退出及同 owner 解锁；未验证或失败不能报告成功。

## 验收与汇报

2026-10-25 已完成控制器的真实正常发布演练，见 [阶段 D 证据](../../../docs/ci/evidence/2026-10-25-stage-d-manual-deployment.json)。**真实回退尚未演练**；不把隔离故障测试或正常发布成功写成现网恢复通过，不自动注入故障或启用 CD。

交付实际版本、CI run/生产与验证 attempt、artifact ID/digest、owner、备份/恢复点、各验收及清理结果，记录下载、上传、总耗时与 server build=0，区分压缩包与二进制大小。失败时同时报告 mutation 边界、实际版本、恢复/锁状态和残留；未完成项保留为未知或阻断。
