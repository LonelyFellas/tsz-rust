# 多维例句发布时自动关联：后端契约方案

> **来源**：2026-08-23 用户口头定稿的产品规则，以及前端 `tsz` 仓库
> `docs/features/example-linking-auto-on-publish/{requirements,design}.md`
> （分支 `feat/example-linking-auto-on-publish` @ `3da99cd`）。
>
> **核对基线**：`tsz-rust` `main` @ `bb17a9b`（2026-08-23）。下文「现状」每一条都读过源码，
> 行号即该 commit 的行号。
>
> **状态**：**已落地**（2026-08-24）。契约经确认后按 §10 实现，下文即实现后的形状；
> 相对初稿的三处调整已就地标注（§2.2 可空字段的序列化、§6.1 整组替换的范围、§6.2 多一个错误码）。
>
> **一句话**：前端建议的 wire 形状基本可用，但有三处必须改——偏移量要说清是**码点**、
> 位置要绑**方言侧**而不是整条例句、关联**不能存进草稿内容**（每次保存词义步都会把内容表整表重写，
> 存进去就会被自己人擦掉）。口径两条都按「宁缺勿错」拍板：只关实词、有歧义就跳过。

---

## 0. 摘要：五个问题的答复

| # | 问题 | 结论 |
| --- | --- | --- |
| 1 | 例句关联怎么进 wire | `WordSentenceV2` 加 **`associations` + `associations_state`** 两个只读字段。`links` 原样保留给归属（`focus`）与存量 `context`。形状与前端建议差 4 处，逐条见 §2.3 |
| 2 | 发布时自动关联挂哪里 | `publishing.rs:353` 的 `insert_publication` 之前的最后一步。**解析失败只跳过该词，发布照常成功**；整段包在同一事务里，发布失败则关联一起回滚 |
| 3 | 筛选口径 | **实词 + 已发布词形 + 停用词表**三重过滤：实词集合独立固定为 `noun/verb/adjective/adverb`，不从词形类型能力推断；只认目标词条**当前发布版本**里真实存在的词形，再扣掉 be/have/do/情态动词等纯语法词。详见 §3 |
| 4 | 歧义策略 | **跳过**。词形→多词条、多词性、多词义，任何一层不唯一都不建关联。理由与代价见 §4 |
| 5 | 正文改了何时重解析 | **下次发布时**。已发布词条改例句正文走的就是普通草稿保存（`PUT steps/meanings`），改完当前发布版本还是旧正文，重解析只能跟着发布走。中间态由 `associations_state = "unresolved"` 显式告诉前端。详见 §7 |

**另有两条前端验收标准本契约不覆盖**，见 §9.1（AC-13/AC-14 例句复用）——今天一条例句在库里
只属于一个词义，复用要改的是例句本身的归属模型，不在本次范围内。**这条必须让前端知道**。

---

## 1. 现状核对

### 1.1 例句今天长什么样

wire（`dto/aggregate.rs:360-374`）：

```rust
pub struct WordSentenceV2 {
    pub id: Uuid,
    pub level: String,
    pub en_text: EnglishTextV2,      // Unified{common} 或 Distinguish{uk, us}
    pub zh_text_id: Uuid,
    pub zh_text: RichText,
    pub links: Vec<WordSentenceLinkV2>,  // { word_id, sense_id, role: "focus"|"context" }
}
```

库层（`migrations/20260811120000_create_lexicon_meanings.up.sql`）：

- `lexicon.sentences (id, entry_id, sense_id, level, sort_order)`——**例句挂在唯一一个词义下**。
- `lexicon.sentence_links` 主键 `(sentence_id, target_entry_id, target_sense_id)`，
  `role IN ('focus','context')`，唯一索引 `lexicon_sentence_links_one_focus_idx` 保证一句一个 focus。
- 正文落在 `lexicon.text_variants`，按 `(owner_node_id, field_role, language, dialect)` 唯一，
  即**一条例句的 en_text 最多有 common / uk / us 三个槽位中的一组**（unified 用 common，
  distinguish 用 uk+us）。

### 1.2 已有的跨词条引用机制

`role = "context"` 的链接今天就是「这句话里提到了别的词条的某个词义」：

- 发布时 `resolve_meaning_references`（`publishing.rs:1372`）把外部 context 收集成
  `entry_publication_sense_refs (reference_kind = 'sentence_context')`，并要求目标是
  **目标词条当前发布版本里的有效词义**，否则 `sentence_context_target_unavailable` 拦下发布。
- 这些引用会反过来**卡住目标词条**：`current_inbound_sense_refs`（`publications.rs:688`）
  让目标词条无法发布一个删掉了被引用词义的版本（`sense_has_inbound_publication_refs`）。

