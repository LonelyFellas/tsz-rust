# 词条标注：后端设计与共享契约

状态：产品与实施已获批准；契约 v1。基线 `4a9a3ecf2a9eec6bc2ad2cb4b3985f8c817d4028`，唯一写入工作树 `tsz-rust-dev-worktree`。

## 依赖与范围

关键路径：已有 create_v3 单事务、幂等哈希、surface 锁与快照 → 新增独立标注存储及重复组计算 → 创建原子补写/编辑 → DTO/OpenAPI → 风险验证与只读独立审查。
可直接复用：最终 headwords 规范化、V2/V3 surface_sources、草稿可见性、空骨架 DuplicateWord、审计、管理员认证。小改动：匹配 context、列表及详情携带标注。新增：annotation/annotation_revision、组约束、标注编辑命令。前端独立实施；学习端、内容发布语义、提交/推送/部署均不在范围内。
预计剩余关键路径：设计定位 20–30 分钟，实现及验证 2–3 小时；超过估计 50% 时更新原因和最小下一步。

## 规则

annotation 独立于 presentation.label。trim 后保存；空串保存为 null（无重复组可空），上限 20 个 Unicode scalar value；重复判断 trim 后 Unicode lowercase，相同组非空且不同。拒绝控制字符。不做 NFKC/全角折叠。前端用 Array.from(value.trim()).length 与 toLowerCase() 对齐。
原型组按最终 headwords 所生成的 dialect_scope/normalized_surface 查询，V3 仅 base，V2 仅 headword；common 展开 uk/us。不根据普通词形 warning 强制标注。保留现有 draft 仅创建者可见、current_publication 仅当前版本规则。无 surface source 的活动 V3 空骨架仍由 DuplicateWord 拦截，不提供标注绕过。
同一 entry 在 entries 只出现一次；groups 保留真实共享原型。A 仅含 x、B 仅含 y、新条含 x/y 时，A/B 可以同标注，新条必须分别不同。编辑检查该词条 draft 与当前 publication 全部有效 base 的直接邻居，不能只检查当前列表搜索词。

## 创建请求与响应

`POST /api/v1/admin/lexicon/entries`，保留现有认证、`Idempotency-Key: UUID` 和 schema_version=3 请求。新增可选字段：

```json
{
  "annotation": "新词条标注",
  "annotation_updates": [
    {"entry_id": "uuid", "annotation": "已有词条标注", "base_annotation_revision": 1}
  ]
}
```

原有字段 `schema_version`, `detection_id`, `kind`, `headwords`, `confirmed_surface_match_token` 不变。annotation 缺省/null 表示未标注；annotation_updates 缺省为空。出现组时必须提交全部相关已有 entries（包括未改的值），禁止额外 entry_id 和重复 entry_id。旧标注即使有效也须携带对应修订，保证完整可编辑且防止覆盖并发修改。

首次请求可以不携带标注；先处理既有 surface confirmation 流程，最终头词验证后服务端返回 HTTP 409 `code: "annotation_conflict"`，`meta.annotation_conflict` 如下：

```json
{
  "reason": "required",
  "entries": [
    {"entry_id":"uuid", "annotation":null, "annotation_revision":1,
     "presentation":{"label":"center","matched_surfaces":["center"],"strategy_version":"..."},
     "pos_labels":[], "gloss_previews":[], "updated_at":"ISO8601", "inbound_relations":{"total":0,"by_type":{"synonym":0,"antonym":0,"derivative":0},"previews":[],"truncated":false}}
  ],
  "groups": [{"dialect_scope":"uk", "normalized_surface":"center", "entry_ids":["uuid"]}]
}
```

entries 使用现有 `MatchedEntryContextV3`（上例 inbound_relations 结构以 OpenAPI 原类型为准），增加 annotation/null、annotation_revision。groups 的 entry_ids 是已有词条；创建中的新词条隐式属于每一组，不伪造 UUID。reason 枚举：`required`（缺标注或缺完整已有条目）、`duplicate`（直接同组重复，或修改旧条时与其其他有效原型的直接邻居重复）、`revision_conflict`（旧标注修订变化）、`group_changed`（提交额外/过时条目）。创建冲突始终返回新条直接相关的完整 entries/groups；旧条其他原型的邻居不额外扩展弹窗。duplicate 因此不一定能在当前表格定位，前端应提示标注与同原型词条重复、已有词条可能关联其他原型，保留输入供更换标注。无部分写入。

前端从该 409 原生弹窗呈现所有已有条目并加入新条目；确认后附加字段重试创建。失败未记录成功幂等结果，可复用该 key；成功后相同规范化 body/key 返回原响应，变更 body/key 仍遵守既有 idempotency_conflict。

成功维持 HTTP 201 `{ "word": ... }`，新词条直接进入 forms。V2/V3 管理列表和详情新增 `annotation: string|null`, `annotation_revision: integer>=1`；内容 revision、投影 revision、has_unpublished_changes 不因标注变化。

## 后续编辑

`PATCH /api/v1/admin/lexicon/entries/{entry_id}/annotation`，现有 active admin 认证。请求：

```json
{"annotation":"标注", "base_annotation_revision":1}
```

成功 HTTP 200：`{"entry_id":"uuid","annotation":"标注","annotation_revision":2}`。实际值不变时不增长修订，仍校验 base_annotation_revision。null 仅在无直接重复邻居时可用。修订不符 HTTP 409 annotation_conflict/revision_conflict；冲突 entries 包含目标和直接邻居，groups 包含目标。不存在 404 word_not_found；归档 409 entry_archived；格式非法、超过20字符、修订小于1或重复提交entry_id：HTTP 400 invalid_request_body，field 指向 annotation、annotation_updates 或 base_annotation_revision。单独编辑不新增幂等协议，以修订防止重复覆盖。

## 数据与事务

entries 增加可空 text annotation、非空 bigint annotation_revision 默认1（历史数据 null/1）。创建规范化在请求哈希前进行，现有 token 校验成功后、INSERT 前校验全部组并更新旧值；所有更新、新条、审计、幂等结果同事务。编辑复用 surface policy/key/context locks 顺序并重新检查当前组。匹配上下文加入标注及修订，owner_bundle digest 自动绑定；变更后旧 token 走既有 surface_matches_changed，不绕过快照。
新增响应字段使用 serde 默认值读取历史缓存/发布快照。旧程序 deny_unknown_fields 不能读取新对象，回滚程序前须清理相关短期检测/匹配缓存；V3 发布快照（JSONB）同样持久化了 annotation/annotation_revision，down 迁移清不掉，旧程序读取快照会 500，回滚须按 V3 原生发布的「恢复发布前数据库备份」规则执行（或先剥离快照里的这两个键）；数据库 down 会删除标注，回退迁移前必须备份。

## 验证

重点覆盖：无重复可创建；普通变体不强制；第二/第三条和历史 null；多原型去重、非直接邻居可同值；trim/大小写/Unicode长度；旧修订与快照漂移；事务失败无部分修改；并发创建/编辑；成功重试与不同body幂等冲突；V2读取；空骨架/草稿可见性；标注不改变内容修订与发布状态。
仅使用显式 DATABASE_URL 指向 tsz_dev_worktree_20260906 或本功能独立测试库。服务仅允许8583，启动前检查进程来源，记录测试数据及清理。定向测试后执行仓库适度质量门，更新 docs/openapi.json，并用只读 agent 独立审查。
