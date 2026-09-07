---
name: deploy
description: 将 tsz-rust 当前 GitHub main 的成功 CI 制品部署到 tshb-test，执行精确来源校验、部署锁、备份、原子替换、完整冒烟与回退。仅用于明确的后端部署请求；讨论或准备部署不触发服务器写入。
---

# 后端部署到 tshb-test

## 执行入口

用户明确要求后端部署时，按项目 AGENTS.md 委派 `backend_deploy_runner`；该 runner 不再次委派。
在 Codex 使用项目定义的 agent；其他工具也须遵守该入口，不能用普通子任务冒充指定 runner。
本技能及其引用的操作手册是唯一部署流程；前端部署、提交或合并授权不能代替后端部署授权。
本会话已明确授权的同一后端部署可以继续，不因跨步骤或等待重新请求相同许可。

实际执行前必须完整读取 [操作手册](references/runbook.md)，包括失败后的回退和锁清理。
仅做讨论/评估时按需读取，不执行手册中的变更命令。

本次需要前端配套发布时，读取本仓 [配套发布清单](../contract-sync/references/paired-release.md)，核实实际版本组合与顺序；清单不能替代本技能的 runner、CI、锁或制品门禁。

## 核心门禁

- 干净本地 main 的 HEAD 必须等于 GitHub 当前 origin/main；锁内记录 SHA、tree 与 session。
- 精确 SHA 最新 CI 必须成功，skipped/neutral、失败、无记录、异常或未知结果均不放行。
- 使用目标提交携带的 CI 门禁工具；等待时保留句柄和原始退出码，最长等待按工具设置，持续提供必要进展。main 前进须释放尚未进入服务器阶段的本 session 锁，再从新 SHA 重走。
- 任意服务器写入前验证 CI run/attempt、制品唯一性/有效期/哈希/manifest、SSH、只读数据库预检、当前部署来源，以及完整 smoke 所需凭据可用且不输出。
- 本地和远端锁 owner 必须属于同一 session；跨调用状态存入锁内 state，不能假设 shell 变量仍在。
- 服务器无 Git checkout，不同步源码、不现场编译；只安装精确 CI 二进制，不改 .env、数据库或前端。
- 成组备份二进制与 manifest，再原子替换；撤下正式 manifest 后的失败按手册回退。涉及迁移时先核对旧二进制兼容新 schema；不能凭空承诺数据库自动回退。
- health、ready、auth 全链路和制品 verify 全部通过才算成功；禁止只凭服务 active、退出码 0 或 JSON 中的 SHA 判定。
- 不抢占/删除其他 session 的锁、不手工补造 manifest，不在失败后盲目重跑整次发布。

## 执行顺序与终点

按手册依次完成「精确源码与本地锁 → CI → 产物与只读预检 → 远端锁与备份 → 原子替换 → smoke → manifest → 同 owner 解锁」。
任一步阻断时按已到达阶段处理；恢复任务先读 state/owner/backup/manifest，不从头猜测或清现场。

报告部署 SHA、CI run/attempt、制品校验、下载/同步耗时、server build=0、smoke、manifest 与锁状态。
失败时报告是否已写远端、是否回退、回退验证与剩余阻塞；未完成完整验收不能报部署成功。