**本次自动关联刻意不走这套**，理由见 §8.3。

### 1.3 可以直接复用的词面索引

`lexicon.surface_sources`（`migrations/20260815100000`）已经把每个词条的**每个词形拼写**投影成
可查的行，且区分 `content_scope = 'draft' | 'current_publication'`：

| 列 | 自动关联怎么用 |
| --- | --- |
| `normalized_surface` + `dialect_scope` | 按 `normalize_headword` 归一化后的词面查，uk/us 两套口径 |
| `content_scope = 'current_publication'` | **只认已发布内容**，草稿天然不进候选 |
| `entry_id` / `entry_kind` | 落到词条；`entry_kind='word'` 过滤掉短语（一期不做短语匹配） |
| `source_kind = 'form'` / `source_node_id` | 命中的是哪个 `form_variant` 节点 |
| `pos_id` / `pos` / `form_type` | 词性与形态类型，**筛选口径与只读投影都靠它** |

⚠️ `source_kind='headword'` 的行 `pos/pos_id/form_type` 全为 NULL
（`lexicon_surface_sources_source_shape_check`），所以解析器**只查 `source_kind='form'`**。
发布必过 `validate_forms`，每个词性都有基本形，词头拼写一定同时以 form 行存在，不会因此漏。

### 1.4 四条决定设计形状的硬事实

**① 草稿内容每次保存都整表重写。** `replace_entry_content`（`entries.rs:495-514`）先
`delete_current_content`（`entries.rs:863`，逐表 `DELETE FROM lexicon.sentences / sentence_links / ...`）
再按请求体重插。**任何存进草稿内容表的关联，都会被下一次 `PUT steps/meanings` 删掉**——
而按产品规则前端保存时根本不带关联字段。
⇒ 关联必须存在**独立于内容重写的表**里，锚点用跨保存稳定的 `lexicon.nodes` 节点 ID。

**② `sentence_links` 装不下位置。** 主键就是 `(sentence_id, target_entry_id, target_sense_id)`，
同一句里同一个词出现两次（AC-05）根本插不进第二行。
⇒ 必须是带自己主键的新表，不能扩 `sentence_links`。

**③ RichText 的偏移量单位是 Unicode 码点。** `canonicalize_v1`（`rich_text/core.rs:72`）与
`validate_v2`（`rich_text/core.rs:182`）都用 `value.text.chars().count()` 校验 span/liaison 区间。
⇒ `source_range` 沿用同一口径：**`RichText.text` 的码点下标，左闭右开**。不是字节、不是 UTF-16。
前端对 spans/liaisons 已经在按这个口径处理，不需要新约定。

**④ `publish` 有两条早返回路径不产生新 publication**（`publishing.rs:174` 与 `publishing.rs:286`）：
同 revision 重复发布、以及回滚后重新发布命中历史 publication。这两条路径上例句正文与上次发布
逐字相同，解析结果不会变，**解析器不挂在这两条路径上**（挂了也是空转）。

---

## 2. 契约：wire 形状

### 2.1 `WordSentenceV2` 新增两个只读字段

```rust
pub struct WordSentenceV2 {
    // ... 既有字段原样不动 ...
    pub links: Vec<WordSentenceLinkV2>,

    /// 只读。发布时由后端解析产出，草稿保存不接受写入（收到也会被丢弃）。
    #[serde(default)]
    pub associations: Vec<WordSentenceAssociationV2>,
    /// 只读。`associations` 是不是当前正文的解析结果。
    #[serde(default)]
    pub associations_state: SentenceAssociationsStateV2,
}

#[derive(Default)]
#[serde(rename_all = "snake_case")]
pub enum SentenceAssociationsStateV2 {
    /// 当前正文还没被解析过：草稿从未发布，或正文改动后尚未重新发布。
    /// 此时 `associations` 恒为空数组，**不代表「这句话没有可关联的词」**。
    #[default]
    Unresolved,
    /// `associations` 就是当前正文的解析结果（可能为空数组，即确实没词可关联）。
    Resolved,
}
```

两个字段都带 `#[serde(default)]`：存量 `entry_editor_projection.meanings` JSONB 里没有它们，
反序列化落到 `Unresolved` + 空数组，与事实一致（存量例句确实没解析过）。
但契约上仍标成必填（`#[schema(required = true)]`）——服务端返回时一定带，
前端不该为一个永远存在的字段写 optional 分支。

