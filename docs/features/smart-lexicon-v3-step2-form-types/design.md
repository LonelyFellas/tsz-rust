# Smart Lexicon V3 Step 2 词形类型能力技术设计

> 状态：已实现并验证
>
> 配套需求：[`requirements.md`](requirements.md)

## 1. 当前实现与根因

### 1.1 已有 V3 结构可以承载目标语义

- `WordPosFormsV3.forms[]` 保存所有具体词形，`base` 不再是单例字段。
- `WordFormGroupV3.members[]` 只引用稳定 form UUID，不表达 base/derived 父子关系。
- contract 允许多个 base、同一 `form_type` 多条及一个 form 被多个 group 引用。
- `lexicon.v3_concrete_forms.form_type` 是 `TEXT`；数据库没有把值限制为当前 Rust enum。
- publication snapshot、surface、例句关联投影均保存/传播 `form_type` code。

### 1.2 真正阻塞点

```text
src/lexicon/form_types.rs
  -> catalog_form_types()
       -> CatalogPart.allowed_form_types/default_form_types
  -> v3_contract::validate_forms()
       -> invalid_form_type_for_part_of_speech
  -> legacy V2 structure validation / initial form-group behavior
  -> sentence_association::associable_pos()  (不应继续耦合)
```

`WordFormTypeV3` 同时是 serde 和 OpenAPI closed enum；枚举之外的 JSON 会先被 raw V3 contract
转为带 form UUID 定位的 `validation_failed`。

## 2. 共同设计原则

已确认的 fixed enum 路线遵守：

1. catalog 与 V3 save/complete/validate/publish 共用一个后端 authority，不复制 POS 表。
2. `base` 与其他 concrete form 类型同级，全部属于当前词条聚合及对应 POS；`base` 永远允许且
   不放入“非 base 可选类型”catalog 列表，多个 base 继续由 `forms[]` 表达。
3. 任一 POS 都允许完整 fixed enum，不再按 `(pos, form_type)` 设白名单；不引入派生 POS。
4. fixed enum 之外的未知 code 在原始 V3 contract 检查阶段 fail closed，并保留 form UUID 定位。
5. 例句自动关联改为显式 `ASSOCIABLE_POS` 判据，保持 noun/verb/adjective/adverb，不再借用
   `allowed_form_types().is_empty()`。
6. 历史数据先审计再启用；没有静默修复路径。
7. 地区模式从现有 `regional_variants.mode` 推导，按 POS 统一；不新增 catalog 或 POS wire 字段。

## 3. D1（已确认）：固定枚举、POS 能力规则

### 3.1 模型

使用 closed enum；当前实现为：

```rust
enum WordFormTypeV3 {
    Base,
    ThirdPersonSingular,
    PresentParticiple,
    PastTense,
    PastParticiple,
    Plural,
    Comparative,
    Superlative,
}
```

本期 enum 固定为当前八项，不新增 variant。`src/lexicon/form_types.rs` 维护唯一有序的七项
non-base authority；`allowed_form_types()` 对所有 POS（包括自定义 POS）返回同一全集。

### 3.2 wire / OpenAPI

字段和 enum schema 均不变，catalog 的运行时数组值统一为完整七项：

```json
{
  "code": "pronoun",
  "allowed_form_types": [
    "third_person_singular",
    "present_participle",
    "past_tense",
    "past_participle",
    "plural",
    "comparative",
    "superlative"
  ],
  "default_form_types": [
    "third_person_singular",
    "present_participle",
    "past_tense",
    "past_participle",
    "plural",
    "comparative",
    "superlative"
  ]
}
```

- `WordConcreteFormV3.form_type` 仍引用 `WordFormTypeV3`。
- `allowed_form_types/default_form_types` 仍为 `WordFormTypeWithoutBase[]`。
- 执行 `cargo run --bin export_openapi`；预期无 schema diff，不能手改。

### 3.3 校验与读取

