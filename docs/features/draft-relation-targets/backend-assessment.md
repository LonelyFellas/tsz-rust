# 关联词可指向草稿：后端评估

> **来源**：2026-08-20《智能词库系统测试报告》「大功能待办 · 关联词不存在时创建草稿并关联」
> （标注为新增功能、待产品确认），以及前端在此基础上的接口侧评估。
>
> **核对基线**：`tsz-rust` `main` @ `9190d5f`（2026-08-21）。下文每条「现状」都读过源码，
> 行号即该 commit 的行号，不是推断。
>
> **复核**：全文 60 条代码事实断言经过一轮独立复核（多 agent 分组核对 + 对存疑项的反驳式
> 二次核对 + 遗漏批评），51 条完全正确、9 条行号或措辞不精确、**0 条判定为错**，不精确项已修正。
> 复核另外查出 6 条会改变结论的遗漏，已并入正文（§2.4 告警框、§2.5 第 7/8/9 条、§4 方案 D
> 告警框与发布节点集合、§6 包 1 告警框与包 2 改写）。
>
> **状态**：**仅评估，未动任何代码**。§4 的发布规则待产品拍板；§2 的保存放宽
> 在产品确认功能要做之后即可单独落地（它不依赖发布规则的结论）。
>
> **后续**：`target_state` 的最终判据与发布短路的修复形状见同目录
> [`target-state-criteria.md`](./target-state-criteria.md)（2026-08-22）。
> 它纠正了本文 §3.3 的一处口径、补齐了 §4 告警框的 DB 依据、并改写了 §7 的「发布短路」用例，
> **下文这三处已就地注明**。产品结论（显式 B）以前端 tsz 仓库 PR #158 为准。
>
> **一句话**：保存阶段放宽**可行且代价很小**，但比提问里描述的更关键——今天这道校验
> 不只卡词义步，**连词形步也一起卡死**；同时前端「不需要任何新接口」的结论**只对
> 「刚创建完立刻关联」这一条路径成立**，还有两个硬缺口（新草稿 `forms.pos` 为空时压根没有
> 词义可关联 / 事后再来关联时下拉搜不到草稿）需要后端补。
>
> **⚠️ 三条对排期影响最大的、容易被漏掉的约束**（都在下文有源码依据）：
> ① 放行档**必须显式写值**，不能沿用现在的 `continue`，否则客户端可以自填只读快照字段
> 一路落进发布快照（§2.4 告警框）；
> ② 「目标发布后回来重新发布」在当前代码里是**静默 no-op**，必须先重存词义步（§4 方案 D 告警框）；
> ③ **AI 补全会把关联赖以存在的 sense 节点整批换掉**，而「新建草稿 → 立刻关联 → 再补全」
> 恰恰是最自然的操作顺序（§2.5 第 7 条）。

---

## 0. 摘要：五个问题的答复

| # | 问题 | 结论 |
| --- | --- | --- |
| 1 | 保存阶段放宽到「存在且未归档」可行吗 | **可行**，DB 层早就支持。但建议**再放宽一档**到「目标 sense 节点存在」，理由见 §2.3——「未归档」这条线会制造新的「改不动」死结 |
| 2 | `target_headword` / `target_gloss` 指向草稿时填什么 | **填草稿当前值**（词头必有、释义常为空串），并**新增一个只读字段 `target_state`** 让前端能判定降级文案。只靠空串反推不可靠 |
| 3 | A / B / C 怎么选 | **推荐 D**（B 的诚实版：发布时剔除未解析关联 + 提供「待补发」查询让管理员显式回收），A 会死锁、C 会撞穿不可变发布快照模型 |
| 4 | `detect` 的重复检测覆盖草稿吗 | **是**，而且比你查的那处更稳——主路径（surface 投影）和 legacy 兜底**两条都覆盖**，且主路径直接回出 `draft/published/archived` 三态 |
| 5 | 会不会产生「存得下、发不出、也删不掉」的中间态 | **会，但可控**。三个坑列在 §5，其中两个**今天就已经存在**，另一个（目标草稿删不掉且不知道被谁引用）需要一处配套修改 |

---

## 1. 复核前端的两条结论

### 1.1 ✅ `detect` 的重复检测确实覆盖草稿（比你的依据更强）

你读的 `repository/dictionary.rs:139-174`（`legacy_exact_duplicates`）判断正确：它只 JOIN
`lexicon.entries`，不按发布状态过滤，把 `is_archived` / `is_published` 当标志位回出。

补充两点：

1. 那是**兜底路径**，不是主路径。`detect` 的主路径是 `service/entry.rs:425-427` 的
   `headword_surface_matches`，走 `repository/surfaces.rs:3-79` 的 `SURFACE_SOURCES_QUERY`。
   该查询的 WHERE 明确写着 `source.content_scope = 'draft' OR (content_scope =
   'current_publication' AND ...)`（`surfaces.rs:64-71`），且 SELECT 里直接算出
   `lifecycle_status` 三态（`surfaces.rs:20-24`）。**主路径同样覆盖草稿**。
2. `legacy_exact_duplicates` 只在 `has_unprojected_legacy_exact` 为真时才会被采信
   （`entry.rs:442-443`），即「B4 投影回填还没追平」的过渡期。所以长期看主路径才是事实源。

**结论：报告要求的「创建前检查已发布词条和既有草稿，避免重复入库」，`detect` 已内建，不需要新接口。**

### 1.2 ⚠️ 「创建草稿那半边不需要任何新接口」——只对一条路径成立

`detect` + `create` 两步确实能建出草稿，但**关联要落地必须拿到一对 `(target_word_id,
target_sense_id)`**，且 `target_sense_id` 受 DB 外键约束（见 §2.1），不能瞎编。于是分三种情形：