`WordSentenceV2` 本身没有 `deny_unknown_fields`，前端把读到的字段原样回传不会 400；
服务端在保存路径上**显式清空**这两个字段后再落库，避免客户端自填只读投影一路进发布快照
（`draft-relation-targets/backend-assessment.md` §2.4 踩过这个坑）。

### 2.2 `WordSentenceAssociationV2`

```jsonc
{
  "id": "0198f2c1-...",              // 关联自身 ID，前端生成、跨保存稳定
  "source_dialect": "common",        // common | uk | us —— 位置落在 en_text 的哪一侧
  "source_range": {                  // RichText.text 的码点下标，左闭右开
    "start": 11,
    "end": 18,
    "surface": "picture"             // 原句里的实际词面，等于 text[start..end]
  },
  "target_word_id": "0198f2c2-...",
  "target_sense_id": "0198f2c3-...",
  "target_form_slot_id": "0198f2c4-...",  // 解析不出时**整个字段缺省**，见下
  "origin": "auto",                  // auto | manual
  // 以下四个是写入时落的快照，只读
  "target_headword": "picture",
  "target_gloss": "图片",
  "resolved_pos": "noun",
  "resolved_form_type": "base"       // 与 target_form_slot_id 同生共死，同样缺省而非 null
}
```

字段说明：

- **`source_dialect`**：一条例句 distinguish 模式下有 uk / us 两份正文，`colour` / `color`
  会让同一个词的下标不同。位置**必须**绑到具体一侧。前端按用户方言偏好渲染哪一侧，就取哪一侧的关联。
  unified 模式下只有 `common` 一组。
- **`source_range`**：`surface` 冗余存一份，既是自检（读取时校验 `text[start..end] == surface`），
  也让前端不必为了展示再切一次字符串。
- **`target_form_slot_id` / `resolved_form_type`**：命中的是目标词条的哪个**词形槽位**
  （`lexicon.form_slots`，如「复数」「过去式」），而不是某个方言拼写。这是「例句里的词按读者方言
  显示成 `centre` 还是 `center`」的锚点。**两个字段整个缺省**（沿用本仓 `skip_serializing_if`
  的既有写法，不发 `null`）有两种情形，前端一律回落到直接显示 `surface`：人工补的关联词面在
  目标词条的已发布词形里找不到（管理员知道得比词库多）；以及同一词性下多个槽位共用一个拼写
  （不规则动词 `cut` 的原形、过去式、过去分词都是 `cut`）——词义仍唯一所以关联照建，
  但槽位是哪一个没有证据，按变体 ID 挑一个等于给猜测背书。
- **`origin`**：`auto` = 发布时自动解析产出，`manual` = 管理员事后改过或补过。仅供展示与后续
  调口径时评估质量，不参与任何判定。
- **快照字段**：`target_headword` / `target_gloss` / `resolved_pos` 在写入时从目标词条**当前发布
  快照**取值后固化，读取时不做跨词条 JOIN。与 `lexicon.relations.target_*_snapshot` 同套做法。
  代价：目标改了释义，这边的 gloss 会旧，直到本词条下次重新解析。**不为此做过期校验**——
  关联词那套 `relation_target_stale` 会拦发布，而自动关联的产品语义是「不能打断发布」。

排序：按 `(source_dialect, source_range.start)` 升序返回，不存 `sort_order`
（前端建议里的 `sort_order` 因此**去掉**——位置本身就是全序）。

### 2.3 与前端建议形状的差异（4 处）

| 前端建议 | 本契约 | 为什么 |
| --- | --- | --- |
| `source_range` 单位未说明 | 明确为 **`RichText.text` 的码点下标** | 与 `RichText` 既有 spans/liaisons 同一口径（§1.4 ③），避免 JS 的 UTF-16 与 Rust 的字节各说各话 |
| 位置绑在整条例句上 | 增加 **`source_dialect`** | distinguish 例句有两份正文，下标会错位（`colour`/`color`） |
| `form_slot_id` | 改名 **`target_form_slot_id`**，且**可空** | 与其他 `target_*` 对齐；人工关联可能落在词库没有的词形上 |
| `sort_order` | **去掉** | `(dialect, start)` 已是全序 |

另新增前端没提的 `origin` 与 `associations_state`——后者是必须的：没有它，前端无法区分
「这句没有可关联的词」与「正文改过、等下次发布重解析」，两种情况都是空数组。

---

## 3. 口径一：哪些词参与自动关联

按顺序过三道闸，全过才建关联。**前端不做二次筛选。**

### 3.1 切词

在 `RichText.text` 上按「拉丁字母 + 词内的 `'` / `-`」切出候选片段，记录码点区间。
片段两端的 `'`、`-` 剥掉（`"picture,"` → `picture`，`don't` 整体保留，`well-known` 整体保留）。
再用 `normalization::normalize_headword`（只读口径，不做字符集校验）归一化成 key。