- authority 返回 typed slice，catalog 序列化和 V3 validation 直接消费同一 slice。
- `base` 不查列表，继续始终允许。
- 所有已知 enum 对所有 POS 合法，不再产生 `invalid_form_type_for_part_of_speech`。
- enum 外 JSON 在 serde 前由 raw contract 返回 `validation_failed`，issue 为
  `code=invalid_form_type_for_part_of_speech`、`field=form_type`、`node_id=form.id`、
  `node_location.form_id=form.id`，不落库。

### 3.4 legacy V2 影响

V2 也消费同一个 `allowed_form_types()`，因此同步允许任一 POS 使用完整 fixed enum，并为原先
空列表 POS 初始化一个空 form group。这是共享 catalog authority 的既定行为。

已确认采用 A1：V2/V3 共用新 authority，catalog 对两代保存一致；增加 V2 回归测试。原先用
“是否有 form types”推断例句自动关联范围的代码必须解耦，避免将所有 POS 纳入关联。

### 3.5 代码范围

- `src/lexicon/form_types.rs`：typed POS capability authority 和单测。
- `src/lexicon/v3_contract.rs`：pronoun/空列表 POS 成功、非法组合失败、重复类型与多个 base 回归。
- `src/lexicon/sentence_association.rs`：拆出显式实词判据，行为保持。
- `src/lexicon/validation/structure.rs`、`src/lexicon/service/entry.rs`：按 A1 更新 legacy 预期。
- `tests/catalog_handler.rs`：11 个内置 POS 的精确 catalog fixture。
- `tests/lexicon_handler.rs`：真实 HTTP save/complete/validate 及错误定位。
- `src/openapi.rs`、`docs/openapi.json`：重导和 enum contract 断言。

fixed enum 放宽本身不需要 migration；后续正式 `dialect_rules` 持久化使用 §3.7 的可回滚
migration。

### 3.6 POS 级正式 `dialect_rules`

V3 新增独立正式 schema：

```json
{
  "pos_id": "<uuid>",
  "pos": "adjective",
  "dialect_rules": {
    "spelling_mode": "unified",
    "phonetic_mode": "distinguish"
  },
  "forms": [],
  "form_groups": []
}
```

`DialectModeV3` 为 `unified|distinguish`，`DialectRulesV3` 不引用 V2 schema。联合规则：

| 规则 | form shape | 额外约束 |
| --- | --- | --- |
| UU | 全部 common | common 拼写与发音集合 |
| UD | 全部 uk_us | 每个 form 的 UK/US spelling 必须逐字相同 |
| DD | 全部 uk_us | UK/US 拼写可相同或不同 |
| DU | 非法 | 返回 `dialect_rules_invalid` |

`v3_contract::validate_forms()` 是 save/complete/validate/publish、forms impact、V2→V3 target validation
和 history activation 的共享 authority。校验直接遍历 `pos.forms[]`，不按 membership 重复处理。

错误定位：

- 缺字段、未知 mode、DU：`code=dialect_rules_invalid`、`field=dialect_rules`、
  `node_id=pos_id`、`node_location.node_role=forms.pos`、`node_location.pos_id=pos_id`；
- UU/UD/DD 与 form shape 不一致，或 UD 的 UK/US spelling 不同：
  `code=invalid_regional_variant_shape`、`field=regional_variants`、`node_id=form_id`、
  `node_location.node_role=forms.concrete_form`，同时带 `pos_id/form_id`。

### 3.7 持久化与历史迁移

active draft 复用 `lexicon.entry_pos.spelling_mode/phonetic_mode`，并继续把完整 forms 保存到
`entry_editor_projection.forms`。已落库 migration
`20260827100000_add_lexicon_v3_dialect_rules` 保持 checksum 不变；follow-up
`20260829110000_require_fresh_v3_dialect_contract` 在正式启用 latest contract 前执行一次性数据门禁：