| 情形 | 能不能只靠 detect + create 完成关联 | 说明 |
| --- | --- | --- |
| **A. 词典命中**，且至少映射出一个 catalog 词性，建完立刻关联 | ✅ 可以 | `create` 会调 `build_initial_meanings`（`service/entry.rs:157-177`）按 forms 的每个词性各播一个空词义，`create` 的响应体 `word.meanings` 里就带着可用的 `sense.id`。前端直接从响应里取即可。**但见 §2.5 第 7 条：这个 `sense.id` 会被 AI 补全换掉** |
| **B. 新草稿 `forms.pos` 为空** | ❌ **不行** | `forms.pos` 为空 → `build_initial_meanings` 产出 `pos: []` → **新草稿一个词义都没有**，没有任何合法 `target_sense_id`。前端必须额外走一次 `PUT /steps/forms`（且要替这个还没人管的词条**选词性**）才能拿到词义。**两个成因**：① 词典未命中（`service/entry.rs:711-722` 的 `NotFound` 分支走 `DraftFormsStepContent::default()`）；② **词典命中但一个词性都映射不出来**——`map_dictionary_pos`（`service/helpers.rs:77-102`）只白名单 11 类，`_ => None` 静默丢弃，`mapped_codes` 空 → `catalog_parts` 空 → `build_suggested_forms`（`entry.rs:75-81`）产出空 `pos`。注意此路径**不会**被 `entry.rs:754` 的 `CatalogMismatch` 拦下（`pos_codes` 与 `part_map` 同为 0） |
| **C. 事后再来给这条草稿加关联** | ❌ **不行** | 关联词下拉走 `GET /entries/related-search`，其 SQL（`repository/query.rs:28-58`）硬 `JOIN lexicon.entry_publications ON publication.id = entry.current_publication_id`，**结构上只可能返回已发布词条**。草稿不会出现在下拉里，除非扩这个接口 |

**给前端**：B 和 C 都是要后端配合的。B 更像产品问题（「给一个连词性都没定的空壳建关联」是否可接受）；
C 是明确的后端工作量（`related_search` 增加一个「含草稿」的检索档位，或新开一个只给关联选择器用的检索接口）。
两者都不在本轮「保存放宽」的范围内，建议单列。

---

## 2. 请求 1：保存阶段放宽

### 2.1 好消息：DB 层从第一天起就支持草稿目标

`migrations/20260811120000_create_lexicon_meanings.up.sql:172-173`：

```sql
CONSTRAINT lexicon_relations_target_fkey
    FOREIGN KEY (target_sense_id, target_entry_id)
    REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT
```

外键指向的是 **`lexicon.nodes`（节点注册表，草稿节点也在里面）**，不是
`entry_publication_nodes`。`sentence_links` 同理（`:145-146`）。也就是说：

- **草稿关联在存储层完全合法**，今天存不下去纯粹是服务层 `resolve_current_published_senses`
  （`repository/publications.rs:423-469`）那道 JOIN 造成的。
- 更有力的旁证：`repository/entries.rs:657-678` 的 `delete_never_published_entry` **已经写了
  「有别的词条的草稿关联指向我就不许删」的护栏**，扫的正是 `lexicon.relations` /
  `lexicon.sentence_links`。这段代码在今天的规则下**永远走不到**（关联只能指向已发布词条，
  而已发布词条本来就不可删——`delete_draft` 还有 `published_revision.is_none()` 前置
  `service/lifecycle.rs:71-73`，DELETE 语句本身也带 `NOT EXISTS (publication)` 兜底）。
  *（这段护栏为什么会被写出来，我没有证据，不做意图推断。）*

**所以这不是「加能力」，是「把一道服务层的门打开」。**

### 2.2 拦路虎比提问描述的更宽：词形步也被卡

`ReferenceResolutionMode::Canonicalize` 有**两个**调用点，不是一个：

- `service/editing.rs:618` —— `save_meanings`（你已发现）
- `service/editing.rs:212` —— **`save_forms`**

`save_forms` 会把既有 meanings 顺带 `reconcile` 一遍并跑同一套引用解析，
`editing.rs:220-224` 同样是「有 issue 就整体 422」。

后果：**一旦某条关联的目标不可解析，这个词条的词形步和词义步会同时锁死**，管理员连改个
拼写都存不下去，唯一出路是先把关联删掉。这条今天就成立（比如目标词条被归档之后），
放宽保存正好把两处一起治好——**改一处 `Canonicalize` 分支，两个调用点同时受益**。

### 2.3 建议的判定分层：把线画在「节点存不存在」，而不是「归没归档」

你提议放宽到「存在且未归档」。我建议**再松一档**，理由是「未归档」这条线会制造新的死结：

> 源词条 S 的草稿关联指向草稿 T → T 被归档（归档只查发布层入链
> `active_inbound_sense_refs`，`service/lifecycle.rs:309-320`，**草稿关联挡不住归档**）
> → 从此 S 的词形步 + 词义步全部 422，管理员必须先删关联才能继续工作。

同样的问题也出现在「目标把那个词义从自己草稿里删掉了」（节点被打上
`removed_from_draft_at`，`repository/entries.rs:403-409`）。

**建议的三档判定**：

| 判定 | 条件 | 保存阶段行为 |
| --- | --- | --- |
| **硬拦（必须保留）** | `(target_entry_id, target_sense_id)` 不是 `lexicon.nodes` 里的 sense 节点（**服务层判定**，见 §2.4 SQL 里的 `node.node_type = 'sense'`） | 422 `relation_target_unavailable`。**这一档不能省，且两个子情形的理由不同**：① 节点**根本不存在** → 撞 `lexicon_relations_target_fkey` 违约，被 `map_entry_write_error`（`repository/entries.rs:6-11`，只识别 headword 唯一索引）原样吞成 `Database` → **500**；② 节点存在但**不是 sense**（比如把 pos 节点 ID 当 `target_sense_id` 传进来）→ **外键拦不住**（它只约束 `(id, entry_id)`，不含 `node_type`），插入会成功并写进脏数据，只能靠服务层这道校验 |
| **放行 + 标注** | 节点存在，但目标是草稿 / 已归档 / 该词义已被移出目标草稿 | 200 保存成功，`target_state` 如实标注成 `draft` / `archived` / `detached`（三者处置不同，见 §3.3），`target_headword` / `target_gloss` 尽力回填 |
| **发布门（不动）** | —— | `publishing.rs:142-153` 的 `validate_forms` + `validate_meanings` 原样不动；`Verify` 模式仍然要求「目标当前发布版本中的有效词义」 |

