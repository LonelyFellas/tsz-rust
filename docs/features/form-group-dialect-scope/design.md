# 设计：英美配置下沉到变化组，词义可绑专用组

档位：**重档**（迁移 + 破坏性契约 + 前后端同批）。需求与未决问题见 `requirements.md`。
下面按「未决问题全部采纳建议」写；会上改了口径，改对应小节即可。

## 1. 现状与依赖

| 层             | 现在                                                                                                    | 位置（tsz-rust 以 `tsz-rust/` 前缀）                                                                                                                                                                       |
| -------------- | ------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| wire           | `WordPosFormsV3.dialect_rules`；`WordFormGroupV3 { id, is_regular, members }`；`WordSenseV3` 无组字段 | `tsz-rust/src/lexicon/dto/v3.rs:353-420, 936`；`packages/types/src/admin-word-v3.ts:39-120, 534`                                                                                                           |
| 存储           | `lexicon.entry_pos.spelling_mode / phonetic_mode`；`v3_form_groups` 无模式列；`v3_group_memberships` 唯一键 `(form_group_id, form_id)` 允许跨组共用；`lexicon.senses` 无组列 | `migrations/20260825100000_expand_lexicon_v3_storage.up.sql`、`20260827100000_add_lexicon_v3_dialect_rules.up.sql`、`20260811120000_create_lexicon_meanings.up.sql:34`                                     |
| 校验           | `validate_dialect_rules` 按词性规则逐个检查词形；membership 只查重复 / 跨词性 / 不存在，允许一形多组      | `tsz-rust/src/lexicon/v3_contract.rs:120-330, 348-400`                                                                                                                                                     |
| 写入           | 词性 upsert 绑两列；组 insert 不带模式                                                                    | `tsz-rust/src/lexicon/service/v3.rs:4460-4510`；词义 `repository/projections.rs:102`                                                                                                                       |
| 新建 / 第 1 步 | 建议词形推出词性规则；第 1 步 headwords 确认后改写词性规则并转换词形                                     | `tsz-rust/src/lexicon/service/v3.rs:153, 267-285, 358-420`                                                                                                                                                  |
| 列表方言列     | `array_agg(DISTINCT entry_pos.spelling_mode)` → `v3_list_dialects`                                       | `tsz-rust/src/lexicon/repository/query.rs:273-280`、`service/queries.rs:592, 667`                                                                                                                          |
| 影响预览       | `forms_impact_v3(current, proposed, meanings)` + `reconcile_v3_meanings_after_forms`                   | `tsz-rust/src/lexicon/service/v3.rs:3812`                                                                                                                                                                  |
| 语法结构形态   | 后端按第 1 步 headwords 放行（unified 只收 common；distinguish 收 common 或 uk+us）；前端按词性拼写模式生成 | `tsz-rust/src/lexicon/validation/meanings.rs:73`；`apps/admin/.../word-creation-v3/meaningsModel.ts:412-425, 652`                                                                                           |
| 前端第 2 步    | 开关画在每张组卡片里但改整个词性；合并冲突整体拒绝                                                       | `components/V3PosTab.tsx:82-112, 328-336`；`operations.ts:371-520`（`updatePosDialectRules` / `normalizePosDialectRules`）；`V3FormGroupCard.tsx:343, 501, 536`                                             |
| 发布快照       | 整个 entry JSON 原样入库，形状变了旧快照即失效                                                            | `tsz-rust/src/lexicon/service/v3_publication.rs:38, 511`                                                                                                                                                   |

不受影响：`presentation_from_native_forms` 按规范化拼写去重，`job` / `Job` 归一后仍是一个词头；
`v3_meaning_validation_forms` 适配器硬编码 distinguish，不读词性规则；web 端不读词形。

## 2. 方案取舍

### 2.1 组级规则怎么落：保留 membership 并收紧 1:1（选定）

| 方案                                                 | 优点                                                                  | 代价                                                                                                                  |
| ---------------------------------------------------- | --------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| **A. 保留 `forms[]` + `members[]`，加约束「一形一组」** | wire 只加不挪；`V3DraftNodeLocation`、影响预览、各类引用的 `form_id` 全不动 | 契约上仍是两层，1:1 靠校验 + 唯一索引保证                                                                             |
| B. 词形内嵌进组（`group.forms[]`，删 memberships）    | 结构最直白                                                             | 定位、影响预览、`text_links` / 例句关联 / 成分用词的祖先链、`v3_group_memberships` 表全要动，工作量约翻倍，收益只是形状好看 |