一期**只切单词，不做多词短语匹配**（前端 requirements 已明确「短语内部成分展开」不做）。
`entry_kind = 'phrase'` 的词条因此永远不会成为自动关联的目标。

### 3.2 停用词表（拦纯语法词）

命中下表直接跳过，早于任何数据库查询：

```
be 系：be am is are was were been being
have 系：have has had having
do 系：do does did done doing
情态：will would shall should can could may might must ought
否定：not
```

**为什么需要它**：只靠词性挡不住——`is` 在词库里是 `be` 的第三人称单数形式，词性是 verb，
不列进停用词就会被关联上，而 AC-07 明确点名 `is` 不该关联。`the` / `a` / `on` 这类由
§3.3 的词性闸拦下，不必进表。

表是**代码里的常量**（放 `lexicon/sentence_association/mod.rs`），改口径 = 改常量 + 升
`RESOLVER_VERSION`（见 §8.2），不做成配置表——现在没有按环境调它的需求。

### 3.3 词性闸：只关实词

候选行必须满足 `surface_sources.pos ∈ {noun, verb, adjective, adverb}`。

这四类等价于 `form_types::allowed_form_types(pos)` 非空的那四类——**词库里「有形态表」的词性
就是实词**，这个划分本仓已经存在（`form_types.rs:11-19`），不新造一套。
`article` / `determiner` / `preposition` / `pronoun` / `conjunction` / `numeral` / `interjection`
（`service/helpers.rs:72-84` 的完整词性表）全部出局，`the` / `a` / `on` / `it` 因此不产生关联。

词性目录 `catalog.parts_of_speech` 是管理员可配的，出现自定义词性时 `allowed_form_types` 返回空
⇒ **fail-closed，不关联**。

### 3.4 目标闸：只认已发布词形

查询条件（一次批量查 `lexicon.surface_sources`）：

```
content_scope = 'current_publication'   -- 只认已发布，草稿不进候选
AND is_deleted = FALSE
AND language = 'en' AND entry_kind = 'word'
AND source_kind = 'form'                -- headword 行没有词性，见 §1.3
AND normalization_version = HEADWORD_NORMALIZATION_VERSION
AND normalized_surface = ANY($tokens)
AND dialect_scope = <本侧正文的方言口径>  -- uk 侧查 'uk'，us 侧查 'us'，common 两侧都查
AND entry_id <> <当前词条>               -- 不关联自己
```

排除自身词条是刻意的：例句本来就挂在自己的词义下（`focus` 归属），句中的主词再指回自己是噪音。

---

## 4. 口径二：歧义一律跳过

一个候选词面要建关联，必须在**三层上同时唯一**：

1. **唯一词条**：命中多个 `entry_id` 就跳过。真实存在——`left` 是 `left` 的基本形，也是 `leave`
   的过去式；`saw` 同理。
2. **唯一词性**：同一词条里命中多个 `entry_pos`（`picture` 既是名词也是动词）就跳过。
3. **唯一词义**：该 `entry_pos` 下当前发布版本里只有一个 sense 才绑；多义就跳过。

**拍板理由**：错误关联比缺失关联难被发现——缺失是空白，管理员看一眼就知道要补；错关到别的义项
在界面上和正确关联长得一模一样，只有懂那个词的人逐条核对才查得出来，而这正是自动化最该避免制造的
那种债。库里现有的排序信号（`senses.sort_order` / `frequency` / `level`）都不是「这句话里用的是
哪个义项」的证据，拿它们择一等于用一个看起来有理的数字给猜测背书。

**代价要说在前面**：常用词往往多义，一期的实际关联覆盖率会明显低于「句中所有实词」。
`Center the picture on the wall.` 里 `picture`、`wall` 能不能关联上，取决于这两个词条各自的名词
词性下是不是只有一个已发布词义。**这是产品可以接受、也应该预期的结果**——补齐靠 §6 的事后编辑。

**歧义不上报**。不在 wire 里回「这个词有歧义所以跳过了」的清单：那等于把「预关联」换个名字请回来，
而它刚被砍掉。管理员在事后编辑界面里选一段文字加关联即可，目标搜索复用现成的
`GET /entries/related-search`（`repository/query.rs:55-58` 硬 JOIN 当前 publication，
**结构上只返回已发布词条**，正好是自动关联的候选口径）。

---

## 5. 发布时自动关联

### 5.1 挂载点

