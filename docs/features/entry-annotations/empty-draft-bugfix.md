# 空草稿检测与继续编辑修复

## 前置与失败证据

2026-09-06，后端worktree `tsz-rust-dev-worktree`，HEAD `4a9a3ecf`，ship暂停。现场8583原PID15866；用户现有center空草稿 `01a076b8-5735-72a2-8abe-034220257948` 由主任务保留，本修复任务未读写其业务数据。
独立临时PostgreSQL16容器 `tsz-empty-draft-bugfix-20260906`，127.0.0.1:55433，master `empty_draft_bugfix`；Redis DB2。所有Cargo命令显式DATABASE_URL隔离，禁止.env回落。

自动回归 `empty_draft_bugfix_http_detect_and_create_explain_existing_draft` 启动随机TCP端口真实Axum服务：检测未收录测试词→创建无surface草稿→再检测→同名创建。
预期检测提供可续编目标，409冲突提供同一目标；实际修复前detect仅空matches，创建409 duplicate_word无meta.word_id，断言失败。
原始证据 `/tmp/empty-draft-bugfix-before.log`。此证据证明现场泛化红错的同一后端因果链，不声称捕获了用户浏览器原始响应。

## 最小协议与实现

- V3 detect新增可选非null UUID `existing_draft_id`，无目标省略。仅返回当前actor创建、同kind、初始确认主词重叠、未归档且没有有效surface source的V3草稿。
- 不把空草稿放入matches，不更改surface snapshot、确认token、POS建议或发布语义。
- 创建保留409 `duplicate_word`；可续编的自己的V3空草稿复用已有 `meta.word_id`，前端去 `/words/{id}/v3/wizard/forms`。其他管理员或非创建路径不增加目标信息。
- 防重仍重查当前最终主词与状态；不相信缓存existing_draft_id来决定放行/拒绝。归档、历史双NULL初始主词回退、有效surface排除规则保留。
- 前端只显示自己可见目标，普通duplicate_word无目标时给准确冲突说明。数字标注0–9最多20位规则由前端独立writer保留；本次未改标注校验或扩大生命周期规则。

## 验证与台账

两个新增回归已通过。新增风险回归覆盖检测后竞态、跨actor无ID、kind隔离、继续原id保存base后再次创建走annotation_conflict、归档排除、缓存提示过时、最终初始主词与原检测词面不同。
独立只读reviewer对本次SQL、可见性、创建重检和token边界未发现P0–P2阻断。
未stage/commit/push/PR/rebase/整合dev；前端由其独立任务修改；8583外的服务不动。

权威OpenAPI SHA256：`852d0db8e521dae56576a6d1bfc54c683c94f646385478c8f7952d0d3bb55a33`。前端任务已显式同步该源并报告定向测试/typecheck/lint通过；最终UI验收由主任务负责。

## 日志完整性

- `/tmp/empty-draft-bugfix-before.log` SHA256 `f3921576b43ba2cc0d0c61da54f2eaa58d8930a644285deb4562e67d83b21742`
- `/tmp/empty-draft-bugfix-after.log` SHA256 `d589d394d72b6e329307cc1dd3fc274c8f871c816975b4fc0b6d5ddfdf137536`
- `/tmp/empty-draft-bugfix-openapi.log` SHA256 `07629ca3e348c0fe586e89784cc302e7e0df402fd322d1bdca43e96373836501`
- `/tmp/empty-draft-bugfix-fmt.log` SHA256 `bd8ca23a0b258d3e4455c23063b8762c7206a3e615f2d2a9507db84a686e966e`
- `/tmp/empty-draft-bugfix-clippy.log` SHA256 `b89fbcaf395646129220172f9eaf093068c141a11f9ffe3616c11ed517f658f1`

## 后端最终验证与交接

- fmt、全目标全features Clippy -D warnings通过；lib 301通过/1已有忽略，bins 7通过，doc通过（0用例）。
- 受影响集成共10项通过：empty筛选5项（含本次2项）、surface_confirm_bugfix 3项、actor/command/revision/digest/policy令牌1项、清空forms防重复initial headword 1项。未重复无关全仓测试。
- 独立只读初审与最终增量复核均无P0–P2阻断；不扩大标注跨生命周期约束。
- 只在核实PID15866的cwd与8583监听后停止原进程；新服务PID37278，readyz HTTP200 {"status":"ready"}。显式localhost5433/tsz_dev_worktree_20260906、Redis DB2，其他服务不动。
- 已通知主任务对原页面保留输入重新检测并继续已有草稿；本记录尚不代表主任务最终UI验收。
- 临时PG活跃测试连接0后docker stop，--rm清理本次子库/数据。Redis未广泛清理，测试快照按TTL过期。用户现场草稿不动。
- ship保持暂停；未stage/commit/push/PR/rebase/整合dev。HEAD仍4a9a3ecf。
- `/tmp/empty-draft-bugfix-unit.log` SHA256 `d8b3f45101b23960e8af16daf304da7e6ba4fc41bae0af8dce29ae7a82c8ee22`
- `/tmp/empty-draft-bugfix-doc.log` SHA256 `b2e980eacd4a44afa75efa94b46bc90f84602ead05f92e1cef81f46b1c6f5096`
- `/tmp/empty-draft-bugfix-regression-empty.log` SHA256 `371727bf64870ed5c2835284ed15efa8dd5893d38f023399ebd46d1cfb08cd84`
- `/tmp/empty-draft-bugfix-regression-surface_confirm_bugfix.log` SHA256 `9d2a6620dcf103f116bfbf942fe00dee2d440a5690fb9cc2b8df39b17cc59045`
- `/tmp/empty-draft-bugfix-regression-v3_surface_warning_tokens_bind_actor_command_revision_digest_and_policy.log` SHA256 `66b50aa3cd003efab85dd71c9480d3189a7f49f7bf7cd67d8911b335a6e844e2`
- `/tmp/empty-draft-bugfix-regression-clearing_v3_forms_cannot_create_duplicate_active_hidden_initial_headwords.log` SHA256 `d81a6b195003e15de1a826afc56f3d5057c816288eb0f6ae7f5458f80f32929c`
- `/tmp/empty-draft-bugfix-build.log` SHA256 `0d719eec32e3e7d480db20cb8abbc6605a7aae6811d81ed121c54bdba829b711`

代码/测试/迁移工作树聚合（path+NUL+内容+NUL）SHA256：`765feb1c9dfc1627168034db22ccb995459a7760f422257825313dffd6308f0b`。

## 主任务最终真实UI验收

证据来源：主协调任务 `01a0763f-ffe9-74a0-aba7-e6069491d36e` 的右侧3201真实UI操作回报；本后端任务未重复操作。

- 在 `/words/new` 重新输入 `center` 检测，显示独立“已有未完成草稿”和“继续创建”链接，不出现新建提交按钮。
- 原形检测仍显示未发现，符合现场草稿确无surface的事实；空草稿没有伪装成真实匹配。
- 点击“继续创建”进入 `/words/01a076b8-5735-72a2-8abe-034220257948/v3/wizard/forms`；AX明确显示center、基本词性0、词形0，与现场原草稿ID一致。
- 没有泛化红错。主任务未重试create，未新增、删除或修改任何词条，未保存forms；现场原草稿完整保留。

本次bug后端与主任务真实UI验收均通过。临时测试实例已经清理；ship仍暂停。此次最终追加仅写文档，未改代码、重跑测试或操作数据。
