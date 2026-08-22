# `target_state` 判据与「不重存直接重发」短路：定稿

> **实现更新（2026-08-22）**：本文基于“草稿目标不能进入来源 publication”的旧前提，已被
> [`implemented-stable-target-anchors.md`](./implemented-stable-target-anchors.md) 取代，保留为历史评估。

> **接续**：`backend-assessment.md`（PR #48）§3.3 与 §4。本文回答前端在
> tsz 仓库 PR #158（`docs/features/related-word-draft-creation/design.md`
> 「后端核验结论 · 仍需后端确认的一条」）提出的缺口，并把 `target_state` 的最终判据定死。
>
> **核对基线**：`tsz-rust` `main` @ `2028ffd`（2026-08-22）。行号即该 commit 的行号。
>
> **状态**：仍是评估/定稿文档，**未动任何代码**。包 1 / 包 3 都还没落地，
> 这里定的是它们落地时要照着写的判据。
>
> **一句话**：前端的发现**成立**，四环全部核实通过；但它比前端描述的还要宽一点
> （`resolvable` 的另一半——快照过期——**今天就已经**在同一条短路上静默 201），
> 而且有一条 DB 硬约束决定了短路**只能报错、不能放行**：
> `UNIQUE (entry_id, source_revision)`（`migrations/20260811130000_create_lexicon_publications.up.sql:21`）
> 让「同一个 revision 发两次」在结构上不可能。所以「重存 → 再提交生效」不是文案偏好，是唯一出路。

---

## 0. 结论摘要（前端照这个实现）

1. **发现成立**。`resolved` 只按字符串比对会漏掉「算出来一致、却从没进过本词条当前发布」的组合。
2. **`target_state` 必须是「读时现算」的派生字段，不入库、不进发布快照**。
   这是本条修复成立的前提，也是对 `backend-assessment.md` §3.3「保存那一刻的快照」那句话的**纠正**（§1.5）。
3. **判据加一层，但不加新状态**：沿用前端已定的五态，把「本词条当前发布未收录该关联」并进 `resolvable`。
   完整判定顺序见 §2.2。
4. **门开在「本词条没有未发布改动」上**，不是「只要没收录就报」。理由见 §2.3——
   否则每一条新加的关联都会被标成 `resolvable`，而它需要的是「发布」不是「重新保存」。
5. **短路修复与状态判据共用同一个谓词**，因此三个界面口径天然一致：
   **关联行显示 `resolvable` ⟺ `POST /validate` 报 issue ⟺ 点「提交生效」得到 422**（§3）。
6. **短路只能改成报错**。`UNIQUE (entry_id, source_revision)` 禁止同一 revision 产生第二次发布，
   「让它直接发出去」需要动发布表的主键语义，不在本功能范围内（§3.1）。

---

## 1. 逐环核实

### 1.1 环 1：字符串的两个来源 ✅

`ReferenceResolutionMode::Verify` 比的两个字符串都由 `published_sense_snapshot`
（`src/lexicon/service/publishing.rs:1094-1112`）从**目标当前发布的 `entry_publications.snapshot`**
现场反序列化后算出：

- 词头 `published_word_headword`（`:1114-1120`）= `ordered_headword_sides` 按目标自己的
  `source_dialect` 排序后 `" / "` 拼接（`service/helpers.rs:46-58`）；
- 释义 `published_sense_gloss`（`:1122-1132`）= 该 sense 的**首条**中文定义（`ZhDefinition` 或
  `ZhSentence`）的纯文本，取不到就 `unwrap_or_default()` → 空串。

两者都是**纯字符串**，没有任何版本号、发布 ID 或引用表参与。✅

### 1.2 环 2：`Verify` 的比对分支 ✅

`publishing.rs:1041-1052`：

```rust
ReferenceResolutionMode::Verify => {
    if relation.target_headword.as_deref() != Some(snapshot.headword.as_str())
        || relation.target_gloss.as_deref() != Some(snapshot.gloss.as_str())
    { ... "relation_target_stale" ... }
}
```

两个 `!=`，仅此而已。`resolved` map 的构造（`:990-1004`）也只装 `(target_publication_id, headword, gloss)`。
**整条解析路径不查 `entry_publication_sense_refs`**。✅