`service/publishing.rs` 的 `publish`，位置在「inbound 引用检查通过之后、`insert_publication`
之前」（现 `publishing.rs:353` 之前），即 §1.4 ④ 说的**只在真的要产出新 publication 的那条路径上**。

与关联词物化（`resolve_pending_relation_targets`，`publishing.rs:218`）同一个事务，**失败语义相反**：

| | 关联词待物化 | 例句自动关联 |
| --- | --- | --- |
| 解析不出目标 | `ValidationFailed` → 整个发布回滚 | **跳过该词，发布照常 201** |
| 会不会建新词条 | 会（建占位词条） | **不会，一个词条都不建** |
| 目标可以是草稿吗 | 可以 | **不可以，只认当前发布版本** |
| 出错时 | 管理员必须去补目标词条 | 管理员什么都不用做 |

解析器内部的任何「查不到 / 有歧义 / 词面非法」都不是错误，只是「这个词不关联」。
真正的错误只剩数据库故障与不变量破坏，那种情况本来就该让整个事务回滚。

### 5.2 每次发布做什么

对本次发布内容里的每条例句、每个存在的 en_text 方言侧：

1. 算 `text_hash = sha256(RichText.text)`。
2. 与 `lexicon.sentence_association_scans` 里记的 hash 比：
   - **相同**（且 `resolver_version` 未变）→ **什么都不做**。已有关联原样保留，包括管理员事后改过的。
   - **不同 / 没记录** → 删掉这一侧的旧关联，重新解析，插入新结果，写回 scan 记录。
3. 收尾：删掉本词条下「例句已从草稿里删除」或「方言侧已不存在」的关联与 scan 记录。

这条 hash 规则同时兑现了四件事：

- **AC-08 幂等**：同 `Idempotency-Key` 重放直接命中 `platform.idempotency_records` 返回原响应；
  即便绕过幂等键再发一次，正文没变就不动关联，不会产生重复。
- **AC-15 正文改了重新解析**：hash 变了就整侧重算。
- **AC-10~12 的修正不会被下次发布抹掉**：正文没变就不重算。
- **口径升级可控**：`RESOLVER_VERSION` 一升，所有例句在各自下次发布时自然重算。

**已知代价（要写进产品预期）**：`picture` 先发布、`wall` 后发布的话，`picture` 那条例句里的
`wall` 不会被回填——除非 `picture` 的例句正文改动后重新发布。这正是「反向认领」被砍掉之后的
必然结果，不是 bug。真要回填，将来加一个显式的「重新解析」动作即可（不在本期）。

### 5.3 一次发布的开销

一条例句 ~10 个候选词 → 一次批量 `surface_sources` 查询（走
`lexicon_surface_sources_lookup_idx`）→ 命中的候选词条批量取一次当前发布快照
（`entry_publications.snapshot`，用来定位 sense、form slot 与 gloss）。
一个词条几十条例句、去重后几十个目标词条，量级与发布本身已有的引用解析相当，不额外加压。

---

## 6. 事后编辑端点

### 6.1 一个 PUT 覆盖三种操作

前端列的「改目标 / 删关联 / 补关联」合并成**按例句整组替换**，与本仓
`PUT /steps/forms`、`PUT /steps/meanings` 的整步替换风格一致：

```
PUT /api/v1/admin/lexicon/entries/{id}/sentences/{sentence_id}/associations
Idempotency-Key: <uuid>
```

> **状态（2026-09-05）**：该事后编辑端点连同 `pending-sentence-associations` 列表/认领端点已下线，
> 草稿期不再人工编辑关联；例句创建流程将重新设计。发布时的自动关联不受影响。

```jsonc
{
  "base_revision": 12,
  "base_lifecycle_revision": 3,
  "associations": [
    {
      "id": "0198f2c1-...",
      "source_dialect": "common",
      "source_range": { "start": 11, "end": 18, "surface": "picture" },
      "target_word_id": "0198f2c2-...",
      "target_sense_id": "0198f2c3-..."
    }
  ]
}
```

- **列表就是这条例句关联的完整目标状态**，覆盖它所有存在的方言侧：改目标 = 改列表里那一项的
  `target_sense_id`；删 = 从列表里去掉；补 = 加一项；空列表 = 清空这条例句的全部关联。
- `target_form_slot_id` / 四个快照字段**不接受输入**，一律服务端解析后落值。
- `id` 由前端生成（沿用本仓「节点 ID 前端生成、跨保存稳定」的约定）；
  与库里同 `id` 且 target 与 range 都没变的行保留 `origin`，其余一律置 `manual`。
- 响应 `AdminWordV2Envelope`（整个词条，`associations` 已回填），前端一次刷新到位。

