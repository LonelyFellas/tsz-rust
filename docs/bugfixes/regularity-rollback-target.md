# 拼写规则标记部署回退入口

2026-09-18 只读预检发现：现网 schema 为 20260917120000，计划发布的 20260917180000 仅增加拼写规则标记。旧数据中有 25 条草稿/快照匹配历史 payload guard；该 guard 无视目标 schema，导致实际 `deploy-undo-migrations` 在运行本次 down 前即失败。直接测试 down SQL 不能证明完整回退入口可用。

修复：目标 schema >= 20260917120000 已支持历史 guard 检查的全部字段，由各后续迁移的专用 down guard 校验新数据；更旧目标继续保留原有保守拦截。未修改 migration SQL、数据或接口。

新增隔离数据库回归用例先在旧实现复现失败，再验证：
- 变体 is_regular 已写入时仍由本次 migration guard 拒绝回退，账本不变。
- 未写入该字段时允许 180000 → 120000，旧正文/多组绑定 JSON 完整保留。
- 继续回退到不支持旧 payload 的更早版本仍被拦截。

命令：显式指定本任务隔离 DATABASE_URL / TEST_REDIS_URL，执行 `SQLX_OFFLINE=true cargo test --locked --all-features --lib deployment_migrations`。原生提交/推送 hooks 执行 SQLx prepare、Clippy 和 lib/bins。

现网只执行了只读预检，未调用 undo 或更改数据库。发布使用修复合 main 后精确 CI 制品，先完成兼容 admin 与旧页面处理，再按专用 backend_deploy_runner 的完整门禁执行。API 候选冒烟期间不开放写入；出现新格式业务数据后仍不得回退旧二进制。
