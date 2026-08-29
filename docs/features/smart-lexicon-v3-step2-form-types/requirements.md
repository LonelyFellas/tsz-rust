# Smart Lexicon V3 Step 2 词形类型能力需求

> 状态：已实现并验证
>
> 日期：2026-08-27

## 1. 背景

Smart Lexicon V3 Step 2 当前已经把具体词形建模为独立 `forms[]`，并通过
`form_groups[].members[]` 表达共享 membership。存储和 V3 contract 已允许：

- 同一 POS 有多个 `form_type=base`；
- 同一 `form_type` 有多条具体词形；
- 同一具体词形被多个词形变化组共享；
- 每条具体词形使用 `common` 或完整 `uk+us` 地区变体；
- 每个变体有多条发音和稳定 UUID。

当前阻塞不是上述结构，而是 `src/lexicon/form_types.rs` 的 POS 白名单：只有 noun、verb、
adjective、adverb 返回非空类型，其余 POS 返回空列表。catalog 直接暴露该列表，V3 保存校验又
拒绝所有非 `base` 且不在列表内的具体词形。因此 pronoun 等 POS 只能新增多个 `base`，无法新增
其他类型的词形。

## 2. 产品术语与层级

Step 2 只使用以下三层：

```text
词性 POS
  -> 词形变化 form group
       -> 词形类型 form type / concrete form
```

- `form_group` 表示一组词形变化及其 membership。
- `form_type` 描述具体词形的类型。
- `base` 是与其他类型平级的 concrete form 类型，不是其他词形的父节点；所有 concrete form
  都归属于当前词条聚合及其中对应的 POS，可以有多条，也可以被多个 group 共享。
- 本功能不新增、不推导、不展示“派生词性”。

## 3. 功能需求

1. 每个可用于 V3 词条的 POS 都能保存多个 `base` 具体词形。
2. pronoun、preposition 等当前 `allowed_form_types=[]` 的内置 POS 能新增并保存非 `base` 词形。
3. V3 catalog 是前端可添加类型的唯一能力来源；前端不得按 POS code 自行推断。
4. V3 create/save/complete/validate/publish 使用与 catalog 相同的后端事实源。
5. 固定枚举之外的未知类型必须 fail closed，并以稳定 issue 定位具体 form UUID；不得降级为
   `base`、不得猜测最接近的类型。
6. 多个 `base`、同类型多条、跨组共享 membership、地区变体、多发音、稳定 UUID、校验定位和
   原生 V3 发布流程不得回归。
7. 已持久化但不符合新能力配置的历史词形不得被静默删除、改名或转换。
8. catalog 响应、Rust DTO/OpenAPI、前端 fixture 所需契约说明和后端测试 fixture 必须一致。
9. 不修改前端仓库；完成后在后端文档中给出前端需要同步的精确字段、枚举和示例响应。
10. `WordPosFormsV3` 必须正式持久化 `dialect_rules.spelling_mode/phonetic_mode`，枚举均为
    `unified|distinguish`。只允许 UU、UD、DD，DU fail closed。UU 对应全部 common；UD 对应全部
    uk_us 且每个 form 的 UK/US spelling 相同；DD 对应全部 uk_us。规则覆盖 POS 下全部 form/group，
    shared membership 仍只引用一个 concrete form 实体。

## 4. 明确不做

- 不引入“派生词性”或 POS 继承关系。
- 不改变 form group membership 结构。
- 不把校验搬到前端或跳过保存/发布校验。
- 不自动修复、删除或转换历史草稿。
- 不在未经单独授权时提交、推送、开 PR、合并或部署。

## 5. 已确认模型

### 已确认：保留固定 `WordFormTypeV3` 枚举

所有 POS（包括以后新增的自定义 POS）共享现有七种非 base 类型：

```text
third_person_singular
present_participle
past_tense
past_participle
plural
comparative
superlative
```

影响：

- **wire**：`WordConcreteFormV3.form_type` 及 catalog 的
  `allowed_form_types/default_form_types` 形状不变；每个 POS 都按上述顺序返回完整七项。
- **OpenAPI**：`WordFormTypeV3` 和 `WordFormTypeWithoutBase` 枚举本身不变；本次另增
  `DialectModeV3` / `DialectRulesV3` 并让 `WordPosFormsV3.dialect_rules` 成为必填字段。
- **存储/历史数据**：`form_type TEXT` 不迁移。属于固定枚举的所有历史 `(pos, form_type)` 组合
  立即合法，原样保留；枚举之外的数据库脏值继续 fail closed，不做转换。
- **前端 catalog**：继续消费字符串数组，无类型生成变更，只需更新 fixture 和中文显示矩阵。
- **限制**：无法准确表达固定枚举之外的代词类型，例如“主格/宾格/所有格/反身”；如果产品真正
  需要这些类型，不能拿 `plural`、`past_tense` 等不相干枚举冒充。