### 6.2 前置条件与错误码（RFC9457，沿用 `docs/features/rfc9457-error-response/`）

| 情况 | 状态码 | `code` |
| --- | --- | --- |
| 词条不存在 / 例句不属于该词条 | 404 | `word_not_found` / **新增** `sentence_not_found` |
| `base_revision` 或 `base_lifecycle_revision` 落后 | 409 | `revision_conflict`（`meta.current_revision` / `meta.current_lifecycle_revision`） |
| 该例句当前正文**尚未解析过**（`associations_state = "unresolved"`） | 409 | **新增** `sentence_associations_unresolved` |
| 词条已归档 | 409 | `entry_archived` |
| 幂等键复用但请求体不同 | 409 | `idempotency_conflict` |
| 区间越界或首尾带空白或超过 200 码点 / `surface` 与正文对不上 / 区间重叠 / 指了这条例句没有的方言侧 / 目标不是已发布词义 / 目标是本词条自己 / `id` 重复 | 422 | `validation_failed`，逐条 `DraftValidationIssue`（`node_id` 是例句节点，`field` 是 `associations`）：`sentence_association_range_invalid` / `..._surface_mismatch` / `..._range_overlap` / `..._dialect_unavailable` / `..._target_unavailable` / `..._self_target` / `..._duplicate_id`。一次请求把所有问题一起报出来，不是首错即停 |

**为什么要 `sentence_associations_unresolved` 这道闸**：草稿从未发布、或正文改了还没重新发布时，
库里的区间对不上当前正文，此时允许编辑只会写进一批下次发布就被冲掉的数据。挡在门口比事后解释便宜。

### 6.3 乐观锁：为什么用 `lifecycle_revision` 而不是 `revision`

`revision` 是**内容修订号**，一改 `has_unpublished_changes` 就变 true，词条会被判定为「有未发布改动」，
这与「事后修正关联」的语义正相反——修正的就是已发布内容的附属数据，不该逼出一次重新发布。
`lifecycle_revision` 本来就是「非内容变更」的计数器（归档/恢复/切换历史版本都在动它），
关联编辑归到这里最贴切。请求同时校验两个 revision、成功后只推进 `lifecycle_revision`。

---

## 7. 正文修改后何时重新解析（前端开放问题 4 的答复）

**先答既有语义**：已发布词条改例句正文，走的就是普通的 `PUT /entries/{id}/steps/meanings`——
`revision + 1`、`has_unpublished_changes = true`，**当前发布版本仍然是旧正文**，
必须再发布一次改动才对外生效（`service/editing.rs:611` 起的保存链路 + `publish` 的 revision 判定）。

**所以答案只能是：下次发布时重解析。** 立刻重解析没有意义——当前发布版本里的正文根本没变，
按新正文算出来的区间是给一份还没发布的文本用的。

中间态对前端是**可见的**：保存后 `associations_state` 从 `resolved` 掉成 `unresolved`、
`associations` 变空数组。前端据此提示「正文已修改，关联将在重新发布后重新解析」，
而不是让管理员以为关联被删了。前端 AC-15 要的「展示受影响引用数并要求确认」照旧由前端在保存前完成，
后端不为此加接口。

---

## 8. 存储设计

### 8.1 两张新表

```sql
-- 关联本体。刻意不进 lexicon.sentences 的子表体系：内容表每次保存整表重写（§1.4 ①），
-- 挂进去就会被下一次词义步保存删掉。锚点用 lexicon.nodes，节点 ID 跨保存稳定。
CREATE TABLE lexicon.sentence_associations (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    sentence_id UUID NOT NULL,
    source_dialect TEXT NOT NULL CHECK (source_dialect IN ('common', 'uk', 'us')),
    range_start INTEGER NOT NULL CHECK (range_start >= 0),
    range_end INTEGER NOT NULL CHECK (range_end > range_start),
    surface TEXT NOT NULL,
    target_entry_id UUID NOT NULL,
    target_sense_id UUID NOT NULL,
    target_form_slot_id UUID,
    origin TEXT NOT NULL CHECK (origin IN ('auto', 'manual')),
    -- 上限 500 而不是 400：distinguish 词条的词头快照是两侧拼起来的 `uk / us`，
    -- 每侧 200 码点，加分隔符最长 403，卡在 400 会让极端词头把发布顶成 500。
    target_headword_snapshot TEXT NOT NULL,
    target_gloss_snapshot TEXT NOT NULL,
    resolved_pos TEXT NOT NULL,
    resolved_form_type TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- 例句节点没了（整条词条被删）就跟着走
    FOREIGN KEY (sentence_id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    -- 目标必须是真实节点，与 sentence_links 同一把尺子
    FOREIGN KEY (target_sense_id, target_entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    FOREIGN KEY (target_form_slot_id, target_entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CHECK (entry_id <> target_entry_id),
    CHECK ((target_form_slot_id IS NULL) = (resolved_form_type IS NULL)),
    UNIQUE (sentence_id, source_dialect, range_start)
);

-- 「这一侧正文解析到什么版本了」。没有它就无法区分「解析过、没词可关联」与「还没解析过」，
-- 也无法在正文没变时跳过重算（那是管理员修正能活过下次发布的唯一原因）。
CREATE TABLE lexicon.sentence_association_scans (
    sentence_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    source_dialect TEXT NOT NULL CHECK (source_dialect IN ('common', 'uk', 'us')),
    text_hash BYTEA NOT NULL,
    resolver_version SMALLINT NOT NULL CHECK (resolver_version > 0),
    scanned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (sentence_id, source_dialect),
    FOREIGN KEY (sentence_id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE
);
```

