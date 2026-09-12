# 设计：例句译文带上语言

需求见同目录 `requirements.md`。档位：重档（契约变更 + 需要前端配套发布）。

## 1. 不新开迁移

`lexicon.text_variants` 的 `language` 列早就存在，译文行现在固定写 `zh`；
`migrations/20260811120000_create_lexicon_meanings.up.sql:109` 的
`lexicon_text_variants_language_check CHECK (language IN ('en','zh'))` 已经容得下 `zh`。
本次只开放汉语一种，列、约束、索引都不动，因此 `deployment_migrations.rs` 的
`CURRENT_RELEASE_VERSION` / `PREVIOUS_RELEASE_VERSION` 也不动。

**将来真的引入第三种语言时，第一步是新开迁移放宽这条 CHECK。** 这是本功能最容易漏的硬门：
wire 侧加一个枚举值不会报错，直到写库那一刻才会撞 23514。

`field_role` 维持 `zh_translation_<band>` 不改名。语言虽然编码在这个字符串里，但它被
`lexicon_text_variants_field_role_check` 的字面量枚举、`lexicon_text_variants_slot_key`
的部分唯一索引、`node_identity.rs:38` 的前缀判定、`projections.rs:411` 的 `stable_slot`
判定、`deployment_migrations.rs:184-320` 的回滚守卫和 `tests/lexicon_handler.rs` 的
`LIKE 'zh_translation_%'` 一起钉死。语言由新的 `language` 字段表达，前缀留作历史常量。

## 2. DTO

`src/lexicon/dto/v3.rs` 新增语言枚举，形状照抄同文件的 `EnglishLanguageV3`：

```rust
/// 译文语言。现阶段只开放汉语，结构为多语言译文预留。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum TranslationLanguageV3 {
    #[serde(rename = "zh")]
    Zh,
}
```

`WordSentenceTranslationV3`（`:844`）加一个可缺省字段：

```rust
#[serde(default)]
#[schema(nullable = false)]
pub language: Option<TranslationLanguageV3>,
```

`Option` + `serde(default)` 让请求可以不带该字段，utoipa 因而不把它放进 `required`——
这正是前端第一步只更新契约就能通过校验的前提。`nullable = false` 保证值要么缺席要么是 `"zh"`，
不会出现 `null`。struct 上的 `#[serde(deny_unknown_fields)]` 保持不变，它才是
spec 里 `additionalProperties: false` 的来源。

注意 openapi 自检里那条 `additionalProperties` 断言（`src/openapi.rs:2033-2039`，位于
`smart_lexicon_v3_c1_contract_is_versioned_closed_and_fail_closed`）**只列了四个具名 schema**
（`AdminWordListPage` 与三个 Problem 系列），并不覆盖译文，也不存在「所有 V3 对象都封闭」的通则。
换句话说，把 `deny_unknown_fields` 摘掉不会有任何测试报警——这次要自己补断言，见第 6 节。

## 3. 服务层：读取时补默认值

译文 wire 不从 `text_variants` 投影回来，而是 `src/lexicon/service/v3.rs:778` 直接把
`lexicon.entries.meanings` JSONB 反序列化成 `AdminWordV3`，所以历史词条反序列化出来的
`language` 一律是 `None`。规范化点是现成的：`normalize_sentence_translations`
（定义在 `src/lexicon/v3_contract.rs:538`）已经在做译文的兜底，在那里把 `None` 填成
`Some(TranslationLanguageV3::Zh)` 即可。

它在 `service/v3.rs` 有三个调用点（`:780`、`:1656`、`:1930`），补在函数内部意味着三条路径
一起受益，不必逐个改调用方。

这样响应里的 `language` 恒为 `"zh"`，字段在 schema 上仍是非必填，前后端两个方向都安全。
写入方向同理：`decode_v3_meanings_request`（`src/lexicon/v3_contract.rs:69`）之后、
落库之前把缺省补成 `Zh`，让仓储层永远拿到确定值。

## 4. 仓储层：语言不再写死

`src/lexicon/repository/sentence_translations.rs:105` 的字面量 `"zh"` 改成读
`translation.language`，映射成库里的语言码后传给
`projections.rs:391` 的 `insert_text_variant`（`:428` bind 到 `text_variants.language`）。