这样「存得下之后又存不下了」这个坑基本被堵死：**保存不会再因为目标的归档、改稿、移除词义而失败**，
所有状态判断都推到发布门和 UI 展示上。

**但不是「永远不会失败」**——目标草稿被硬删仍是例外（§2.5 第 1 条的竞态），
以及删除之后再保存会落到「硬拦」档。这一条别写太满。

### 2.4 落地形状（若要动工）

新增一个 repository 方法（**不改** `resolve_current_published_senses`，`Verify` 继续用它）：

```sql
WITH requested AS (SELECT DISTINCT target_entry_id, target_sense_id FROM unnest($1,$2) ...)
SELECT requested.target_entry_id,
       requested.target_sense_id,
       entry.archived_at IS NOT NULL          AS target_archived,
       node.removed_from_draft_at IS NOT NULL AS target_removed,
       entry.headword_mode, entry.source_dialect,
       -- 词头三侧，取法与 repository/entries.rs:554-559 的 entry_by_id 完全一致
       ...,
       projection.meanings                    AS draft_meanings,
       publication.snapshot                   AS published_snapshot   -- 仅当该 sense 在当前发布里
FROM requested
JOIN lexicon.nodes node                       -- ← 这个 JOIN 就是「硬拦」那一档
  ON node.id = requested.target_sense_id
 AND node.entry_id = requested.target_entry_id
 AND node.node_type = 'sense'
JOIN lexicon.entries entry ON entry.id = requested.target_entry_id
LEFT JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
LEFT JOIN lexicon.entry_publications publication
       ON publication.id = entry.current_publication_id
      AND publication.entry_id = entry.id
      AND EXISTS (SELECT 1 FROM lexicon.entry_publication_nodes pn
                  WHERE pn.publication_id = publication.id AND pn.entry_id = entry.id
                    AND pn.node_id = requested.target_sense_id AND pn.node_type = 'sense')
```

> ⚠️ **`target_state` 的推导必须让 `archived` 压过 `published`，别直接看 `published_snapshot` 有没有值。**
> **归档不清空 `current_publication_id`**——`transition_lifecycle` 的 UPDATE 只写
> `archived_at` / `archived_by_admin_id`（`repository/lifecycle.rs:13-19`）。所以对
> 「已发布、随后被归档」的目标，上面的 `LEFT JOIN` 和 `EXISTS` **都会命中**，
> 天真地按「有 publication 就是 published」推导会算出 `published`；
> 而发布门的 `resolve_current_published_senses_for_publish` 带
> `AND entry.archived_at IS NULL`（`repository/publications.rs:498-500`），会判它不可用。
> 结果就是**前端显示正常、发布却被拒**——正是这份评估想消灭的那类矛盾。
> 正确顺序：**`archived → published → detached → draft`**。
> 注意 `published` 要压过 `detached`：目标把一个**已发布**的词义从自己草稿里删掉后，
> 该词义在目标的**当前发布版本里依然有效**——`resolve_current_published_senses_for_publish`
> 查的是 `entry_publication_nodes`，**完全不看 `lexicon.nodes.removed_from_draft_at`**
> （`repository/publications.rs:486-511`）。这种目标仍可正常发布，标成 `detached`
> 会让前端把一条完全可用的关联提示成失效。

`resolve_meaning_references`（`publishing.rs:911-1092`）按 `mode` 分流：`Canonicalize` 用上面这条，
`Verify` 走原路。**`Canonicalize` 分支下 `publication_references` 是被丢弃的**
（`editing.rs:626` 与 `editing.rs:220` 都只读 `.issues`），所以放宽不会污染发布引用表。

> ⚠️ **放行档必须显式写值，不能沿用 `continue`。**
> 现在的回填循环是 `let Some(snapshot) = resolved.get(&key) else { continue; };`
> （`publishing.rs:1033-1035`）——**解析不到就原样保留客户端传来的值**。今天这不可见，
> 只因为解析不到必然先 422 了。放宽之后如果放行档还走 `continue`，客户端就能自填
> `target_headword` / `target_gloss`（以及新加的 `target_state`）一路落进
> `entry_editor_projection.meanings`（`repository/entries.rs:437-447`）、再进发布快照。
> DTO 上的 `#[schema(read_only)]` 只是 OpenAPI 标注，serde 既不拒绝也不清洗，
> `WordRelationV2` 也没有 `deny_unknown_fields`。
> **放行档要么写入服务端算出的草稿值，要么显式清空，绝不能放任客户端值穿透。**

**例句 `context` 链接建议保持严格**：`resolve_meaning_references` 把 relation 和
sentence_context 放在同一个 `resolved` map 里，但 issue 是按 `usage.kind` 分别产出的
（`publishing.rs:1011-1023`），所以「relation 放宽、sentence_context 不放宽」只需在那个
match 上加一条判断即可。本轮需求只提了近义/反义/派生，**不顺手扩大范围**；产品若要一并
放宽，是一行的事。

### 2.5 副作用清单（含你没点到的几条）