### 1.3 环 3：`entry_publication_sense_refs` 的写入时机 ✅

全仓只有一处 INSERT：`repository/publications.rs:113-134`，在 `insert_publication` 里，
数据源是 `reference_resolution.publication_references`。而那个 Vec 的构造
（`publishing.rs:1060-1066`）以 `let Some(snapshot) = resolved.get(&usage.target) else { continue; };`
开头——**解析不到就不产行**。

所以在「显式 B」下（未解析关联留在草稿、留在 snapshot、但不进引用表），
S 那次发布**确实不会**为这条关联写行。✅

> 顺带核实前端 Q1 的口径：`insert_publication` 把 `word` 整体序列化进 snapshot（`publications.rs:24`），
> 未解析的关联**逐字保留在 snapshot 里**；`entry_publication_nodes` 走
> `lexicon.nodes WHERE removed_from_draft_at IS NULL`（`:51-64`），relation 节点照常入集。
> 三者的唯一差异就是引用表少一行——**「对外不生效」的判据只有引用表这一处**，
> 这正是本文判据要查它的原因。

### 1.4 环 4：`publishing.rs:155` 的短路 ✅

```
155:  if current_publication_source_revision == Some(word.revision) {
156-158:    status = Published; published_revision = ...; has_unpublished_changes = false;
180:        insert_idempotent_word_response(..., 201)
193:        return Ok(AdminWordV2Envelope { word });
194:  }
196:  let reference_resolution = resolve_meaning_references(..., Verify, true)
```

短路在引用解析**之前**，直接 201 + `Published` 并 `return`。✅

并且 **publish 全程不写 `entries.revision`**：`insert_publication` 只 UPDATE
`current_publication_id` / `draft_based_on_publication_id`（`publications.rs:171`）、
`nodes.first_published_at`（`:178`）。`entries.revision` 的唯一写入点是
`replace_entry_content`（`repository/entries.rs:414-425`），只在保存步骤时 `revision + 1`
（`service/editing.rs:267` 词形步、`:677` 词义步，**无条件 +1，没有「内容没变就不 bump」的分支**）。✅

而 `has_unpublished_changes` 的定义是
`current_publication_source_revision.is_some_and(|r| r != revision)`（`service/entry.rs:56-58`）。
于是：**短路命中 ⟺ 有当前发布 且 `has_unpublished_changes == false`**。这个等价关系是 §2.3 门条件的由来。✅

### 1.5 需要纠正的一个前提：`target_state` 必须读时现算

`backend-assessment.md` §3.3 末尾写着「它……是**保存那一刻的快照**，只在源词条重存词形/词义步时刷新，
**不是目标的实时状态**」。**这句话要改**——它与前端已定的五态互斥，而且会自己造出别的洞：

- 若是保存期快照，前端设计里「目标词条日后发布 → 关联行变成 `resolvable`」（design.md）根本不会发生：
  存下来的值会永远停在 `draft`。前端会显示「草稿 / 去补录」，管理员点进去发现目标早就发布了。
- 同理，目标在保存之后被归档，存量值仍是 `published`/`resolved` → 界面无标记，
  而发布门会拒（`resolve_current_published_senses_for_publish` 带 `entry.archived_at IS NULL`，
  `publications.rs:500`）。这正是 §2.4 告警框想消灭的那类矛盾，只是换了个方向重新出现。
- 反过来，一旦读时现算，`target_state` 就**不需要落库**，§2.4 那条「客户端自填值穿透」的担忧
  对这个字段自动消失（`target_headword` / `target_gloss` 仍然要防）。

**定稿口径**：

- `target_state` 是**响应期计算**的派生字段，写库时一律为 `None`
  （`skip_serializing_if = "Option::is_none"`，所以它不会出现在 `entry_editor_projection.meanings`，
  也不会出现在 `entry_publications.snapshot`）。
- 出现在**读接口**（`GET /entries/{id}`）与**保存步骤的响应**（词形步 / 词义步）里。
- **不出现在 `publish` / `activate_publication` 的响应里**：这两个响应会被
  `insert_idempotent_word_response` 原样存进幂等表并在重放时逐字回出，
  存一个会过期的状态比不存更糟。前端发布后本来就要刷新词条。