1. 前一 migration 更新 versioned CHECK，使 V2/V3 均要求合法非 NULL mode；
2. 若发现 migration 前已经存在 schema 3 entry，则明确失败并要求先执行已批准的数据清理；
3. 不回填 editor JSON，不读取或转换缺字段 snapshot；
4. 新 HTTP 请求、数据库 editor projection 与 publication snapshot 均必须显式携带合法规则；
5. down migration 仅承担代码回滚，不构成旧 V3 数据兼容承诺。

2026-08-29 产品决定：Smart Lexicon 未上线历史数据全部清理，代码只支持 latest contract。缺字段或
mixed common/uk_us shape 不做推导；仍在产品中使用的 V2 路由和功能不受该决定影响。

## 4. 本期不选：V3 可配置/自定义词形类型

### 4.1 模型

新增 POS 级 V3 类型定义，例如：

```text
catalog.part_of_speech_v3_form_types
  id UUID PK
  part_of_speech_id UUID FK -> catalog.parts_of_speech ON DELETE RESTRICT
  code TEXT
  name_zh TEXT
  name_en TEXT
  sort_order INTEGER
  is_default BOOLEAN
  status TEXT  -- active | retired
  revision / audit fields
  UNIQUE(part_of_speech_id, code)
```

`base` 是系统保留 code，不进入配置表。配置 code 使用与 POS code 相同级别的稳定标识规则；展示
名可改，code 创建后不可改。

Rust 将 `WordFormTypeV3` 拆为系统 `base` 加受约束 code。实现形式可为经过验证的 newtype，而不是
在 enum 上继续追加业务 variant。所有现有 exhaustive `match` 改为统一的 `as_str()`/parse helper，
避免 service、surface、publication、migration 各保留一份字符串表。

### 4.2 建议 wire

为避免改变 legacy V2 已有字段的语义，保留当前
`allowed_form_types/default_form_types`，新增 V3 专属定义：

```json
{
  "code": "pronoun",
  "allowed_form_types": [],
  "default_form_types": [],
  "v3_form_types": [
    {
      "code": "subject",
      "name_zh": "主格",
      "name_en": "Subject",
      "sort_order": 10,
      "is_default": true,
      "status": "active"
    }
  ]
}
```

上例的 code/文案仅用于展示 wire 形状，不代表已确认产品枚举。

V3 form wire：

```json
{
  "id": "019f...",
  "form_type": "subject",
  "regional_variants": {
    "mode": "common",
    "common": {
      "id": "019f...",
      "dialect": "common",
      "spelling": "he",
      "origin": "manual",
      "pronunciations": []
    }
  }
}
```

OpenAPI 对 `form_type` 只表达：string、长度、code pattern 和 `base` 保留语义；是否适用于当前 POS
由 catalog + 服务端运行时校验决定。前端必须拒绝 catalog 未加载时新增类型，不能从 OpenAPI 的
string 放宽推导“任何值均可”。

### 4.3 配置生命周期与历史数据

- active 定义可用于新节点和 form_type 变更。
- 被 active draft 或任何保留 publication 使用的定义不能硬删除，只能 retired。
- retired 定义继续随 catalog/lookup 返回展示元数据，保证历史页面不显示裸 code。
- 既有 form UUID 的未改动 retired type 可以读取和普通 draft save；新节点或改变 type 到 retired
  必须 fail closed。
- complete/publish 是否允许 retired 由产品确认。默认设计为返回定位到 form UUID 的稳定 issue，
  要求管理员显式修改或删除。
- publication snapshot 继续保存稳定 code；定义行的保留策略保证历史解释性，不改写 snapshot。

迁移先为现有固定值建立定义，再扫描 active draft 和 schema 3 publication 的实际 pair；任何无法
归类的值使 migration/上线审计失败，不自动映射。

### 4.4 一致性与并发

- catalog 读取和配置更新继续递增 `catalog.metadata.version`。
- forms save 在事务内按 POS 的 catalog UUID + form type code 校验 active/retired 状态。
- 配置 retire 与 forms save 并发时锁定定义行或依赖固定 FK/事务顺序，不能出现 catalog 已删但
  新 draft 写入成功的窗口。
