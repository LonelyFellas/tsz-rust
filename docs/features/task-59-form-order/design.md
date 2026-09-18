# TASK#59：关联候选词形顺序

任务：http://47.121.142.19/zentao/index.php?m=task&f=view&taskID=59

## 规则与修复

- 关联下拉的词形顺序以目标词条 `form_groups[].members[]` 为准，不是全局词形类型 sort_order，也不是 `pos.forms[]` 创建顺序。
- `PublishedAssociationTarget::from_v3_parts` 按组顺序、成员顺序生成候选；共享 form_id 首次出现优先，未入组历史词形按原数组顺序追加，保持候选集合。
- 草稿和发布快照共用转换层；不混用未发布内容，不改持久化数据或候选 ID、变体、可关联词义。
- 无 DTO、OpenAPI、SQL、迁移变更；旧前端直接消费有序 forms。前端共用级联选择器已保序，补组件回归；独立快捷入口 `savedSenseTargets()` 同步采用组/成员排序并补先红后绿测试。
- 多维释义、多维例句、共享例句和短语成分使用此候选链；关联词独立入口按词条/词义选择，不新增词形层。

## 验证

- 修复前 `SQLX_OFFLINE=true cargo test --lib v3_snapshot_derives_form_group_bases_for_candidate_inventory` 失败：创建顺序与组成员顺序不一致。
- 修复后 `SQLX_OFFLINE=true cargo test --lib sentence_association`：29 项通过，覆盖组内调序、多组共享、历史未入组原形、草稿与发布转换及绑定身份不变。
- `cargo fmt --check` 与 `SQLX_OFFLINE=true cargo clippy --all-targets -- -D warnings`：通过。
- 初次14项数据库测试因缺少连接未完成；补建 TASK59 专用 PostgreSQL16/Redis7 后 `cargo test --lib`：294 项全部通过；`shared_sentences`：24 项通过；`lexicon_handler` 定向候选测试：10 项通过，日志 `/tmp/task59-db-tests.log`。
- 前端最终补测后8个相关测试文件202项通过，admin typecheck、变更代码 ESLint 和格式检查通过。
- 最终独立代码审核未发现本次新增阻断缺陷；既存快捷入口专用组词义范围差异已在前端设计文档单独记录。
- 最终真实验收已通过全部列明的适用入口（释义、例句、共享例句、短语成分、关联词、快捷关联），发布A→草稿调序B隔离→重发布B、英美筛选、同拼写身份、保存刷新也通过。详情见前端同目录 `full-acceptance.md`。跨组共享 form_id 写入被拒绝列不适用；历史读取兼容为单测证据。当前证据支持提交，不代表生产部署验收。
- 专用容器、进程及会话凭据已清理；未提交、推送或部署。

完整跨仓需求及前端验证命令位于前端仓库 `docs/features/task-59-form-order/requirements.md` 和 `docs/features/task-59-form-order/design.md`。