- 弱一致是可接受的（前端 design.md 已写明它只用于编辑期提示、不用于发布把关）。

### 1.6 场景可达性

链路成立，但精确的触发条件比「S 先于 T 发布」更小一圈，写清楚免得测试用例写歪：

> **S 的最近一次发布发生在这条关联还不可解析的时候**，且此后 S 没有再保存过任何一步。

因为任何一次保存都会 `revision + 1`（词形步也会顺带跑 `Canonicalize` 刷新关联快照，
`service/editing.rs:212`），于是 `has_unpublished_changes` 变真、短路失效、下一次发布就会
正常写入引用行。所以这不是「早晚都会踩」，而是「**S 发完就不再动它**」这条路径专属——
恰恰是成对录入里最常见的收尾方式。

另外补一条前端没点到的：**同一条短路上还挂着 `resolvable` 的另一半，而且今天就成立**。
S 已发布、关联已进引用表，之后 T 改了释义再发布 → 字符串不一致 → 界面 `resolvable`
（判据的既有那一半），管理员点「提交生效」→ 同样撞短路 → 同样静默 201。
今天这条有一层薄薄的兜底：`POST /entries/{id}/validate` 走的是无短路的 `Verify`
（`publishing.rs:33-41`），会报 `relation_target_stale`，预览步能看见。
**而本次新发现的那一半连 validate 都看不见**——validate 的字符串比对同样通过。
所以修复要覆盖两半（§3.2）。

---

## 2. 定稿判据

### 2.1 一个谓词

```
Q(relation) :=
      目标当前可解析（= §2.4 的放宽查询里 published_snapshot 命中，且目标未归档）
  AND (
        本词条存的 target_headword / target_gloss 与现场算出的两个字符串不一致   -- 既有那一半
     OR 本词条当前发布的 entry_publication_sense_refs 里没有这条关联            -- 本次新增
      )
```

口语版：**「现在把词义步重存一次，本词条的发布内容会因此改变」**。

### 2.2 五态判定顺序

```rust
// target.* 来自 backend-assessment.md §2.4 的放宽解析查询（一次查完全部关联目标）
// source.* 来自 entry_by_id 已有的列 + 一次 sense_refs 查询（§2.4b）
fn relation_target_state(rel, target, source) -> RelationTargetStateV2 {
    if target.archived { return Archived; }                    // 归档压过 published，见 §2.4 告警框
    if let Some(published) = target.published_snapshot {       // 该 sense 在目标当前发布里
        if rel.target_headword.as_deref() != Some(&published.headword)
        || rel.target_gloss.as_deref()    != Some(&published.gloss) {
            return Resolvable;                                 // 既有判据
        }
        if source.current_publication_id.is_some()
        && !source.has_unpublished_changes
        && !source.published_relation_refs.contains(&(rel.id, rel.target_word_id, rel.target_sense_id)) {
            return Resolvable;                                 // ← 本次新增的一层
        }
        return Resolved;
    }
    if target.removed_from_draft { return Detached; }          // published 压过 detached，见 §2.4 告警框
    Draft
}
```

新增的那一次查询（`published_relation_refs`）：

```sql
SELECT sense_ref.source_node_id,
       sense_ref.target_entry_id,
       sense_ref.target_sense_id
FROM lexicon.entry_publication_sense_refs sense_ref
WHERE sense_ref.publication_id = $1        -- lexicon.entries.current_publication_id
  AND sense_ref.entry_id       = $2        -- 源词条 ID（冗余但让语义自明）
  AND sense_ref.reference_kind = 'relation'
```

- `current_publication_id` 为 NULL 时**整条查询跳过**，集合为空，但 §2.2 的门条件也为假 → 不影响判定。
- 走主键 `(publication_id, source_node_id, reference_kind, target_entry_id, target_sense_id)`
  的最左前缀（migration `:116-119`），**不需要新索引**。一条词条的引用行数是个位数量级。
- 匹配键必须带上 `(target_entry_id, target_sense_id)`：同一个 relation 节点 ID 被改指到别的目标时，
  旧引用行不能算数。

