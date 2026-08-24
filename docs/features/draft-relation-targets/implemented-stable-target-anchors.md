# 草稿 relation 目标的稳定锚点语义

> 实现基线：`main` @ `f962451` 之后的 bugfix。
> 本文取代同目录旧评估中“目标必须先发布、来源才能发布”的方案。

## 发布引用

`relation` 的正式引用始终锚定稳定身份 `(target_entry_id, target_sense_id)`。来源发布时：

- 目标 sense 已在目标当前 publication 中：记录 `target_content_scope = publication`、目标
  publication ID 与该 publication 的 `source_revision`；
- 目标 sense 只存在于当前草稿：记录 `target_content_scope = draft`、空
  `target_publication_id` 与目标当时的 entry revision；
- 来源 publication 的不可变 JSON snapshot 继续保存由服务端规范化的
  `target_headword` / `target_gloss`，因此历史内容与结构化锚点可以一起审计；
- 数据库用稳定 target-node 外键保护两种范围，并对 `publication` 范围额外用
  `(target_publication_id, target_entry_id, target_revision)` 外键校验 revision，不能靠可空字段绕过
  目标完整性；
- `sentence_context` 不在本次放宽范围内，数据库约束与 service 均继续要求目标已发布。

目标后续发布不会回写历史来源 publication，也不会把历史 `draft` 锚点改成
`publication`。来源下一次正常发布时，若目标当前 publication 已包含该 sense，新引用自然使用
`publication` 范围。

## 生命周期与删除

- 只存在草稿入链时，目标允许归档；来源草稿仍可保存，但发布会因目标归档被拒。
- 存在其他词条的 active current publication 入链时，目标归档被拒；来源与目标可按既有批量
  生命周期规则一起归档。
- 来源已归档后目标可以归档；目标不可用时，来源恢复被
  `entry_has_unavailable_publication_refs` 拒绝。
- 目标 sense 后续从草稿移除时，若它仍在目标当前 publication 中，正式 relation 仍有效；若它
  只存在于草稿，来源发布/恢复会被拒。
- 从未发布的目标只要进入过任一来源 publication 的历史引用，就不再允许硬删除。历史引用通过
  稳定 node 外键保留；管理员应归档目标，而不是破坏发布审计。仅有草稿入链时，移除这些草稿
  relation 后仍可删除目标。

## 待建关联词遇到同名归档词条

关联词搜索只回已发布且未归档的词条，所以同名词条一旦归档，管理员在下拉里看不见它，
只会把这个词当库外新词写成待建关联词（`pending_target_headword`）。

**绑定阶段一律拒绝绑到归档词条**——草稿保存的绑定与发布时的物化都是，报
`relation_target_archived`，issue 锚在那条 relation 节点上、字段是
`pending_target_headword`，待建词面原样留在草稿里。

这与上面「目标事后被归档，来源草稿仍可保存」不矛盾，两者是不同时刻：

- **已绑定**的关联词，目标事后才被归档 → 不追罚来源，保存照旧成功，发布门拦；
- **尚未绑定**的待建关联词，绑定当刻目标就已归档 → 此刻词面还在管理员手上，是唯一
  能提示他的时机。绑上去 `pending_target_headword` 会被清空，而发布必被
  `relation_target_unavailable` 拒，管理员重填同一个词面还会再绑上来——「改不动也发不了」。

出路是恢复那条词条（恢复后原样重存即可绑上）或改指向别的词。**不能绕过它另建同名新条**：
归档只写 `entries.archived_at`，`entry_headword_keys` 那行还在，
`lexicon_entry_headword_keys_unique_idx` 也建在 keys 表上，归档词条依然占着词面。

## 检测上下文与确认

重复/surface 检测的 `inbound_relations` 合并两层事实：当前草稿 `lexicon.relations` 与来源词条
当前 publication 的 `entry_publication_sense_refs`。同一个 relation 节点、目标 entry 与目标 sense
只计一次，草稿层优先展示当前编辑值。

每条 preview 必须返回 `source_status`：

1. 来源 `archived_at` 非空 → `archived`；
2. 否则有 `current_publication_id` → `published`；
3. 否则 → `draft`。

`matched_entry_contexts` 有独立 digest，并与 surface membership digest 一起在确认消费时重检。
关联增删、来源词头或生命周期变化后，旧 token 返回 `surface_matches_changed`；检测过期、策略
epoch、原子消费、幂等键和锁内 surface 重检仍沿用原门禁。所有会改变检测上下文的词条写入与
token 消费事务还共同持有按目标 entry ID 分片的事务级 advisory lock，确保上下文不能在 digest
重检后、命令提交前并发变化。锁冲突快速返回 `reference_conflict`，互不相关的词条不会排队占用
数据库连接。