- **legacy V2**：V2/V3 共用同一 catalog authority，V2 同步允许所有 POS 使用完整 fixed enum。

### 本期不选：V3 使用可配置/自定义词形类型

为 catalog POS 配置带稳定 code 和展示名的 V3 词形类型；V3 concrete form 保存 code，而不是由
Rust closed enum 穷举所有业务类型。

影响：

- **wire**：`WordConcreteFormV3.form_type` 从 closed enum 变为受格式约束的 code；catalog 需要
  返回 code、中文名、英文名、排序、默认状态和可用状态。属于明确契约变更。
- **OpenAPI**：`WordFormTypeV3` 不再能用固定 enum 完整表达；动态“某 POS 允许哪些 code”只能
  由 catalog 在运行时给出，Rust/OpenAPI 只约束 code 格式和长度。
- **存储/历史数据**：现有 `TEXT` 列可保留，但要新增 catalog 配置表/迁移。已有固定枚举值要生成
  稳定定义；已被草稿或 publication 使用的定义只能停用、不能硬删除。
- **前端 catalog**：V3 Step 2 必须切到定义对象，不再把固定 TS union 当作授权来源；未知 code
  仍 fail closed。fixture、runtime schema 和显示文案均需同步。
- **优势**：能准确表达 pronoun 等 POS 的真实类型，也支持将来新增 POS，而无需每次扩 Rust enum
  和重新生成前端类型。
- **代价**：需要 catalog 管理、历史停用语义、迁移和更多契约测试，改动明显大于方案 A。
- 本期明确不建设该能力。

### 已确认结论

本期使用当前 `base + 7` fixed enum，不建设在线自定义类型系统，不新增 variant，不保留 POS
白名单。所有 POS 对 fixed enum 的能力完全相同。

## 6. 数据基线要求

V3 尚未上线，2026-08-29 已明确清空本地 Smart Lexicon 历史业务数据。fixed enum 模型只执行
latest contract：

1. 所有 V3 editor/publication JSON 必须显式携带 `dialect_rules`，缺字段直接拒绝。
2. 固定枚举之外的值、mixed common/uk_us shape、DU 或 rule/shape 不一致均 fail closed。
3. migration 仅为 fresh/latest 数据安装约束；检测到 migration 前遗留的 V3 行时明确失败，不回填、
   推导或转换。
4. 本决定不删除或停用仍在产品中使用的 V2 路由与功能。

## 7. 验收标准

- [x] pronoun 及至少一个当前空列表 POS 能通过 HTTP create → forms save/complete → validate，保存
      一个经 catalog 授权的非 `base` 词形。
- [x] catalog 对该 POS 返回的能力与保存/完成/发布校验完全一致。
- [x] 固定枚举之外的未知类型以 `validation_failed` +
      `invalid_form_type_for_part_of_speech` 精确定位 form UUID，不能落库或被转换。
- [x] 同一 POS 多个 `base`、同一非 base 类型多条均可保存。
- [x] 同一 form 被多个 group 共享，地区变体、多发音和稳定 UUID 不回归。
- [x] 扩展词形能力不会把 pronoun/preposition 等自动加入例句自动关联；该判据必须与词形能力
      解耦并保持现有实词集合。
- [x] latest contract 之外的数据按 §6 fail closed，不做旧 V3 兼容。
- [x] `cargo test --locked --all-features`、OpenAPI 导出和相关 contract fixture 通过。
- [x] 同一 POS 全部 common 可保存/完成，全部 uk_us 可保存/完成。
- [x] 同一 POS 跨 form group 混用 common/uk_us 返回稳定 issue，shared form 不被复制或重复报错。
- [x] mixed-mode 内容在读取/保存/完成/发布/激活均不受支持。
- [x] `WordPosFormsV3.dialect_rules` 在 OpenAPI 中必填，使用独立 V3 schema。
- [x] UU、UD、DD 均可 HTTP 保存、关系库存储、读取回显、完成与发布；DU 稳定拒绝。
- [x] rule/shape 与 UD 异拼写均定位冲突 form；缺字段新请求定位 POS。
- [x] migration 在 fresh/latest 基线上安装约束，遇到遗留 V3 数据明确失败。

## 8. 决策记录

- 2026-08-27：确认使用当前 `base + 7` fixed enum。
- 2026-08-27：确认所有 POS 共用完整 enum，不设置 POS 白名单。
- 2026-08-27：`base` 与其他 concrete form 同级，均属于当前词条聚合及对应 POS。
- 2026-08-27：地区模式按 POS 统一；同一 POS 的全部 concrete forms 共用 common 或 uk_us。
- 2026-08-27：批准正式 V3 `dialect_rules`；合法状态为 UU、UD、DD，DU 非法。
