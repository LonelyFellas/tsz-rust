# 细分词性的代码文本落到「编码」，正式英文放开重复

状态：**已实施（2026-09-11）**，尚未部署。评估基线 tsz-rust `9f40e1a`、tsz `70df59b`。

## 背景

管理端「系统设置 → 词性配置 → 细分词性」里，管理员一直把 Collins 式的代码文本
（`N-COUNT`、`N-UNCOUNT`）填进「正式英文」，因为稳定编码不对他们暴露、由英文全称派生。
这条路走不通：更细的划分会正当共用同一个 Collins 编码——「不可数物质名词」和
「不可数抽象名词」的代码文本都是 `N-UNCOUNT`——而 `name_en` 在同一基本词性下忽略大小写唯一，
第二条存不进去，页面报 409「正式英文与已有细分词性重复」。

用户 2026-09-11 拍板：**代码文本改由专门的「编码」承载，正式英文降级为纯展示名**。

## 范围

只动细分词性。基本词性与词形变化的编码仍由英文全称派生、用户不填不看，`name_en` 唯一性不变。

## 需求

1. `catalog.sub_parts_of_speech.name_en` 不再唯一：同一基本词性下允许重复。
2. `code` 由管理员在管理端填写（前端改动，本仓不涉及），后端 POST 早已收 `code`，无需改。
3. `PATCH /admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}` 新增**可选** `code`：
   - 缺省表示不改，旧前端不受影响；
   - 与现值不同且该细分词性已被词义引用时返回 409 `sub_part_of_speech_in_use`，
     带 `meta.usage_count`；
   - 编码撞车仍是 409 `sub_part_of_speech_conflict`，顶层 `field` 为 `code`；
   - 并发版本优先：`base_revision` 过期时先返回 409 `revision_conflict`，
     不能被引用守卫盖成「已被引用」。

## 约束

- 编码是词条引用的口径，且发布快照里存的是编码文本（`entry_publications.snapshot` 里的
  `sub_pos`）。被引用后禁改是刻意从严，不是技术限制——引用本身是按 UUID 建的。
- 引用计数复用删除用的 `sub_part_usage_count`（活动草稿 sense + 所有历史 publication 引用去重）。
- 迁移只删索引、不改数据，`catalog.metadata.version` 不自增（目录内容没变）。
- 回退（down）会重建唯一索引：上线并产生重复正式英文后就退不回去了。down 里加了守卫，
  失败时报出重复的 `name_en` 而不是裸 23505，避免整串 undo 被一条看不懂的冲突打回。

## 发布顺序

**后端先**。`code` 在 PATCH 里是可选的，旧前端不发也不会 422，所以不必同批；
前端放出编码输入必须等后端上线之后，否则改编码的请求会被 `deny_unknown_fields` 打成 422。

## 验收

```bash
cargo test --locked --all-features --test catalog_handler sub_part_code_carries_the_code_text_and_freezes_once_referenced
```

配套用例：

- `cargo test --locked --all-features --test catalog_schema sub_part_name_en_may_repeat_under_the_same_parent`
- `cargo test --locked --all-features --lib deployment_migrations::tests::deployment_undo_reports_duplicate_sub_pos_name_en`