**首档那条别名走的是另一条路径**：`sentence_translations.rs:51-69` 的 UPDATE 只改
`field_role` 和 `sort_order`，完全不碰 `language`，沿用 `zh_text` 建行时的值。
本次只开放汉语时它恰好也是 `zh`，但这是巧合不是保证，必须在这条 UPDATE 的 SET 里
补上 `language = $n`，否则将来第一条译文换语言时会静默留在 `zh`。

`text_variants.language` 是只写不读的关系投影列（全仓对该表的读只有
`query.rs:254/362/466` 的检索与存在性判断，以及 `v3_publication.rs:1104` 取 content_hash，
都与译文 wire 无关）。修它的意义是让关系侧不再撒谎，不影响读路径。

## 5. 发布快照

不改结构、不加迁移。`v3_publication.rs:977` 的 `insert_v3_publication` 把整个
`AdminWordV3` 序列化进 `entry_publications.snapshot`，新字段自动进快照 JSON。
V3→V2→V3 往返里 `restore_sentence_zh_translations`（`v3.rs:3612`）按整对象 clone
把译文复制回来，新字段跟着走，不用改。

唯一的副作用是 `snapshot_hash` 会变，且只对本次改动之后的新发布生效，旧快照原样不动。

## 6. 测试

| 用例 | 覆盖 |
| --- | --- |
| `v3_contract::sentence_translation_language_defaults_to_zh_when_absent` | 请求省略 `language` 时补成 `zh`，旧前端写入不被拒 |
| `v3_contract::sentence_translation_language_round_trips` | 请求带 `language: "zh"` 时原样保存并读回 |
| `dto::v3::translation_language_rejects_unknown_value` | `"fr"` 之类未开放的取值反序列化失败，而不是静默落库撞 CHECK |
| `openapi` 新增断言（两条） | `WordSentenceTranslationV3` 的 `additionalProperties` 为 `false`，且 `language` 不在其 `required` 里。前者补上现有自检的空白，后者锁住「前端先部署」的前提 |
| `lexicon_handler`（`:8661` / `:8811` / `:8939` 三个已直接查 `text_variants` 的用例扩断言） | 落库行的 `language` 列等于 wire 传入值；首档别名那条也不例外 |

`lexicon_handler.rs:8581` 与 `v3_contract.rs:2210` / `:2310` 是译文的既有主场用例，
改完要确认它们没被新字段带红。

## 7. 前端配套

三步部署的第 1 步与第 3 步都在 tsz 前端仓：

1. `pnpm --filter @tsz/api-client sync:openapi` 重新生成 `openapi.snapshot.json` 与
   `admin-word-v3.runtime-schema.json`，确认 `language` 进了 schema 且不在 `required`。
   这一版请求侧仍不发 `language`。
2. 后端上线后，`V3SentenceTranslationsField.tsx` 把写死的「汉语译文」标题换成语言选择器，
   当前只有汉语一项；`meaningsModel.ts` 的 `newSentenceTranslations` 与组件底部的
   「添加译文」在新建行时带上所选语言。

## 8. 风险与回退

- **前端先部署这一步不能省。** 后端先上线会让未更新 schema 的前端因
  `additionalProperties: false` 拒收整个词条响应，表现是词条编辑页整页打不开，不是局部降级。
- 回退后端只需回滚代码：没有迁移，库里 `language` 列的值本来就全是 `zh`，
  旧代码读 JSONB 时会因 `deny_unknown_fields` 拒绝带 `language` 的历史 JSONB——
  **因此回退前要确认没有新写入的词条**，或同批回退前端。这是本次最需要注意的回退约束。
- 真正开放第二种语言时，先放宽 `lexicon_text_variants_language_check`，再动枚举。

## 验收

```
cargo test --locked --all-features --test lexicon_handler sentence_translation
cargo test --locked --all-features --lib openapi
cargo test --locked --all-features --lib sentence_translation_language
```

集成测试依赖隔离数据库，按仓库既有的 `tests/` 约定起库。

前端（后端上线后执行）：

```
pnpm --filter @tsz/api-client sync:openapi && pnpm --filter @tsz/api-client test
```