| # | 副作用 | 严重度 | 处置 |
| --- | --- | --- | --- |
| 1 | **删除草稿会 500** —— 目标草稿被 `delete_draft` 硬删的瞬间，源词条正在写 `lexicon.relations`，外键 RESTRICT 会炸成 `Database` 错误 → 500 | 中（窄竞态，但今天不可能发生，是新开的口子） | **两侧都要处置**：① 保存侧在 `map_entry_write_error` 里把 `lexicon_relations_target_fkey` / `lexicon_sentence_links_target_fkey` 的 23503 映射成 422 `relation_target_unavailable`（可复用 `platform/db.rs` 的 `is_foreign_key_violation`）；② 删除侧在 `delete_never_published_entry` 的 DELETE 上识别同样两个约束，映射成 409 `EntryNotDeletable`，与护栏返回 `false` 的语义对齐。<br>**不建议在保存路径上加锁**：`save_meanings` / `save_forms` 都先 `entry_by_id_for_update`（`repository/entries.rs:593`）锁住自己的 entry 行，再去解析目标；A↔B 互为近义词并发保存时，若再对目标取 `FOR KEY SHARE`，就是「各持己方 `FOR UPDATE`、互求对方 KEY SHARE」的经典死锁。<br>**那为什么发布路径可以用 `FOR SHARE OF entry NOWAIT`？**（`repository/publications.rs:510`）因为发布是低频、显式、可重试的动作，55P03 → `TargetPublicationBusy` → 409（`entries.rs:13-21`）对管理员是可理解的「稍后再试」；而保存是高频自动动作，同样的 409 会把「对方管理员正好在存」变成误伤 |
| 1b | **既有的发布期 409 会变常见** —— `resolve_current_published_senses_for_publish` 的 `FOR SHARE OF entry NOWAIT` 命中率随跨词条关联增多而上升 | 低（既有机制，非新增） | 不改机制。但前端要把 `TargetPublicationBusy`/409 的重试提示做好，这条今天几乎不出现，放宽后会 |
| 2 | **目标草稿删不掉，且不知道被谁引用** —— `delete_never_published_entry` 的护栏返回 `false` → `EntryNotDeletable`（`lifecycle.rs:95`），而这个错误 **不带任何 reference 列表**（`handler.rs:209-213`），跟 `EntryHasInboundPublicationRefs` 那种会回出 `reference_locations` 的不一样 | **高**（这是 Q5 里唯一真正无解的中间态） | 让 `delete_never_published_entry` 回出引用清单，`EntryNotDeletable` 的 `ProblemMeta` 跟着带 `reference_locations`。草稿引用没有 publication，**建议给它另开一个 `draft_reference_locations` 字段**，而不是把既有 `ProblemReferenceLocation.source_publication_id` 改成 `Option`——后者是对两个已发布错误响应的破坏性变更，理由见 §6 包 1 的告警框 |
| 3 | **发布时通常先来一次 `relation_target_stale`** —— 保存时快照写的是草稿值，目标发布后 `Verify` 会拿它跟发布快照比字符串（`publishing.rs:1042-1043`） | 低 | 已有错误码与文案（「请重新保存词义步骤」），前端已能处理。<br>**别写成「必然」**：那就是两个普通字符串相等比较。若目标的词头与首条中文释义在源词条保存后没变，两侧相等，不产生 stale；目标若**仍未发布**，报的是 `relation_target_unavailable`（`publishing.rs:1011-1016`）而不是 stale。<br>另外 §2.4 的草稿回填口径要与 `published_word_headword`（`publishing.rs:1114-1120`，`" / "` 拼接）、`published_sense_gloss`（`1122-1132`，首条中文释义）对齐，否则会平白多出一轮 stale |
| 4 | **词义步可以被标记「完成」而关联仍不可解析** —— `validate_meanings`（`validation/meanings.rs:403-434`）对 relation 除了 `unique_node` 登记节点 ID 唯一性，只校验 `score` 与 `relation` 枚举，**从不校验目标可用性** | 低（符合「发布门不动」的约束） | 不改。但要说清**拦住它的不是校验函数**：是 `publish` 与 `POST /entries/{id}/validate` 各自那次独立的 `resolve_meaning_references(Verify)`（`publishing.rs:32-41`、`196-208`）。前端预览步能提前看到 |
| 5 | **归档不受草稿关联阻挡** —— `archive` 只查发布层入链 | 低 | 按 §2.3 的「放行 + 标注」设计，这不再是问题（保存不会因此失败）。发布门仍会拦 |
| 6 | 存量数据 | 无 | `docs/frontend-integration.md` §9.4 记录「线上 `lexicon.relations` 目前 0 行、草稿里也没有关联数据」。落地前复核一次即可，**无需回填** |
| 7 | **AI 补全会把关联赖以存在的 sense 节点整批换掉** —— `content_completion` 每个 sense 都是新 `Uuid::now_v7()`（`content_completion/worker.rs:203`）、`relations: Vec::new()`（`worker.rs:260`），结果按 pos 整份组装（`content_completion/repository.rs:464-472`）。而 §1.2 情形 A 推荐的正是「从 `create` 响应里取占位词义 `sense.id` 建关联」。**「新建草稿 → 立刻关联 → 目标再跑一次补全并保存」是最自然的操作顺序**，一旦发生，占位 sense 被 `removed_from_draft_at` 标掉，源词条的关联立刻变 `detached` | **高**（是主推流程自带的坑） | 后端拦不住也不该拦（补全本身不产关联、不写草稿，只把结果存 job 表）。**必须靠产品/前端**：要么补全结果落盘时保留既有 sense ID，要么在「该词义已被别的词条关联」时对补全覆盖给出显式确认。这条建议单独提给产品 |
| 8 | **impact_store 从罕见路径变成常态路径** —— `pos_meanings_have_content`（`service/editing.rs:1199`）里把 `!sense.relations.is_empty()` 并进了「有内容」的或运算。占位词义一挂上关联，改/删该词性就从静默重建变成 `downstream_required`，必须带 `confirmed_impact_token`（`editing.rs:427`） | 中 | 机制是既有的，不用改。但「先建空壳草稿再挂关联」成为常态后这条路径会频繁触发，前端要确保确认流走得通，§7 补测试 |
| 9 | **不受影响的（写下来省得重复排查）** | —— | `surface_projection_sources` 只读 headwords + forms（`repository/surface_writes.rs:56-101`），meanings/relations **完全不进 surface**——同形确认、`confirm_visibility_command`、`surface_backfill` 都不被牵动。`validate_node_identities` 对 relation 节点只校验 `(node_type, parent = sense.id, RELATION_ROLE)`（`validation/structure.rs:349-360`），不看目标。`lock_node_ids` 是 `pg_advisory_xact_lock` 且只锁自己的节点 ID（`repository/publications.rs:397-419`），不参与跨词条竞争 |

---

## 3. 请求 2：`target_headword` / `target_gloss` 指向草稿时填什么

