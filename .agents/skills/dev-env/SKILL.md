---
name: dev-env
description: 诊断或按授权准备 tsz-rust 本地环境，核实 API 进程、可执行版本、Postgres/Redis 隔离及启动迁移；不用于服务器部署。
---

# Rust 服务环境

遵循 [AGENTS.md](../../../AGENTS.md)，诊断默认只读。先核实实际运行版本与数据去向，再处理缺少的依赖；已授权本地启动/修复不逐步重复确认。

1. 核实任务 checkout、SHA/脏差异、实际监听 URL/PID、cwd/可执行文件、启动时间和构建来源。默认 8383 仅为排查起点；HEAD、端口、mtime、OpenAPI 或 health 不能单独证明 binary 版本。
2. 按 [环境核对](references/environment-checks.md) 核实配置、Postgres host/port/数据库与 Redis host/port/DB/键隔离；只报告脱敏标识，不输出密码、token、整份 env 或进程参数。
3. 主服务启动会执行 migration。确认数据库归属、schema 与迁移影响后才能启动，不用裸 `cargo run` 探测未知库。测试核实其实际读取的连接变量和 fallback；显式传隔离目标，不能只改一个未被读取的变量。
4. `SQLX_OFFLINE=true` 只代表编译用 `.sqlx`，不代表运行免数据库；`cargo sqlx prepare` 也不是只读探针。共享 Cargo target 不做无范围 clean，不删除服务使用中的二进制。

已授权准备环境时优先复用身份与隔离明确的服务，不足时使用本任务独立资源和空闲端口。只停止/重启本任务拥有且身份明确的进程，接管其他任务服务需对应授权；不能按端口批量 kill、清共享库/缓存或 `docker compose down -v`。生产 COOKIE_SECURE 不得设为 false。

涉及 UI/Agent 时定位各自任务 checkout，按需读取其 dev-env；前端代理/mocks、Agent 显式迁移与 worker 的规则不在这里重复维护。需要真实页面验收读取选定 tsz 的 test/references/real-backend-acceptance.md。
记录新增服务句柄、日志和临时资源清理方式；结束按项目规则定向清理，保留共享服务。报告 URL/进程、构建来源、API/数据目标和未验证项；单个 health 不替代 ready、鉴权或目标业务验收。