选 A。B 可以作为后续清理单独排。

### 2.2 「通用 / 专用」显式字段（选定）vs 由绑定派生

显式 `scope`。派生方案做不了「专用组必须被绑定」的完成校验，第 2 步也无法表达意图。

### 2.3 绑定方向：词义持有 `form_group_id`（选定）

向导顺序 基础 → 词形 → 词义 → 预览，第 2 步保存时词义未必存在，组上存词义列表没法校验；
由第 3 步引用第 2 步节点，与例句 / 成分用词引用词形同向；删组时复用影响预览的 `Sense` 节点类型。

## 3. 契约变更

```ts
// packages/types/src/admin-word-v3.ts（镜像 tsz-rust/src/lexicon/dto/v3.rs）
export type FormGroupScopeV3 = "general" | "dedicated";

export interface WordFormGroupV3 {
  id: string;
  is_regular: boolean;
  scope: FormGroupScopeV3;          // 新增，必填
  dialect_rules: DialectRulesV3;    // 从 WordPosFormsV3 挪过来，必填
  members: WordFormGroupMemberV3[];
}

export interface WordPosFormsV3 {
  pos_id: string;
  pos: string;
  // dialect_rules 删除
  forms: WordConcreteFormV3[];
  form_groups: WordFormGroupV3[];
}

export interface WordSenseV3 {
  // ...现有字段
  /** 缺省 = 通用；只能指向同词性下 scope=dedicated 的组。 */
  form_group_id?: string;
}
```

`WordSenseWritableV3` 同步加 `form_group_id`。响应侧沿用 `sense_group_id` 的写法
（`skip_serializing_if = Option::is_none`、`nullable = false`）。

校验码（`V3ValidationIssueCode`，字符串与 `field` / `node_id`）：

| 码                                   | 触发                                                    | field           | node          | 时机        |
| ------------------------------------ | ------------------------------------------------------- | --------------- | ------------- | ----------- |
| `dialect_rules_invalid`（沿用）      | 组规则非法组合                                          | `dialect_rules` | group         | save        |
| `invalid_regional_variant_shape`（沿用） | 词形形态与**所属组**规则不符                          | `regional_variants` | form      | save        |
| `form_group_membership_invalid`（沿用，新消息） | 同一词形出现在两个组                            | `form_id`       | membership    | save        |
| `sense_form_group_invalid`（新）     | 绑定的组不存在 / 不在本词性 / 不是专用组                | `form_group_id` | sense         | save        |
| `dedicated_form_group_unused`（新）  | 专用组没有任何词义绑定                                  | `scope`         | group         | complete / publish |
| `sense_form_group_required`（新）    | 词义未绑定且本词性没有通用组                            | `form_group_id` | sense         | complete / publish |

`V3DraftNodeLocation` 已有 `form_group_id`，组级问题定位不用加字段；前端组卡片已有
`data-v3-node-id={group.id}`，把 `data-v3-field="dialect_rules"` 与新加的 `scope` 挂到卡片内即可。

## 4. 存储与迁移

新迁移 `2026MMDDHHMMSS_form_group_dialect_scope`（时间戳定下后同步改
`tsz-rust/src/deployment_migrations.rs:114` 的 `CURRENT_RELEASE_VERSION`，否则回退测试齐红）。

```sql
-- up
ALTER TABLE lexicon.v3_form_groups
    ADD COLUMN scope TEXT NOT NULL
        CONSTRAINT lexicon_v3_form_groups_scope_check CHECK (scope IN ('general', 'dedicated')),
    ADD COLUMN spelling_mode TEXT NOT NULL
        CONSTRAINT lexicon_v3_form_groups_spelling_mode_check CHECK (spelling_mode IN ('unified', 'distinguish')),
    ADD COLUMN phonetic_mode TEXT NOT NULL
        CONSTRAINT lexicon_v3_form_groups_phonetic_mode_check CHECK (phonetic_mode IN ('unified', 'distinguish')),
    ADD CONSTRAINT lexicon_v3_form_groups_modes_check
        CHECK (spelling_mode <> 'distinguish' OR phonetic_mode = 'distinguish');

-- 一形一组：新唯一键蕴含旧的 (form_group_id, form_id)，旧键可删。
ALTER TABLE lexicon.v3_group_memberships
    ADD CONSTRAINT lexicon_v3_group_memberships_form_key UNIQUE (form_id);

-- 词义绑定：复合外键顺带锁死「同一词性」。投影是整体重建（先 DELETE 后 INSERT，同一事务），
-- 重建顺序里词义在词形之后写入即可；实现时确认顺序，必要时加 DEFERRABLE INITIALLY DEFERRED。
ALTER TABLE lexicon.senses
    ADD COLUMN form_group_id UUID,
    ADD CONSTRAINT lexicon_senses_form_group_fkey
        FOREIGN KEY (form_group_id, entry_pos_id, entry_id)
        REFERENCES lexicon.v3_form_groups(id, entry_pos_id, entry_id)
        DEFERRABLE INITIALLY DEFERRED;  -- 实施时改为不级联 + 延迟检查，见 §12

ALTER TABLE lexicon.entry_pos
    DROP CONSTRAINT lexicon_entry_pos_versioned_modes_check,
    DROP COLUMN spelling_mode,
    DROP COLUMN phonetic_mode;
```