### 2.3 为什么门开在「没有未发布改动」上

不加这道门的话，**任何一条刚加进草稿、还没发布的关联都会被标成 `resolvable`**——
而前端 `resolvable` 的文案是「已发布」+ 行内入口「重新保存」。管理员刚保存完，被告知「重新保存」，
纯噪音；他真正要做的是「发布」，而那件事已经由全局的 `has_unpublished_changes` 提示过了。

加上这道门之后，标记只出现在**唯一一个界面上没有任何其他信号的格子里**，而且它和短路条件
（§1.4 证明的等价关系）严丝合缝：

| 本词条状态 | 关联未进当前发布 | 判定 | 管理员能看到的信号 |
| --- | --- | --- | --- |
| 从未发布（`current_publication_id` 为 NULL） | —— | `resolved` | 整条词条是草稿，状态栏已说明 |
| 已发布，有未发布改动 | 是 | `resolved` | 全局「有未发布改动」；下一次发布会自动收录 |
| 已发布，无未发布改动 | 是 | **`resolvable`** | **只有这个标记**——所以它必须存在 |
| 已发布，无未发布改动 | 否 | `resolved` | 无需信号 |

### 2.4 明确**不要**做的两件事

1. **不要比 `sense_ref.target_publication_id` 与目标当前 `current_publication_id` 是否相等。**
   目标每发一次新版都会让所有入链关联变成「锚在旧版」，若据此报 `resolvable`，
   目标发一次版就会把全库指向它的关联全部点亮，而绝大多数内容根本没变。
   现行语义（`relation_target_stale` 只看内容字符串）是对的，
   `unavailable_outbound_sense_refs_for_publication`（`publications.rs:628-661`）
   同样查的是目标**当前**发布的节点集合而不是锚定的那次发布，口径一致。
2. **不要为这一层新开一个状态。** 处置动作与既有 `resolvable` 完全相同
   （回词义步重存 → 再提交生效），前端 `RELATION_STATE_TAG` / `RELATION_STATE_ACTION` 两张映射表
   一个字都不用改。前端只需把 `resolvable` 的语义说明从「本词条的快照还没跟上」
   放宽成「**本词条的当前发布还没跟上**」。

---

## 3. 与短路修复的关系

### 3.1 先说死一条：短路不能改成「放行重发」

一个自然的想法是让 `publishing.rs:155` 在检测到「有关联变得可解析」时不再 return，
而是继续往下走、正常产生一次新发布。**这条路被 DB 堵死了**：

```sql
-- migrations/20260811130000_create_lexicon_publications.up.sql:21
CONSTRAINT lexicon_entry_publications_entry_revision_key UNIQUE (entry_id, source_revision)
```

`insert_publication` 写的 `source_revision` 就是 `word.revision`（`publications.rs:31` 列、`:39` 绑值），
而 publish 全程不改 `entries.revision`（§1.4）。所以「同一个 revision 发两次」会撞唯一约束，
且这条约束不在 `map_entry_write_error` 的识别名单里（`repository/entries.rs:6-11` 只认
headword 唯一索引），会原样吞成 `Database` → **500**。

要放行就得允许一个 revision 对应多次发布，那是发布表主键语义的变更，
连带 `publication_number`、回滚列表、`draft_based_on_publication_id` 一起动。**不在本功能范围内。**

⇒ **「回词义步重新保存一次，再提交生效」是结构性的唯一出路，不是文案偏好。**
前端 design.md 现在的措辞与行内入口（`resolvable` → 「重新保存」）是对的，可以定死。

### 3.2 短路修复的形状：在短路之前判 Q，命中就 422

```rust
if current_publication_source_revision == Some(word.revision) {
    // 新增：短路之前先问「重存一次会不会改变本词条的发布内容」
    let pending = pending_relation_refs(
        &mut transaction,
        entry_id,
        current_publication_id.expect("source_revision 存在即当前发布存在"),
        &word.meanings,
    ).await?;
    if !pending.is_empty() {
        return Err(LexiconServiceError::ValidationFailed(pending));
    }
    // ... 以下原样 ...
}
```