`UNIQUE (sentence_id, source_dialect, range_start)` 顺带挡住并发重复插入，也表达了
「一个起始位置最多一条关联」这条业务约束。

### 8.2 `RESOLVER_VERSION`

Rust 侧常量，切词规则、停用词表、筛选口径、歧义策略任一变更都要 +1。
scan 记录里存版本，比对时版本不等即视为需要重算——口径升级不需要写数据迁移，
各词条在下次发布时自然跟上。与 `HEADWORD_NORMALIZATION_VERSION`、`SURFACE_WRITER_VERSION` 同套思路。

### 8.3 为什么不登记 `entry_publication_sense_refs`

自动关联**不**写 `entry_publication_sense_refs`（§1.2 那套跨词条引用台账）。

登记的话，A 词条发布时自动产生的关联会**卡住 B 词条**：B 之后想发布一个删掉该词义的版本，
会被 `sense_has_inbound_publication_refs` 拦下，而 B 的管理员既没做过任何选择、也无从知道
这个引用是哪来的。手工 `context` 链接卡人是合理的（那是管理员显式录入的意图），
自动产物卡人不合理——它违背「自动关联是发布的附带产物，不打断任何人」的产品定位。

代价是关联可能悬空：目标词条重新发布时删掉了那个词义，关联仍指着一个已不在当前发布版本里的
sense 节点（节点行本身不会消失，`lexicon.nodes` 只标 `removed_from_draft_at`，外键仍成立）。
读取时快照字段照常返回，前端点过去可能落空。**一期接受**：本词条下次重新解析会清掉它，
管理员也能手工删。真要收敛，后续加一个「悬空关联巡检」比反过来卡住所有人便宜得多。

### 8.4 读取路径

`associations` **不进** `entry_editor_projection.meanings` JSONB，也**不进** publication 快照——
只有一份真相在表里，读取时按 `entry_id` 一次查出、按例句归位：

- 每条例句的每个方言侧，比对当前正文 hash 与 scan 记录：一致 → 挂上该侧关联；
  不一致或无记录 → 该侧无关联。整条例句所有存在的侧都一致才置 `associations_state = "resolved"`。
- 挂载点：`get`、`save_forms` / `save_meanings`、`publish` 的三条返回路径、
  `activate_publication` 的两条返回路径，以及新的关联编辑端点。
  **必须在写幂等响应体之前回填**，否则重放返回的响应会缺字段。
- `create` 不回填：新建草稿必然是 `unresolved` + 空数组，默认值就是对的。
- 归档/恢复（批量命令）不回填：它们不碰例句，响应也不用于渲染例句编辑区，
  为此给批量里的每个词条各加两次查询不值得。

---

## 9. 明确不做

### 9.1 前端 AC-13 / AC-14「例句复用」本契约不覆盖 ⚠️

前端 requirements 的功能范围第 4 条与 AC-13/AC-14 要求「同一条例句被多个词义引用，正文只存一份」。
**今天的库层做不到**：`lexicon.sentences.sense_id` 是 NOT NULL 的单值外键，一条例句只属于一个词义；
`lexicon_sentence_links_one_focus_idx` 又保证一句只有一个 focus。要支持复用，得把例句从
「词义的子节点」抬成独立聚合，涉及内容表、发布快照、编辑器投影与整个词义步的保存契约——
是与本功能同量级的另一件事。

**本契约只做「发布时自动关联 + 事后修正」**，AC-13/AC-14 需要单独立项。请前端据此调整验收范围。

### 9.2 其余不做项