### 3.1 事实

- 两个字段在 wire 上是 `Option<String>` + `#[schema(read_only)]`（`dto/aggregate.rs:382-387`），
  **对可解析的目标**，客户端传什么都会被服务端覆盖——伪造入参见
  `tests/lexicon_handler.rs:2718-2719`，断言在 `2745-2748`
  （测试 `published_sense_references_are_resolved_snapshotted_and_protected`），
  另有 `4811` 的 `relation_target_headwords_follow_the_source_dialect`。
  **注意 `read_only` 只是 OpenAPI 标注**，serde 不拒绝也不清洗；对**解析不到**的目标
  今天走 `continue`（`publishing.rs:1033-1035`）会原样保留客户端值——放宽后这一点会变成真问题，
  见 §2.4 的告警框。
- 落库时是 `TEXT NOT NULL`，写入前 `.unwrap_or("")`（`repository/projections.rs:485-486`），
  **所以 `None` 和 `""` 在库里没有区别**。
- 草稿侧的值取得到：词头在 `lexicon.entries` + `lexicon.entry_headwords`
  （取法与 `entry_by_id` 的 `repository/entries.rs:554-556` 一致），
  释义在 `lexicon.entry_editor_projection.meanings` 这个 JSONB 里。

### 3.2 建议

**`target_headword` 填草稿当前词头，`target_gloss` 填草稿当前首条中文释义（新建草稿必为 `""`）。**

理由：留空会让前端在「刚建的草稿」和「目标已被删词义」之间完全失去区分能力，而词头是
唯一稳定可读、且对管理员有意义的信息（「reliability（未补录）」远好过「（未知词条）」）。

### 3.3 但只靠这两个字段不够 —— 建议新增一个只读字段

前端要区分「草稿目标」和「已发布目标」，唯一可用的信号会是「`target_gloss` 是不是空串」。
这条推断**碰巧成立，但它依赖两条互不相干的校验叠加，比看上去更脆**：

- `published_sense_gloss`（`publishing.rs:1122-1132`）取的是**首条 Zh 定义**，而不是「首条非空 Zh」；
- `native_definition_required`（`validation/meanings.rs:322-331`）只保证「**存在某一条**非空中文释义」，
  堵不住「首条 Zh 为空、第二条非空」；
- 真正堵住它的是另一条：`definition_invalid`（`validation/meanings.rs:309-320`），
  任何一条 Zh 定义 trim 后为空都会单独产出 issue，发布门见 issue 即整体 422。

**两条规则叠加才有「已发布 sense 的首条 Zh 必非空」。** 动其中任意一条，前端的这条推断就悄悄崩掉。
这恰恰是应该加显式字段、而不是让前端去推断的理由。

