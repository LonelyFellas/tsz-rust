# Agent 独立备份

目标：每天备份 `tshb-test` 的 `tsz_agent` 数据库到河源私有 OSS 并复制到深圳；每周从深圳副本在无网络的临时 PostgreSQL 容器中恢复和验证。该任务复用现有数据库备份的私有存储、加密、复制、权限和 14/30 天云端保留策略；Agent 与 Rust 的本机状态、归档目录和监控指标分开。

Agent 备份读取正在运行的 `tsz-agent-api-1` 数据库连接，只接受 `127.0.0.1:5433/tsz_agent`。上传和回读校验成功后才记录成功；深圳副本校验失败、备份失败和恢复失败均应报警。恢复验证检查 Agent 核心表和非空 Alembic 版本，并清理临时容器、卷及下载文件。本地归档保留 14 天，只有逐份核对深圳副本 SHA-256 后才清理，始终保留最新归档。

灾难恢复：深圳桶的 `database-backups/scheduled-agent-<时间>-<随机值>/` 包含加密的 `backup.dump` 和 `manifest.json`，后者独立记录数据库身份、PostgreSQL 主版本、字节数和 SHA-256。源机和本地状态丢失时，用有权限的运维账号在深圳桶列出该前缀，选择一组同时包含归档和清单的对象；读取清单后下载同组归档，核对大小和 SHA-256，再在隔离 PostgreSQL 18 中恢复。常规定时恢复也先从深圳读取并校验该清单。未复制完整的对象组不能用于恢复。

安装依赖：合入并审查过的脚本及 systemd 单元、既有 OSS/告警凭据、PostgreSQL 18 客户端和固定镜像；根目录 `ops/agent-backup.example.json` 应以 root 所有且 `0600` 安装为 `/etc/tsz-rust/agent-backup.json`。不可与 Rust 备份共享本地目录。配置 Agent 独立的 `tsz_agent_backup_health` 低值和无数据报警，使用现有失败事件通知联系组。首次启用前执行一次备份，等待深圳复制并验证 SHA-256，然后从深圳副本执行隔离恢复；确认告警、清理和计时器后启用每日、每周及心跳任务。

验收：`python3 -m unittest discover -s ops -p 'test_postgresql_backup_job.py'` 和 `python3 -m unittest discover -s ops -p 'test_postgresql_backup_prune.py'` 应通过。线上确认备份状态和恢复回执均为 `passed`、恢复 `cleanup_complete` 为真，四个 `tsz-agent-*` 计时器已启用，独立健康指标及失联告警正常。部署和真实恢复验收在目标环境执行；仅通过单元测试不代表已启用。