- publication 写入前重新校验定义状态，不能只信早先的 save 结果。
- create/save/validate/publish 共用 repository/service capability 查询，不维护内存硬编码副本。

### 4.5 代码与契约范围

- catalog migration、model/repository/service/handler/OpenAPI 与管理端请求 DTO。
- `WordFormTypeV3` 及所有 parse/name match：`dto/v3.rs`、`service/v3*.rs`、
  `v3_contract.rs`、`v3_migration.rs`、`v3_projection.rs`、例句关联投影。
- catalog fixture、V3 HTTP/DB、migration、publication/history/surface contract 测试。
- `docs/openapi.json` 必须重导且会有契约 diff；前端需重跑 OpenAPI/runtime schema 同步。

## 5. 历史审计

部署前的只读运维审计至少覆盖：

```sql
-- active V3 draft
SELECT DISTINCT pos.code, form.form_type
FROM lexicon.v3_concrete_forms form
JOIN lexicon.entry_pos entry_pos ON entry_pos.id = form.entry_pos_id
JOIN catalog.parts_of_speech pos ON pos.id = entry_pos.part_of_speech_id;
```

历史/current publication 不能只查 active relational rows；还要解析/投影 schema 3 immutable
snapshot，汇总同一 `(pos, form_type)`。审计只报告，不写数据。

## 6. 测试矩阵

| 层 | 场景 | 预期 |
| --- | --- | --- |
| authority | 11 个内置 POS / 自定义 POS | 任一 POS code 返回相同有序全集；POS 是否存在仍由 catalog 校验 |
| catalog HTTP | pronoun 等原空列表 POS | 响应能力与 authority 完全相同 |
| V3 contract | pronoun + 允许的非 base 类型 | save/complete 无 type issue |
| V3 contract | 任一 POS + 任一 fixed type | save/complete 无 type issue |
| raw contract | 未知/非法 type code | `validation_failed` 精确定位 form UUID，不降级、不落库 |
| V3 HTTP + DB | create → save → get → validate | form type、UUID、顺序、variant、pronunciation 原样 round-trip |
| V3 publication | complete → publish → history read | snapshot/surface 保留 type，发布校验与 catalog 一致 |
| V3 regression | 多个 base、同类型多条 | 均成功并保持顺序 |
| membership | 一个 form 跨组共享 | 成功；同一 group 重复 membership 仍失败 |
| history | 旧白名单不匹配的已知 enum 值 | 自动成为合法组合，内容与 UUID 不被转换/删除 |
| association | pronoun/preposition 能添加 form | 自动例句关联 POS 集合仍保持原四类 |
| OpenAPI | 导出与断言 | `DialectModeV3`、`DialectRulesV3` 注册且 POS 字段必填 |
| dialect rules | UU / UD / DD | HTTP 保存、DB mode、读取回显均一致 |
| dialect rules | DU / 缺字段 | `dialect_rules_invalid` 定位 POS |
| rule/shape | UU/UD/DD 与 form 不一致 | `invalid_regional_variant_shape` 定位 form |
| shared form | 同一 form 被多个 group 引用 | form 只校验一次，不复制、不重复 issue |
| latest storage | 缺字段 active/snapshot | 读取失败；不推导、不改写 |
| migration | fresh schema / 已存在 V3 行 | fresh 安装约束；遗留 V3 行 fail closed |

验证命令：

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo run --bin export_openapi
git diff --exit-code -- docs/openapi.json
```

## 7. 决策与实施结果

已确认：

1. fixed enum 维持当前 `base + 7`。
2. 所有 POS（包括自定义 POS）共享全集。
3. legacy 采用 A1，V2/V3 共用规则。

实施按失败回归 → authority/validation → catalog/HTTP fixture → OpenAPI/全量 Rust 门禁完成。