建议给 `WordRelationV2` 加一个只读枚举字段：

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
#[schema(read_only)]
pub target_state: Option<RelationTargetStateV2>,  // 见下方注记：定稿为五态
```

- `published`：目标当前发布版本里有这个词义（今天唯一存在的情形，快照取自 publication）
- `draft`：目标存在、未归档，但该词义只在草稿里 → 前端渲染「（未补录）」
- `archived`：目标词条已归档 → 前端提示**恢复目标词条**
- `detached`：该词义已被移出目标草稿（`removed_from_draft_at`）**且不在目标的当前发布版本里** → 前端提示**重选词义或删除关联**

**`archived` 与 `detached` 必须分开，不能合成一个 `unavailable`**：两者的正确处置相反。
归档目标该做的是 restore（恢复后关联立即可用），词义被移除才是「必须重选或删掉」。
合成一个状态，前端只能二选一给提示，另一半必然误导管理员去删掉本可以救回来的关联。

> ⚠️ **上面这段已被 [`target-state-criteria.md`](./target-state-criteria.md) 修订，以那份为准。**
> ① 状态定稿为**五态**：`published` 拆成 `resolved` / `resolvable`（后者 = 目标已发布，
> 但本词条存的快照、或本词条的当前发布，还没跟上），判定顺序见该文 §2.2；
> ② 它**不是保存期快照，而是响应期现算**的派生字段，不落库、不进发布快照（该文 §1.5）。
> 保存期快照那套做法会让「目标日后发布 / 归档」永远反映不到界面上，反而制造新的静默矛盾。

**语义注意（要写进前端说明）**：它和 `target_headword` / `target_gloss` 不同——后两者是保存那一刻
写死的快照，`target_state` 是每次读取现算的派生值，因此是**弱一致的编辑期提示**，不用于发布把关。

**契约影响**：`WordRelationV2` 增 1 个可选只读字段 + 1 个新枚举 schema，纯增量、
`required` 不变。需要重导 `docs/openapi.json`，前端重跑 `sync:openapi`。

---

## 4. 请求 3：发布规则三方案评估 + 第四条路

先把发布快照模型说清楚，三个方案的代价全从这里推出来：

> `insert_publication`（`repository/publications.rs:7-233`）做**三**件事：
> ① 把整个 `AdminWordV2` 序列化进 `entry_publications.snapshot`，并算 `snapshot_hash`；
> ② 把本次发布的**节点集合**写进 `entry_publication_nodes`——注意它
> **不是从 snapshot 派生的**，而是 `SELECT ... FROM lexicon.nodes WHERE entry_id = $2
> AND removed_from_draft_at IS NULL`（`publications.rs:56-60`）；relation 节点在上一次草稿保存时
> 就已经写进 `lexicon.nodes` 了（`repository/projections.rs:459-467`）；
> ③ 为每条**已解析**的关联写一行 `entry_publication_sense_refs`，其外键**钉死到目标的
> 那一次具体发布**（`migrations/20260811130000_create_lexicon_publications.up.sql:126-129`）。
> 归档/改版/回滚的完整性校验（`current_inbound_sense_refs`、
> `unavailable_outbound_sense_refs_for_publication`）全部建立在 ② 之上。
> **发布快照是不可变的**：`activate_publication` 靠回放历史 snapshot 工作
> （`publishing.rs:387-396`），`snapshot_hash` 是内容指纹，outbox 事件已经发出去了。

### 方案 A · 阻止发布

- **代价**：0 行代码（今天就是这样）。
- **死锁是真的**：A、B 互为近义词，两边都发不出去。逃生路径存在但极其反直觉——
  删掉 A→B 的关联、发 A、发 B、再把 A→B 加回来、**再发一次 A**（三次发布 + 一段假的
  发布历史）。没有任何 UI 提示会告诉管理员这条路。
- **若产品仍选 A，必须同时给出破环手段**，最低限度是把上面这条配方做成错误响应里的
  可执行提示（`relation_target_unavailable` 的 issue 里带上目标词条 ID 和「先发目标」按钮）。
  即便如此，成对录入是这个功能的**主场景**，A 等于把主场景留给了手工绕行。

### 方案 B · 发布时丢弃未解析关联

- **看起来免费，其实不是**：`publication_references` 的构造本来就会跳过未解析项
  （`publishing.rs:1060-1086` 的 `let Some(snapshot) = ... else { continue }`），
  但 **`snapshot` 是整个 `word.meanings` 原样序列化的**（`publications.rs:24`）。
  什么都不做的话，发布快照里**留着一条没有 `entry_publication_sense_refs` 兜底、
  指向未发布词义的悬空关联**——归档校验看不见它，回滚校验看不见它，C 端将来渲染会拿到死链。
- **所以 B 必须真的把关联从 `word.meanings` 里剔掉再序列化**。这就带来代价：
  - **剔掉快照还只做了一半**：`entry_publication_nodes` 走的是 `lexicon.nodes` 而不是 snapshot
    （`publications.rs:56-60`），relation 节点照样进本次发布的节点集合，于是出现
    「节点在、快照里没有这条关联、也没有 sense_ref」的三方不一致，而 DB 层不会替你发现它。
    B/D 要么同时过滤节点集合，要么发布前把这些 relation 节点标 `removed_from_draft_at`。
  - 发布快照不再等于草稿第 N 版 → 「发布是某一版草稿的忠实快照」这个心智模型破了。
  - `publishing.rs:155` 的 `current_publication_source_revision == word.revision` 短路
    （同版本重发直接复用旧发布）在「剔除过内容」的前提下语义变模糊。
  - 管理员录的东西**静默消失**——报告自己也点了这一条。
- **工作量**：中等（1～2 人日）。**风险点集中在快照忠实性**。

### 方案 C · 挂起后补

- **撞穿不可变发布快照模型**。要让关联「等目标发布后自动补进快照」，只有两条路：
  1. **改写已有的 `entry_publications.snapshot`** —— 直接违反不可变性：`snapshot_hash`
     对不上、`activate_publication` 回放的历史被篡改、outbox 事件已发出无法追溯。**不可行**。
  2. **目标发布时自动给源词条产生一次新发布** —— 等于替另一个管理员执行发布动作。要连带
     解决：actor 是谁（审计写谁）、`revision` / `lifecycle_revision` 怎么推、幂等键从哪来、
     surface 投影与同形确认（`confirm_visibility_command` 需要人来确认 token）怎么走、
     级联深度（A 发布触发 B 发布触发 C 发布…）如何收敛、失败了怎么办。
- **工作量**：高（周级），且引入一套全新的异步补偿语义。**投入产出比最差。**

### 方案 D（推荐）· B 的诚实版：剔除 + 显式回收清单

把 B 的「静默消失」换成「可见的待办」，其余不变：

1. **发布时**：未解析的关联**不进 `entry_publication_sense_refs`，也不进 snapshot**
   （同 B），但**留在草稿里**，`target_state` 仍是 `draft`。发布响应/发布前的
   `POST /entries/{id}/validate` 明确回出「本次发布有 N 条关联因目标未发布而未生效」。
2. **目标发布后**：源词条的关联自然变成可解析。给一个只读查询回答「哪些**已发布**词条的
   草稿里，还有已经能解析、但没进当前发布快照的关联」——本质就是
   `lexicon.relations`（草稿层）与 `entry_publication_sense_refs`（发布层）做差集，
   一条 SQL，无新表、无新数据模型。
3. **管理员看到待办 → 先重存词义步、再发布**。

> ⚠️ **不能只说「点重新发布」**：`publishing.rs:155-193` 有一条短路——
> `current_publication_source_revision == Some(word.revision)` 时直接回 201 + `Published` 并
> `return`，**位置在 `resolve_meaning_references`（`publishing.rs:196`）之前**。
> 也就是说目标发布后，源词条若没重存过（revision 没变），再点发布**既不产生新发布、
> 也不报任何错**，前端拿到 201 会以为补发成功。
> 所以 D 的第 3 步口径必须是「**重存词义步（revision +1）→ 再发布**」；
> 若要让「没重存就重发」也有信号，得额外改这条短路，这笔工作量要算进 D。
>
> **补充（见 [`target-state-criteria.md`](./target-state-criteria.md) §3.1）**：这条短路
> **只能改成报错，不能改成放行**。`lexicon.entry_publications` 上有
> `UNIQUE (entry_id, source_revision)`（migration `:21`），而 publish 全程不改 `entries.revision`，
> 所以「同一个 revision 再发一次」在结构上不可能——硬发会撞唯一约束，且该约束不在
> `map_entry_write_error`（`repository/entries.rs:6-11`）的识别名单里，会变成 500。
> 「重存 → 再提交生效」因此是结构性的唯一出路，不是文案偏好。

**为什么推荐 D**：

- **没有死锁**：任一边都能先发。
- **没有静默丢失**：关联始终在草稿里可见，且有明确的待办清单指回来。
- **不动不可变快照模型**：所有发布都是人触发的正常发布。
- **不新增数据模型**（这是相对 C 的核心优势）——「挂起」状态不需要存，它是
  「草稿有、发布没有」的**推导结果**，天然自愈、不会有孤儿。
- **工作量**：B 的量 + 待办查询 + 短路信号 ≈ **3～5 人日**（含测试）。**这是粗估**，
  下面三条都还没验证过实现难度。

**D 的已知代价与未验证点**（要如实告诉产品）：

- 成对关联要**两轮发布 + 中间一次重存**才完整生效（发 B → 回到 A 重存词义步 → 再发 A）。
  这是不引入自动化补偿的必然代价，但每一步都有 UI 指引，不是 A 那种要靠管理员自己想明白的绕行。
- 发布快照与草稿在「未解析关联」这一点上会不一致。需要在 `word-data-model.md` 里写死这条口径。
- **「一条 SQL 就能算出待办」尚未验证**：差集要跨 `lexicon.relations`（草稿层）与
  `entry_publication_sense_refs`（发布层，锚 `target_publication_id`），还要排除本节开头
  ② 那种「节点在、引用不在」的情况。落地前先把这条查询写出来验一遍，别把它当已知量。

### 推荐结论

| | A | B | C | **D** |
| --- | --- | --- | --- | --- |
| 死锁 | ❌ 有 | ✅ 无 | ✅ 无 | ✅ 无 |
| 静默丢数据 | — | ❌ 有 | ✅ 无 | ✅ 无 |
| 冲击发布快照模型 | ✅ 无 | ⚠️ 快照≠草稿，**且发布节点集合也不一致**（见本节 ②） | ❌ 撞穿不可变性 | ⚠️ 同 B |
| 新数据模型 | 无 | 无 | ❌ 需要 | ✅ 无 |
| 工作量 | 0 | 1–2 人日 | 周级 | 3–5 人日（粗估） |

**推荐 D。若产品要压工期，退而求其次选 B，但必须把「本次发布有 N 条关联未生效」的提示做出来
——B 减去这个提示就是报告里说的「悄悄消失」。A 不建议，除非产品接受成对录入必须手工绕行。**

---

## 5. Q5：会不会产生「存得下、发不出、也删不掉」的中间态

分三个主体看，结论是**会有中间态，但只有一处需要新代码来解开**：

| 主体 | 状态 | 有没有自救路径 |
| --- | --- | --- |
| **源词条 S** | 存得下（放宽后）、发不出（发布规则未定时 = 方案 A 的现状） | ✅ **有**。S 的词形/词义步随时可再存（按 §2.3 的设计，保存永远不会被目标状态卡住），删掉那条关联即可发布。S 若从未发布也随时可删——`delete_never_published_entry` 的护栏查的是**入链**，S 的出链不影响 S 自己被删 |
| **目标草稿 T** | 删不掉：`delete_draft` → 409 `EntryNotDeletable` | ⚠️ **有路径但看不见**。出路是「去 S 里删掉关联」，但 409 **不告诉管理员 S 是谁**（`handler.rs:209-213` 不带 `reference_locations`）。**这是唯一真正的死结，必须配套修**（§2.5 第 2 条） |
| **目标草稿 T** | 归档：允许（草稿关联挡不住归档） | ✅ 有。归档后 S 仍可保存（§2.3 的放行档），发布被拦，UI 显示「已失效」 |

**所以答复是：先放宽保存、发布规则后定，是安全的**——只要同时做 §2.5 第 2 条（把引用清单
放进 `EntryNotDeletable` 的响应）。没有这一条，管理员会遇到一个「删不掉又查不出原因」的词条。

另外提醒一句：**在发布规则定下来之前，中间态数据是「关联存在于草稿层、不存在于发布层」，
这恰好就是方案 D 的常态**。也就是说，放宽保存这一步**天然沿着 D 的方向走**，不会为 B/C 造成
返工，只有选 A 才需要把这些草稿关联再倒回去清理。这一点也支持先做保存放宽。

---

## 6. 若产品拍板后的落地清单

分成三个可独立上线的包，**包 1 不依赖发布规则结论**。

> **关于工期**：下面的人日是**按改动点数粗估的**，没有做过实现难度验证，评审时请当量级看、
> 不要当承诺。三个包里包 2 的不确定性最大（见其中的设计取舍）。

### 包 1 · 保存放宽（2.5～4 人日）

| 项 | 位置 |
| --- | --- |
| 新增放宽版引用解析查询（§2.4 的 SQL）+ 新 record 类型 | `repository/publications.rs` |
| `resolve_meaning_references` 按 `mode` 分流；`Canonicalize` 用新查询，relation 放行、sentence_context 保持严格 | `service/publishing.rs:911-1092` |
| 草稿侧 `headword` / `gloss` 提取（对齐 `published_sense_snapshot` 的形状） | `service/publishing.rs:1094-1132` |
| `WordRelationV2` 增 `target_state` 只读字段 + 新枚举 | `dto/aggregate.rs:376-389`、`openapi.rs:225` |
| 外键违约映射成 422，不再 500 | `repository/entries.rs:6-11` |
| `EntryNotDeletable` 带上 `reference_locations` **并能区分两种原因**——`handler.rs:209-213` 那一条同时覆盖「已发布/已归档不可删」（`lifecycle.rs:71-73`）和「有入链草稿引用」（`entries.rs:650-677` 返回 false），只补引用清单不够 | `repository/entries.rs:650-678`、`service/lifecycle.rs:85-96`、`error.rs:552-559`、`handler.rs:209-213` |
| 新增用例（见 §7）。**既有断言用例预期不需要改**——放宽不改变已发布目标的行为，`target_state` 是可选字段不影响 `assert_eq!` 取值；当成回归项核对即可 | `tests/` |
| 重导 `docs/openapi.json`；`frontend-integration.md` 补一节 | —— |

> ⚠️ **包 1 里藏着一个破坏性契约变更，别跟 `target_state` 的「纯增量」混为一谈。**
> `ProblemReferenceLocation` 五个字段全部没有 `skip_serializing_if`（`error.rs:552-559`），
> 且已经在 `EntryHasInboundPublicationRefs` / `EntryHasUnavailablePublicationRefs`
> 两个**已发布**的错误里输出（`handler.rs:214-252`）。把 `source_publication_id` 改成
> `Option<Uuid>` = 把已发布 schema 的 required 字段降级为可选。要么接受这个破坏性变更并通知前端，
> 要么给草稿引用另开一个 `draft_reference_locations` 字段。**建议后者**。
>
> 另两条实测：`tests/lexicon_schema.rs` 全是 DB 约束测试，没有任何 DTO/OpenAPI 断言，
> 新增 `target_state` 对它**零影响**；仓库 CI 里**没有 openapi.json 漂移门禁**，
> 「重导 openapi.json」纯靠人工纪律，评审时要专门看一眼。

### 包 2 · 关联选择器能看见草稿（**≥ 2～3 人日，且含一个设计取舍**，解 §1.2 情形 C）

**这不是「加个 query 参数」，先前 1 人日的估算站不住。** 两个硬点：

1. **等于写第二份查询**：`related_search` 每一列都从 `publication.snapshot` 里 `#>>` 出来，
   `pos_labels` 走 `entry_publication_part_of_speech_refs`（`repository/query.rs:26-75`）。
   草稿两样都没有，词义得改从 `entry_editor_projection.meanings` 取。