- 不做预关联、反向认领、`pending/reserved/linked` 生命周期（已作废）。
- 不做短语（多词）匹配，不做词形推断——只认词库里真实录入的词形（`surface.rs:17-21` 的既有原则）。
- 不做歧义候选清单接口（§4）。
- 不做「目标词条后发布时回填历史例句」（§5.2）。
- 不做悬空关联巡检（§8.3）。
- 不动 `links`：`focus` 归属与存量 `context` 原样保留，读写行为不变（AC-16/AC-17）。
- 不做学习端消费。

---

## 10. 落地情况

| 包 | 落在哪 |
| --- | --- |
| **1. 库层与 wire** | `migrations/20260824100000_create_lexicon_sentence_associations.*`；`dto/aggregate.rs` 的 `WordSentenceAssociationV2` / `SentenceSourceRangeV1` / 两个枚举 / `WordSentenceV2` 的两个新字段；`repository/sentence_associations.rs` |
| **2. 解析器** | `lexicon/sentence_association.rs`（切词、停用词、词性闸、正文指纹、码点切片，纯函数带单测）+ `service/sentence_association.rs`（候选查询、三层歧义判定、快照回填） |
| **3. 发布挂钩** | `service/publishing.rs` 的 `publish` 主路径；`repository/publications.rs` 里发布快照剥掉只读投影 |
| **4. 事后编辑端点**（已下线，2026-09-05） | `PUT /entries/{id}/sentences/{sentence_id}/associations`：`dto/operations.rs` 输入、`service/sentence_association.rs` 服务、`handler/commands.rs` + `router.rs`、`error.rs` 两个新错误码 |

### 测试

单测（`lexicon/sentence_association.rs`，9 个）：停用词表有序且只收词性闸拦不住的虚词、
只有带形态表的词性参与关联、切词的码点区间能原样切回词面、同一拼写两次给出两个位置、
词内撇号与连字符保留而标点断词、组合附加符号不断词、码点切片拒绝越界与空区间、词面可落库判据与列约束对齐、正文指纹逐字敏感。

集成测试（`tests/lexicon_handler.rs`，11 个）：

| 测试 | 覆盖 |
| --- | --- |
| `publishing_resolves_sentence_words_to_the_single_published_sense` | AC-01~04：草稿期零关联、发布后 `wall` 关联到唯一词义、区间/词形槽位/快照字段正确、GET 读回一致、发布快照不带只读投影 |
| `sentence_associations_are_position_wise_and_publish_survives_every_skip` | AC-05~07：同词两次两条位置、库外词与虚词跳过、发布仍 201 |
| `ambiguous_targets_are_skipped_without_failing_the_publish` | §4 词义层歧义 |
| `one_surface_owned_by_two_entries_is_left_unlinked` | §4 词条层歧义（`walls` 同时属于两个已发布词条） |
| `distinguish_sentences_anchor_associations_to_each_dialect_side` | §2.1：uk/us 两侧各自解析，`colour`/`color` 的区间互不串 |
| `published_sentence_associations_can_be_retargeted_removed_and_added` | AC-09~12 + 乐观锁 409 + 幂等重放 + 关联修正不推进 `revision` |
| `changing_sentence_text_defers_reparsing_to_the_next_publish` | AC-08/AC-15 + §7：正文没变的重新发布保住人工修正、正文改了转 `unresolved` 且拒绝编辑、重新发布后按新正文重算 |
| `sentence_association_edits_reject_bad_ranges_and_unavailable_targets` | §6.2 全部错误码 + 校验失败不落行 |
| `manual_association_input_that_the_column_would_reject_is_a_422` | 首尾带空白的区间挡在服务层，不漏成 500 |
| `a_surface_shared_by_several_slots_of_one_pos_links_without_guessing_the_slot` | 同词性多槽位同拼写时关联照建、槽位留空 |
| `dropping_one_dialect_side_stops_serving_that_side_s_associations` | 某一侧改回 missing 后不再返回那一侧的历史关联 |

AC-13/AC-14（例句复用）见 §9.1，不在本次范围内。

---

## 11. 上线顺序与回滚

- **后端必须先上线并部署到测试服**。`PublishAdminWordV2Input` 带 `deny_unknown_fields`
  （`dto/operations.rs:785-793`），前端抢跑发新字段会 400。前端 PR 3 等测试服就绪后再合。
- 契约定稿后跑 `cargo run --bin export_openapi` 同步 `docs/openapi.json`，
  前端以 `pnpm --filter @tsz/api-client sync:openapi` 对账。
- 回滚：两张新表是纯增量，`associations` / `associations_state` 带 `#[serde(default)]`，
  回退后端版本不会让存量数据读不出来；两张表留着即可，下次上线继续用。
