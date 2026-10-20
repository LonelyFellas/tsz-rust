# 禅道 132：词库与语音权限整理

任务：[132](http://47.121.142.19/zentao/index.php?m=task&f=view&taskID=132)。本记录是两仓共同的实现与验收入口；已授权提交、推送并创建前后端 PR；不合并、不部署，不操作线上数据。

## 范围与行为

- 权限名称、说明按任务更新；`words.detect`、`words.validate` 合入 `words.create`。现有创建权限保留，仅有旧识别/校验权限的账号不自动获得创建权限。编辑、发布所需校验仍由相应操作权限执行。
- `speech.upload_audio` 仍是唯一 key，在“词库管理”和“语音合成管理”均展示，勾选同步、计数和提交去重。
- `speech.global_dubbing` 仅为“全局配音”的预留定义，说明明确功能尚未接入；没有配音中心或全局生效逻辑，也不隐含其他语音/词条权限。
- 从垃圾桶恢复为草稿，保留当前草稿正文与发布历史，清除当前公开指针；内容 revision 不变，lifecycle revision +1。退役公开 surface，重建草稿 surface。活跃的 Published 不能借 restore 隐式撤回。
- 从已有发布历史恢复草稿，覆盖已保存的词形与释义；仅允许非归档 Draft，不自动撤回或发布，不新增 publication，不保存每次编辑的历史。保留创建者、创建时间及当前标注。
- 历史恢复要求 `words.rollback`；恢复他人草稿另需 `words.edit_others`。范围权限不能代替操作权限，也不放宽他人发布、删除或垃圾桶恢复。事务中重查权限、归属与版本，幂等重放同样复核授权。
- 恢复词形、释义、节点身份、关联、音频引用、译文、步骤完成状态、投影、审计和幂等结果在同一事务完成；历史内容按当前草稿安全规则校验，不绕过正常发布校验。

## 接口及配套版本

新增 `POST /api/v1/admin/lexicon/entries/{id}/publications/{publication_id}/restore-draft`：

- 必填 `Idempotency-Key`；请求包含 `schema_version: 3`、`base_revision`、`base_lifecycle_revision`，必要时附 `confirmed_surface_match_token`。
- 成功为 200 `AdminWordV3Envelope`；仅内容 revision +1。确认 token 独立绑定操作、词条、来源历史、两个版本和候选内容，不复用 SaveForms 确认。
- 历史列表和详情在 `current_publication_id IS NULL` 时正常返回，所有历史项 `is_current: false`。

单条和批量垃圾桶 restore 使用独立 DTO，均必填 `restore_as: "draft"`；archive 请求不变。新幂等 scope 为 `lexicon.entry.restore_draft` / `lexicon.entry.restore_draft_batch`，不会重放旧的自动公开结果。

旧 `/publications/{publication_id}/rollback` 保留 201 和线上重新发布效果，但路由、事务服务均要求 `words.publish` + `words.rollback`，仍限制本人/超管。新前端只调用 `/restore-draft`，遇到 404/405 不降级为旧命令。

| 前端 / 后端 | 恢复行为                                                                            |
| ----------- | ----------------------------------------------------------------------------------- |
| 旧 / 新     | 垃圾桶恢复缺 `restore_as`，422，零写入；需更新前端后使用                            |
| 新 / 旧     | 垃圾桶恢复携带旧 DTO 不接受的字段，拒绝、零写入；新历史端点不存在，明确失败且不降级 |
| 新 / 新     | 按草稿恢复；旧线上 rollback 不被新 UI 使用                                          |

候选顺序为后端先、前端随后；过渡窗口允许恢复功能暂时安全失败，不允许误公开。没有执行部署。失败发布回退前须停止业务写入并核对迁移版本，不把“同批发布”当作兼容性保证。

## 授权迁移与只读影响查询

迁移：`20261019100000_merge_word_creation_permissions.up/down.sql`。

up 仅移除退休 key，保存原授权者、授权时间、标签条目和迁移后期望版本；仅实际变化的管理员/标签递增版本。不补 `words.create`、不修改历史审计。备份表仅用于受控回退，不是新的授权来源。

down 仅恢复这次退休的原始记录，继续递增版本。资源版本有后续变化、退休 key 已重新授权、原授权者不存在，或已授予 `speech.global_dubbing` 时拒绝自动回退，避免覆盖后来授权。先审查实际差异并明确重新授权/撤销预留权限，再按精确迁移版本处理；不能盲目删除备份或修改 expected_version 强行回退。

以下 SQL 在迁移前只读运行，用于列出待审查资源。**本轮未查询真实环境，因此没有现网受影响账号名单。**

```sql
SELECT a.id, a.display_name, a.permission_version,
       array_agg(g.permission_key ORDER BY g.permission_key)
         FILTER (WHERE g.permission_key IN ('words.detect', 'words.validate')) AS retired_keys,
       bool_or(g.permission_key = 'words.create') AS retains_create
FROM admins a
JOIN admin_permission_grants g ON g.admin_id = a.id
GROUP BY a.id, a.display_name, a.permission_version
HAVING bool_or(g.permission_key IN ('words.detect', 'words.validate'))
ORDER BY a.id;

SELECT t.id, t.name, t.version,
       array_agg(i.permission_key ORDER BY i.permission_key)
         FILTER (WHERE i.permission_key IN ('words.detect', 'words.validate')) AS retired_keys,
       bool_or(i.permission_key = 'words.create') AS retains_create
FROM permission_tags t
JOIN permission_tag_items i ON i.tag_id = t.id
GROUP BY t.id, t.name, t.version
HAVING bool_or(i.permission_key IN ('words.detect', 'words.validate'))
ORDER BY t.id;
```

## 版本与验证

- 前端：`codex/zentao-132-permissions`，基线 `88e7db5b` + 本任务工作区差异。
- 后端：同名分支，基线 `096f665` + 本任务工作区差异。
- 工作区分别位于 `.worktrees/frontend/zentao-132-permissions`、`.worktrees/backend/zentao-132-permissions`。未修改主 checkout。
- 契约从本任务后端用 `SQLX_OFFLINE=true cargo +1.98.0 run --locked --all-features --bin export_openapi` 导出。前端显式设置本任务 `OPENAPI_SOURCE` 并清除 `SYNC_OPENAPI_RUNTIME_ONLY` 后原生同步；新增一个路径、四个请求相关 schema，响应结构未扩展。
- PostgreSQL 为任务独立 cluster，端口 55432；Redis 为任务独立实例，端口 56379。集成测试数据库与浏览器验收数据库分离，没有使用共享/远程业务库。
- 浏览器验收使用实际构建并复制到任务 runtime 的后端（127.0.0.1:18383）及任务 admin Vite（127.0.0.1:13001）；代理到该后端 `/api/v1`。目标词库、词性、TTS 和本地语音 mock 均关闭，Playwright 未拦截 API。短信服务沿用后端当前内置测试 sender，不代表真实短信验收。

### 已完成检查

后端命令均显式使用 Rust 1.98.0、`--locked --all-features`，数据库和 Redis 指向上述隔离实例：

- `cargo +1.98.0 fmt --check`、`cargo +1.98.0 clippy --locked --all-features --all-targets -- -D warnings` 通过。
- `cargo +1.98.0 test --locked --all-features --lib`：374 项通过。
- `admin_permissions_handler`：17 项通过，包含退休授权不提权、重复 up、受控 down 和后续授权冲突保护；`admin_profile_handler`：12 项通过。
- `lexicon_v3_lifecycle`：12 项通过，包含草稿/线上状态限制、归属范围与撤权重放、晚期投影失败全事务回滚、确认来源绑定、同 base revision 并发仅成功一次、历史读取及单条/批量垃圾桶恢复。
- `lexicon_handler` 原路径 148 项通过；6 个受本次 restore 协议/语义影响的场景更新请求并定向复验通过，仍检查失效草稿引用、拼写格式协商、短语、历史与公开投影。未重新执行耗时的无关全目标场景。
- `lexicon_v3_storage_schema`：11 项通过；`lexicon_v3_relation_consumers` 当前目标运行 0 项，不将其计作额外引用测试覆盖。

前端：

- 权限编辑器/列表、共享鉴权、词条写权限、向导、历史、预览和垃圾桶共 9 个文件：257 项通过，2 项既有跳过（旧左栏跳转行为已取消，非本次跳过）。使用 `--maxWorkers=2` 限制本地并发，不改变断言。
- `pnpm --filter @tsz/api-client test --maxWorkers=2`：27 个文件、576 项通过，包含端点、DTO roots、格式/幂等头、严格 runtime schema、请求 marker 及禁止降级。
- `@tsz/admin`、`@tsz/shared`、`@tsz/types`、`@tsz/api-client` 的 typecheck 与 lint 通过。
- 生成 spec 的 SHA-256：`2540306f1f5cf81992088e29a342bcca1bf307c4c4779b9260ace6d015943495`；runtime bundle 哈希与本任务来源一致。

### 真实验收

使用本任务临时账号、词条、发布历史及公开词表，不访问外部业务数据：

1. 浏览器真实登录，真实 API 创建词条、保存完整词形/释义并发布。
2. 页面归档、垃圾桶恢复后为 Draft；修改并保存释义，再从第 1 次发布恢复，刷新后重新读取历史释义，内容 revision 增加、lifecycle revision 不因历史恢复增加，历史没有被该操作扩增。
3. 显式重新发布后才回到 Published。
4. 页面在两个分类中勾选同一上传权限，预览并提交；关闭后重新打开两组均选中，数据库仅有一条 `speech.upload_audio` 授权。
5. 普通管理员真实登录，仅获得上传权限，直接恢复历史收到 403，没有因展示分组获得额外能力。
6. 管理端页面归档/恢复/发布配合真实学习端 `GET /api/v1/wordlists/{id}/items`：公开时有 entry；归档后无内容；恢复草稿仍无内容；显式重新发布后才返回内容。没有启动 Next Web 页面，不声称完成学习端界面交互验收。

浏览器曾发现空 current_publication_id 导致历史查询布尔字段解码 500，已在列表/详情两处改为 COALESCE 并补回归。取消自动公开后失去消费者的旧可见性激活模块和相关查询已删除；发布时原有公开校验仍保留。

### 审查修复（2026-10-19）

已修复审查的 8 项问题，范围限前端及原任务记录，没有改动后端接口、契约或迁移：

1. 所有历史入口统一使用未保存草稿保护。真实权限刷新会清理授权缓存并重定向到只读预览；页面用同词条占位数据维持草稿会话，重定向不再卸载向导。没有放宽服务端权限或跨词条沿用缓存。
2. 历史恢复成功后，通过同一请求实例重新 GET 完整 `AdminWordDraftV3Envelope`，连同 `retired_stable_nodes` 更新缓存后再重建会话；冲突刷新也保留完整结果。写入成功但读取失败时保留原幂等键重试。
3. 发布历史独立于预览步骤，在未完成草稿的编辑步骤也可查看、恢复，不放宽发布完整性条件。
4. 恢复 POST 及完整 GET 待决期间禁止关闭详情、切换历史或重复写入；关闭按钮、X、Esc、遮罩与同步锁保持一致。
5. 复用已有 `newWordNodeId`，缺少 `crypto.randomUUID` 时使用 `getRandomValues` 生成 UUID；生成键在取得操作锁之前完成。
6. 同名确认分页默认使用注入的 `requests.surfacePage`，不回落到其他后端或认证上下文。
7. 复用 V3 问题分类，展示校验原因、步骤/字段/节点位置，以及引用冲突的来源和打开入口，不再只留通用失败文案。
8. 移除无人调用的 `rollback` 同名确认 UI 分支，测试改为草稿恢复语义；底层旧线上 rollback API 仍保留原有兼容用途。

验证：先建立失败复现；退役账本还做了断言敏感性检查，保留旧账本会使 split 槽位 UUID 断言失败。最终定向命令覆盖 `V3PublicationHistory`、`WordWizardV3`、`LifecycleSurfaceConfirmation`、V3 请求包装、发布预览和授权缓存共 6 个文件，**169 项通过、2 项既有跳过**；`pnpm --filter @tsz/admin typecheck`、`pnpm --filter @tsz/admin lint`、6 个修改文件的 Prettier 检查和 `git diff --check` 通过。未改动的后端和 API-client 契约复用上文结果，没有重复跑全后端测试。

真实验收复用 SHA-256 匹配的同一后端构建，前端为本任务 Vite，目标 API 无 Playwright 拦截、无 service worker、目标 mock 均关闭。使用本轮独立数据库 `task132_review_acceptance` 和启动前为空的 Redis DB 12，实际验证：

- 未完成草稿从词形步骤打开并恢复发布历史。
- 在本任务词条上持有数据库行锁制造真实 POST 待决，确认关闭按钮、X、Esc、遮罩不能丢弃结果；释放锁后恢复成功并完成完整 GET。
- 页面加载后仅模拟缺少 `randomUUID`、保留真实 `getRandomValues`，恢复请求携带合法 UUID 并返回 200。
- split 草稿恢复 common 历史后再切 split，两侧重用原 UUID，真实保存 200，刷新及独立 GET 确认持久化。
- 在未保存释义期间真实撤销编辑权限、保留恢复权限，获取新 profile 后只读预览仍阻止历史恢复。这条路径补齐了单元测试先前漏掉的授权缓存清理与重定向条件。

最终浏览器记录 50 个真实 API 响应，唯一 401 为登录前无会话的自动 refresh；业务操作没有异常响应。分页、结构化错误和读取失败重试由定向测试覆盖，没有把它们写成真实故障注入验收。本轮应用已停止，独立数据库已删除并复查不存在，DB 12 的 2 个精确缓存键已删除并复查为 0；临时凭据、脚本、浏览器数据、复制二进制和原始日志已清理，没有创建云对象。首轮 PostgreSQL/Redis 停止权限阻塞仍保留，不绕过限制。

### 收尾与限制

最终同状态后端构建（SHA-256 `22745d882b2821f69486fab6d51f205a016fa4998e1ebe6173a8b21a918b731f`）再次完成管理端两种草稿恢复、刷新及显式重新发布；学习端词表接口五个状态检查均为 200，可见性依次为 true / false / false / false / true。

应用服务已停止，13001/18383 均不再监听。独立验收数据库 `task132_acceptance` 已删除并独立确认不存在，包含的临时账号、会话、词条、发布历史、词表、审计和 outbox 随之删除。浏览器凭据/会话、本地 secrets、脚本、截图、复制二进制和原始应用/测试日志已清理；没有上传音频或创建云对象。

**清理阻塞：**权限系统拒绝停止 PostgreSQL/Redis，因此没有绕过限制、没有删除运行中的 data directory。`/private/tmp/tsz-task132-runtime.M1EQn4/pg`（55432）及该任务 Redis（56379）仍运行，独立核查 PostgreSQL 已无任务数据库（只剩系统数据库），Redis DB 0 首轮收尾时有 329 个任务测试缓存 key，本轮复查剩 38 个；两个实例及 data directory 仍在，不能宣称资源清理完成。需获得停止这两个任务隔离实例的权限后收尾；不涉及共享数据库或共享 Cargo target。脱敏回执保存在 `/private/tmp/tsz-task132-evidence.itipq4wt/verification.json`。

已进入两仓 PR 交付；尚未合并或部署，没有现网影响查询、生产迁移或真实短信/云配音验收。全局配音仍只是预留权限。

### PR 交付前同步主线

前端同步至 `1ad751f8`，后端主线新增 `9f811be` 的退休词表权限迁移，与本任务原迁移版本号重复。因此将本任务迁移改为 `20261019100000_merge_word_creation_permissions.up/down.sql`，同步部署回退测试版本和迁移测试引用；保留上游迁移及其测试，不修改迁移业务内容。合入后的检查结果随 PR 正文记录，先前运行制品与验收哈希仍只代表上文所注明版本。