- **用 422 `ValidationFailed`，不新开错误码族**。`handler.rs:264-265` 已经把它映射成 422 +
  `field_issues`，前端渲染 issue 的通路是现成的；而 409 那一族
  （`unresolved_relations_confirmation_required` / `_changed`）语义是「确认后重试同一个动作」，
  这里重试同一个动作永远不会成功（§3.1），套错了。
- 产出两类 issue，形状与 `relation_target_stale` 完全一致
  （`step = meanings`、`node_id = relation.id`、`field = "target_sense_id"`）：

  | code | 触发 | message |
  | --- | --- | --- |
  | `relation_target_stale` | Q 的既有那一半（字符串不一致） | 沿用 `publishing.rs:1049` 的现有文案 |
  | `relation_ref_not_in_publication` | Q 的新增那一半（当前发布未收录） | 「这条关联尚未进入本词条的当前发布，请重新保存词义步骤后再提交生效」 |

- `pending_relation_refs` 用**不加锁**的 `resolve_current_published_senses`
  （`publications.rs:423-468`），**不要**用 `_for_publish` 那条带 `FOR SHARE OF entry NOWAIT` 的
  （`:471-516`）——这条路径不写发布，没有理由把 `TargetPublicationBusy` 409 带进一个 no-op 请求。
- **只需覆盖 relation，不用管 sentence_context**：例句 context 在保存期保持严格（§2.4），
  未解析的 context 存不下去；发布期解析失败仍是 422（显式 B 只放宽 relation）。
  所以 context 的引用行必定写过，「未收录」分支不可达。
  *若将来产品把 context 也放宽，这个谓词要跟着扩。*

### 3.3 `POST /entries/{id}/validate` 也要报同一条

否则预览步会出现「校验全绿 → 点提交生效 → 422」。validate 走的是无短路的 `Verify`
（`publishing.rs:33-41`），今天已经能报 `relation_target_stale`；把
`relation_ref_not_in_publication` 按同样的门（有当前发布 且 `has_unpublished_changes == false`）
一并加进去即可。

> ⚠️ **这与前端「`unresolved_relations` 绝对不能塞进 `issues`」那条禁令不冲突，别混。**
> 那条禁的是「目标还是草稿」——补救要靠**另一个词条**发布，塞进 `issues` 会把发布按钮灰掉，
> 等于把显式 B 偷偷做回方案 A。
> 这里补救是**本词条内一次重新保存**，管理员当场就能做完；发布按钮在做完之前本来就该灰着，
> 因为点了也只会 422。

### 3.4 三个界面口径的一致性

门条件三处相同，于是（在同一时刻、同一份数据下）：

**关联行显示 `resolvable` ⟺ `validate` 报出这条 issue ⟺ 点「提交生效」得到 422**

`resolved` 的关联则三处都安静。**修完短路仍然需要状态判据这一层**——
前端说得对：`has_unpublished_changes = false`、界面无标记时，管理员**没有任何理由**去点提交生效。
反过来只做标记不修短路，管理员会照着标记去重存（这条路是通的），
但一旦他跳过重存直接点提交生效，仍会拿到一个骗人的 201。**两件事互不替代，要同批上线。**

---

## 4. 落地清单（并进 `backend-assessment.md` §6）

### 并入包 1（保存放宽）

| 项 | 位置 |
| --- | --- |
| §2.4 的放宽解析查询**额外回出** `target_archived` / `target_removed` / `published_snapshot`（原设计已含） | `repository/publications.rs` |
| **新增** `published_relation_refs`（§2.2 的 SQL），按 `current_publication_id` 查一次 | `repository/publications.rs` |
| **新增** `target_state` 的响应期标注函数（§2.2 的判定顺序），挂在 `GET /entries/{id}` 与两个保存步骤的响应上；publish / activate 的响应不挂 | `service/entry.rs`、`service/editing.rs` |
| `WordRelationV2.target_state` 只读字段 + `RelationTargetStateV2` 枚举（**五**态，含 `resolvable`） | `dto/aggregate.rs:376-389`、`openapi.rs` |
| 写库前确保 `target_state` 为 `None`（读时现算，不落 `entry_editor_projection.meanings`、不落 `entry_publications.snapshot`） | `repository/projections.rs`、`repository/publications.rs` |