2. **游标语义会被迫改口径**：游标绑 `related_search_dataset_version`，而它**故意只数
   `lexicon.entry` / `lexicon.entry.lifecycle` 两类 outbox**（`query.rs:8-18`，注释原文
   「草稿保存与其他业务事件不会让已签名游标无故失效」），草稿保存发的是
   `lexicon.surface_projection`（`repository/surface_writes.rs:409-417`）。把草稿放进结果集后
   只能二选一：**要么翻页漏/重**，**要么把草稿保存也算进版本**——后者意味着任何人存一次草稿，
   所有打开的下拉游标全部 `related search targets changed; restart the search`
   （`service/queries.rs:201-206`）。

**这是设计取舍，要产品/前端一起定**，不是后端能单方面加的功能。
一个折中方向：关联选择器不复用 `related_search`，另开一个**不分页、按前缀限量返回**的
草稿检索端点，绕开游标问题。

### 包 3 · 发布规则（按产品结论，D 约 3～5 人日）

见 §4。

### 未解决、需要产品先定的

- **§1.2 情形 B**：词典未命中的词，新建草稿零词义。是「不允许对这类词就地建关联」，
  还是「建的时候强制选一个词性」，还是「后端在没有 forms 时也播一个无词性的占位词义」？
  第三条会动 `build_initial_meanings` 的不变量，**不建议**。