无兼容：`NOT NULL` 不给默认值，表空才跑得过；测试服先清库再部署。down 反向即可。
`.sqlx` 离线数据随 pre-commit 重新 prepare。

## 5. 后端改动清单（tsz-rust）

| 文件                                        | 改动                                                                                                                                                                                    |
| ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/lexicon/dto/v3.rs`                     | §3 三个结构 + `FormGroupScopeV3` 枚举 + 三个新问题码；`WordSenseWritableV3` 加字段                                                                                                        |
| `src/lexicon/v3_contract.rs`                | `validate_dialect_rules` 改为按组：先建 `form → group` 映射，每个词形按所属组规则查形态；membership 计数 >1 报错；raw JSON 预检 `valid_raw_dialect_rules` 改读组；相关单测与 fixture 搬字段 |
| `src/lexicon/validation/meanings.rs`        | 加 `sense.form_group_id` 结构校验（需要 forms 内容，现有签名已接收 `form_pos`，补传组表）；complete / publish 加两条完成规则                                                                |
| `src/lexicon/service/v3.rs`                 | `suggested_v3_dialect_rules` 结果落到初始组；`apply_confirmed_v3_headwords` 改写每个组的规则（新建时只有一组）；组 insert 绑三列、词性 upsert 去两列；`reconcile_v3_meanings_after_forms` 清掉指向已删 / 改回通用的组的绑定并计入 `affected`（`node_type: sense`，reason `form_group_binding_cleared`） |
| `src/lexicon/repository/projections.rs`     | 词义 insert 绑 `form_group_id`                                                                                                                                                          |
| `src/lexicon/repository/query.rs`、`service/queries.rs` | 列表方言列改聚合 `v3_form_groups.spelling_mode`                                                                                                                              |
| `src/lexicon/v3_projection.rs`、`service/sentence_association_tests.rs` 等 | 只是 fixture 里 `dialect_rules` 换位置                                                                                                                                |
| `src/openapi.rs` → `docs/openapi.json`      | 重新生成                                                                                                                                                                                |
| `src/deployment_migrations.rs`              | `CURRENT_RELEASE_VERSION`                                                                                                                                                               |

`v3_publication.rs` 不改逻辑；快照随 DTO 自然带上新字段。

## 6. 前端改动清单（tsz）

| 文件                                                                 | 改动                                                                                                                                                                                                                                                      |
| -------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `packages/api-client`（`sync:openapi`）                              | `openapi.snapshot.json`、`admin-word-v3.runtime-schema.json` 重生成；`endpoints.contract.test.ts` / `runtime-schema.test.ts` / `admin-word-schema.test.ts` 的样本搬字段                                                                                  |
| `packages/types/src/admin-word-v3.ts`                                | §3                                                                                                                                                                                                                                                        |
| `word-creation-v3/operations.ts`                                     | `updatePosDialectRules` / `normalizePosDialectRules` 改成 `…GroupDialectRules(content, posId, groupId, rules, …)`，只遍历本组成员；`addFormGroup` 新组 `scope: "general"`、规则复制本词性最后一组；`addPartOfSpeech` 模板规则落到初始组；删掉无调用方的 `addMembership`；`removeMembership` 在 1:1 下等价删词形，卡片上「移除 / 删除」两个动作合成一个（可后置） |
| `word-creation-v3/model.ts`                                          | 本地校验镜像后端：`isDialectRulesValid` 按组、`regionalVariantsMatchRules` 查所属组、一形多组报错；`clone` 位置同步                                                                                                                                       |
| `word-creation-v3/readiness.ts`                                      | 235 行英美判断与 254 行方言页签按组算                                                                                                                                                                                                                     |
| `word-creation-v3/meaningsModel.ts`                                  | `spellingModeForPos` 改为「任一组 distinguish」；652 行 `spellingModeByPos` 同源                                                                                                                                                                          |
| `components/V3PosTab.tsx`                                            | `dialectControl` 变成每组一份（`applyDialectRules(groupId, rules)`），删 `dialectScopeNote`；合并冲突提示只针对本组；组卡片头加「通用 / 专用」切换（antd `Segmented`），专用且已被绑定时显示「已绑定 N 个词义」（绑定数从向导的 `meanings` 状态算，不做本地清除，交给保存时的影响预览确认） |
| `components/V3FormGroupCard.tsx`、`V3ConcreteFormRow.tsx`、`V3DialectSeparatedFormMatrix` | `dialectRules` 改从 `group.dialect_rules` 取                                                                                                                                                                                                  |
| `V3MeaningsAndExamplesStep.tsx`                                      | 词义卡片加「词形与发音」`Select`（`data-v3-field="form_group_id"`）：选项「通用（默认）」+ 本词性各专用组「第 N 组 · <原形拼写>」；本词性没有专用组时不渲染                                                                                                  |
| `V3MeaningsPreview.tsx`                                              | 词义行显示绑定的组                                                                                                                                                                                                                                        |
| `V3WordCreationWizard.tsx:480` 附近                                  | 语法结构形态重算的前提注释按新来源改                                                                                                                                                                                                                      |
| fixtures：`word-creation-v3/fixtures.ts`、`dictionary/mock/fixtures.ts`、`sentences/fixtures.ts`、`e2e/tests/support/mockAdminV3Api.ts` | 搬字段                                                                                                                                                                                                                       |
| `e2e/tests/admin-word-v3.spec.ts:154`                                | payload 断言改为 `form_groups[0].dialect_rules`；补一条「第 2 组独立设不区分」用例                                                                                                                                                                         |

词条列表页不改：后端 `dialects` 字段形状不变。

## 7. 数据与状态流

1. **新建**：检测建议词形 → `suggested_v3_dialect_rules` → 写进初始组；第 1 步确认「区分英美」
   → `apply_confirmed_v3_headwords` 改写该词性所有组（此时只有一组）并转换词形。
2. **第 2 步**：每组独立切换英美；切换只转换本组成员词形，变体 ID 走 stable factory。
   新建组默认通用、规则复制上一组。标专用不需要任何前置条件。
3. **第 3 步**：词义卡片选组；只列本词性专用组。第 2 步删组 / 改回通用后，`reconcile` 清绑定。
4. **保存第 2 步**：`preview-forms-impact` 返回被清绑定的词义（`requires_confirmation: true`），
   确认后保存；响应里的 `meanings` 已是清过的。
5. **完成 / 发布**：两条完成规则 + 现有规则；问题定位到组卡片或词义卡片。
6. **列表**：方言列按组聚合。
7. **发布快照**：带 `scope` / 组级 `dialect_rules` / `form_group_id`，供 C 端将来按词义选组。

## 8. 发布顺序

同批：后端合 main → 前端 `sync:openapi` 合 main → 测试服**先清库**（口径见 requirements 约束）
→ 部署后端 → 部署前端 → 用新建词条走一遍验收清单。不清库则旧投影 JSON 缺组级字段，读取即 422。

## 9. 风险与验证

| 风险                                                     | 验证 / 缓解                                                                                          |
| -------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| 投影重建顺序与新外键冲突                                  | 实现时读 `rebuild` 事务顺序；不确定就 `DEFERRABLE INITIALLY DEFERRED`                                |
| 影响预览漏报被清绑定的词义                                | handler 测试：删被绑定组 → `affected` 含该 sense；改回通用同样                                        |
| 前端本地校验与后端口径漂移（一形多组、组规则）             | `model.ts` 单测对照后端 `v3_contract.rs` 用例；契约测试 PENDING 白名单不应新增                          |
| fixture 面广，`dialect_rules` 搬位漏改                    | `pnpm typecheck` 全仓 + `cargo test`；strict schema 会把漏改的响应样本直接打红                          |
| 语法结构形态前后端口径本就不完全一致（后端看第 1 步、前端看拼写模式） | 本次只改前端取值来源，不动口径；记入未决 5 的结论                                                    |
| 组级切换英美改变变体 ID 槽位                              | 沿用 stable factory；e2e 覆盖「第 2 组单独切换后保存 200」                                            |

## 10. 工时估算

前提：未决问题按建议拍板；不含 C 端；不做兼容。

| 侧     | 内容                                                                               | 人日      |
| ------ | ---------------------------------------------------------------------------------- | --------- |
| 后端   | 迁移 + 账本 + sqlx（0.5）；DTO / OpenAPI / 契约校验（1）；写入、新建路径、列表查询（1）；影响预览与 reconcile（0.5）；测试与 fixture（1） | **≈ 4**   |
| 前端   | sync + 类型 + api-client 测试 + fixture（0.5）；第 2 步按组逻辑与 scope 控件（1.5）；第 3 步选择器与预览（1）；e2e 与向导联调（1）       | **≈ 4**   |
| 联调   | 清库、同批部署、验收清单                                                           | **0.5–1** |
| 合计   |                                                                                    | **≈ 8.5–9 人日** |

方案 B（词形内嵌进组）在此基础上后端 +2、前端 +2，不建议本期做。

## 11. 文档跟进

- `tsz-rust/docs/word-data-model.md` §8.1 / §8.2：模式列从 `entry_pos` 挪到组，`senses` 加 `form_group_id`。
- `tsz-rust/docs/features/smart-lexicon-v3-step2-form-types/requirements.md` 决定记录：追加 2026-09 反转条目。
- `tsz-rust/docs/frontend-integration.md`：同批发布与清库说明。
- 记忆 `project_list_dialect_column_v3_semantics`（列表方言列口径）部署后更新为按组。

## 12. 后端实施记录（2026-09-13）

与上文的出入以代码为准：

- **词义外键不级联、延迟到提交检查。** 词形保存在同一事务里先重写词义、再整体删除并重插全部组。
  `ON DELETE CASCADE` 会在删组那一刻连带删掉刚写入的词义，所以改为 `DEFERRABLE INITIALLY DEFERRED`
  的 `NO ACTION` 外键。被删掉或改回通用的组，由 `reconcile_v3_meanings_after_forms` 先清掉绑定。
- **旧唯一键删除。** `v3_group_memberships` 的 `(form_group_id, form_id)` 唯一键被 `UNIQUE (form_id)` 取代。
- **迁移前置守卫。** 库里还有 V3 词条时迁移直接报错，沿用 `require_fresh_v3_dialect_contract` 的做法。
  down 迁移在存在专用组、词义绑定或同词性各组规则不一致时拒绝回退。
- **绑定校验在 `v3_contract::validate_sense_form_groups`。** V2 语义校验器拿不到组表，所以没有放进
  `validation/meanings.rs`。调用点是词义步保存（两个 intent）、validate 端点和发布；词形步保存不调用。
- **`WordSenseV2` 加了 `form_group_id`。** 它是写关系投影的内部结构，不在 OpenAPI 里；
  V3 → V2 → V3 往返靠它才不丢字段。
- **部署回退守卫。** `deployment_migrations::undo` 把「词性上缺 `dialect_rules`」和 `senses[*].form_group_id`
  登记为回退版本读不了的形状。按词性判断而不是按组判断，词性下还没有组的草稿也能拦住。
- **完成状态随绑定规则失效。** 词形保存与词义草稿保存在沿用词义步的完成状态之前，按 complete 口径重跑
  `validate_sense_form_groups`。第 2 步把组改成专用、删掉唯一的通用组，或第 3 步草稿清掉绑定后，
  `completed_steps` 会去掉 `meanings`，不用等到 validate 或发布才发现。
- **ops 发布脚本。** `ops/lexicon-publish/publish_words.py` 同步把 `dialect_rules` 挪进组，并带 `scope: "general"`。

## 13. 后端验收命令

```bash
cargo test --locked --all-features --lib -- v3_contract::tests::sense_form_group_bindings_follow_scope_and_intent v3_contract::tests::dialect_rules_apply_per_group v3_contract::tests::wire_order_is_preserved_and_one_form_cannot_join_two_groups service::v3::tests::forms_impact_reports_senses_whose_dedicated_group_binding_is_cleared v3_list_dialects_tests
cargo test --locked --all-features --test lexicon_handler -- v3_dedicated_form_group_binds_senses_publishes_and_clears_through_impact v3_form_group_changes_invalidate_meanings_completion
cargo test --locked --all-features --test lexicon_v3_storage_schema v3_form_group_rules_scope_and_sense_binding_are_enforced
```