> 命名提醒：`service/lifecycle.rs:194` 已有一个不相干的 `TargetState`（Active/Archived）。
> DTO 侧按 §3.3 用 `RelationTargetStateV2`，别复用。
>
> 读路径开销：单词条读取在**有关联时**多两次查询（§2.4 的放宽解析 + §2.2 的引用行），
> 都按主键/最左前缀走。**列表接口不受影响**——`AdminWordListItem`（`dto/operations.rs:857-882`）
> 根本不带 meanings/relations，不会出现 N+1。

### 并入包 3（发布规则 · 显式 B）

| 项 | 位置 |
| --- | --- |
| `pending_relation_refs` + 短路前置判定（§3.2） | `service/publishing.rs:155` |
| `validate` 增报 `relation_ref_not_in_publication`（§3.3） | `service/publishing.rs:33-41` |
| 新 issue code 写进 `docs/frontend-integration.md` 的错误码表 | —— |

工作量增量：判据这一层约 **+0.5 人日**（一条 SQL + 一个纯函数 + 用例），
短路那一层约 **+0.5 人日**。相对 `backend-assessment.md` §6 的估算是小增量，
**但它把包 1 与包 3 绑成了「必须同批上线」**——包 1 单独上线时 `target_state` 会出现
`resolvable`，而那时点提交生效仍是静默 201，标记指向一个走不通的动作。

---

## 5. 测试清单增补（接 `backend-assessment.md` §7）

- **本条缺口的直接回归**：S 保存关联指向 T 的草稿词义 → S 发布（显式 B，关联不进引用表）→
  T 以**与 S 抄下的草稿值逐字相同**的词头/释义发布 → 读 S：
  `target_state` 必须是 `"resolvable"`，**不得**是 `"resolved"`
- 承上：此时 `POST /validate` 必须报 `relation_ref_not_in_publication`，且 `valid = false`
- 承上：此时 `POST /publish` 必须 **422**，**不得**是 201；断言没有新的 `entry_publications` 行
- 承上：重存词义步（revision +1）→ 再发布 → 201，且 `entry_publication_sense_refs`
  出现该 `(publication_id, source_node_id, target_entry_id, target_sense_id)` 行 → 再读 S 为 `"resolved"`
- **门条件（防噪音）**：S 已发布后新加一条指向**已发布**目标的关联并保存（有未发布改动）→
  `target_state` 必须是 `"resolved"`，validate 不得报这条 issue
- **门条件（防噪音）**：S **从未发布**，关联指向已发布目标且字符串一致 → `"resolved"`
- **既有那一半也要覆盖**：S 已发布且关联已进引用表 → T 改释义重新发布 → 不重存直接 publish
  必须 **422 `relation_target_stale`**（**这是对今天静默 201 的行为变更，要单独写一条**）
- **不要比锚定发布版本**：T 以**完全相同**的词头/释义再发布一次（引用行仍锚在旧版）→
  S 必须仍是 `"resolved"`，validate 与 publish 都不得报错
- **`target_state` 不落库**：保存/发布之后直接查 `entry_editor_projection.meanings` 与
  `entry_publications.snapshot`，两处 JSON 里**都不得**出现 `target_state` 键
  （客户端自填也不行——覆盖 §2.4 告警框那条注入用例）
- **回滚交互**：S 回滚到旧发布（`activate_publication`）后 `has_unpublished_changes` 通常为真 →
  关联行不得出现 `resolvable`；断言判定读的是**回滚后**的 `current_publication_id`

---

## 6. 对 `backend-assessment.md` 的三处修订

已在同 PR 里就地加了指回本文的注记，不改原文结论：

1. **§3.3** —— 「保存那一刻的快照」改口径为「读时现算」，并把枚举补成五态（§1.5）。
2. **§4 方案 D 告警框** —— 「若要让『没重存就重发』也有信号，得额外改这条短路」
   补上「而且只能改成报错，不能改成放行」的 DB 依据（§3.1）。
3. **§7** —— 「发布短路」那条用例原本断言「当前是 201 no-op」，
   改为指向 §5 的新断言集合。