- **AI 补全与关联的冲突**（§2.5 第 7 条）：目标词条跑补全会换掉全部 sense ID，把指向它的
  关联打成 `detached`。是「补全落盘时保留既有 sense ID」，还是「该词义被别人关联时对覆盖
  给出显式确认」，还是「接受它、由 UI 提示重新选择」？**这条不定，主推流程就有一个自带的坑。**
- **`ProblemReferenceLocation` 的破坏性变更 vs 另开字段**（§6 包 1 告警框）。

---

## 7. 测试清单（落地时）

- 保存：关联指向草稿词义 → 200，`target_state = "draft"`，`target_headword` = 草稿词头，`target_gloss` = `""`
- 保存：关联指向不存在的 sense_id → **422**（不是 500）
- 保存：关联指向 `node_type != 'sense'` 的节点 → 422
- 保存：目标被归档后，源词条的**词形步**和**词义步**仍能保存（§2.2 的回归点）
- 保存：目标把该词义移出草稿后（**且该词义从未发布过**），源词条仍能保存，`target_state = "detached"`
- **保存：目标把一个「已发布」的词义移出草稿 → `target_state` 仍是 `"published"`**
  （该词义在目标当前发布版本里依然有效，发布期解析不看 `removed_from_draft_at`）
- **保存：目标「已发布后又被归档」→ `target_state` 必须是 `"archived"` 而不是 `"published"`**
  （归档不清 `current_publication_id`，这是 §2.4 告警框那条坑的直接回归测试）
- 发布：关联目标未发布 → 按最终方案断言（A：422 / D：201 且该关联不在 `entry_publication_sense_refs`、不在 snapshot）
- 发布：目标先发布 → 源词条不重存直接发布应得 `relation_target_stale`；重存后发布成功
- 删除：目标草稿被草稿关联引用 → 409 且响应里能定位到源词条
- 死锁回归：A↔B 互为近义词，两边并发保存不 500、不死锁
- 竞态：删除目标草稿与保存关联并发 → 二者其一得到 409/422，不出现 500
- **只读字段不可注入**：目标不可解析时，客户端自填的 `target_headword` / `target_gloss` /
  `target_state` **不得**落库、不得进发布快照（对应 §2.4 告警框，这是放宽后最容易漏的一条）
- **impact 路径**：给占位词义挂上关联后改/删该词性 → 必须要求 `confirmed_impact_token`
  （`pos_meanings_have_content` 因 `!relations.is_empty()` 判定有内容）
- **AI 补全交互**：目标词条跑补全并保存后，指向它的关联落到 `target_state = "detached"`，
  且源词条**仍能保存**（不得回到 422）
- **发布短路**：目标发布后源词条不重存直接发布 → **断言 422**（`relation_target_stale` 或
  `relation_ref_not_in_publication`），不再是 201 no-op。完整断言集合见
  [`target-state-criteria.md`](./target-state-criteria.md) §5
- 回归：`tests/lexicon_handler.rs` 既有的「客户端伪造值被服务端覆盖」用例仍然通过

---

## 8. 回滚

包 1 **不是**纯增量：`target_state` 那一半是（前端按 `skip_serializing_if` 本就要容忍缺省），
但若采用「`ProblemReferenceLocation.source_publication_id` 改 `Option`」的做法，那是对两个
**已发布**错误响应的破坏性变更，回滚它会再破坏一次前端。**这也是 §6 建议改为另开
`draft_reference_locations` 字段的原因**——那样两边都是纯增量，回滚 = revert。**唯一不可逆的是数据**：放宽后落库的草稿关联在回滚后会让对应词条的
词形/词义步重新变成 422（§2.2）。若需要回滚，得先清掉指向未发布目标的
`lexicon.relations` 行。上线前的 0 行现状（§2.5 第 6 条）让这个风险目前接近于零。
