# 设计：解除词形类型与英美结构的引用锁定

档位：**重档**（契约变更 + 前后端同批）。需求与口径见 `requirements.md`。

## 0. 实施状态（已落地）

评估基线 tsz-rust `origin/main d9dafb6`、tsz `origin/main b1db76e`。两仓均从 `main` 切 worktree、
同名分支 `feat/form-type-dialect-unlock`。

- 后端 worktree：`tsz-rust-task-58`
- 前端 worktree：`tsz-task-58`

实测（两仓）：

| 项 | 结果 |
|---|---|
| 后端 `cargo test --lib` | 276 passed / 14 failed（均为缺 `DATABASE_URL` 的集成测试，改动前后一致，零回归）；新增 6 个漂移测试全绿 |
| 后端 `cargo clippy --all-targets` | 零警告 |
| 前端 `pnpm --filter @tsz/admin test -- --run` | 1609 passed / 2 skipped / 0 failed |
| 前端 `pnpm --filter @tsz/admin typecheck` | 通过 |
| 前端 `pnpm --filter @tsz/admin lint` | 0 error；1 个既有 warning（`V3FormGroupCard` 的 `Typography` 未用，改动前即存在，不在本次范围） |
| OpenAPI | `docs/openapi.json` 已重导出；前端 `sync:openapi` 已同步 `target_dialect` |

与本文的两处简约化订正（实测后的决定，不是遗漏）：

1. **§3.5 的类型漂移确认流未做**。需求口径定为「保护式放开」，而「改类型会让 N 处引用失效」的提示
   前端本地就能做（它已有完整引用索引），不需要后端确认 token。后端只负责「类型漂移后引用仍成立」，
   前端负责提示。代价：直连 API 可绕过提示，但引用不会损坏，无数据完整性风险。见 §4。
2. **§3.4 的 `TextLinkV3.target_dialect` 已加**（例句引用需要），成分引用的 `target_dialect` 原本就有。

## 1. 现状与依赖

| 层 | 现在 | 位置 |
|---|---|---|
| 例句引用判定 | `id == link.target_variant_id` 硬相等 + 拼写归一 | `src/lexicon/service/text_links.rs:495` `shared_target_matches` |
| 成分引用判定 | `form.id == target_form_id` 且 `form_type` 相等 且变体 id 相等 | `src/lexicon/service/v3.rs:2924` `phrase_component_matches_target` |
| 保存守卫 | 本次新破坏的引用 → 409 `InboundReferenceConflict` | `service/inbound_references.rs:672` `ensure_inbound_references` |
| baseline 机制 | 在旧内容上先评估，排除本就 stale 的引用 | `service/v3.rs:1858` 传 `Some((&current_forms, &saved_meanings))` |
| 例句引用存储 | `TextLinkV3` 存 `target_variant_id`，**无 target 方言**；方言从标注 `source_dialect` 取 | `dto/v3.rs:691`、`service/inbound_references.rs:169` |
| 成分引用存储 | `PhraseComponentUsageV3::Resolved` **已存 `target_dialect` + `target_form_type`** | `dto/v3.rs:209-232` |
| 前端锁定（英美） | `split`/`merge` > 0 禁用开关 | `referenceGuard.ts:208`、`V3PosTab.tsx:151` |
| 前端锁定（类型/删除） | `formReferenceCount > 0` 禁用类型下拉与删除 | `referenceGuard.ts:150`、`V3FormGroupCard.tsx:264` |
| 前端结构转换 | 拆/并时按角色分配变体 id，跨角色不复用 | `operations.ts:465` `normalizeGroupDialectRules` |

不受影响：词义级引用（`publication_sense_ref` / `draft_relation` 只绑 `sense_id`）、
拼写一致性校验、发音/规则/成分类合并冲突、已发布冻结快照。

## 2. 方案取舍

### 2.1 引用定位：改「语义坐标」（选定）vs 只做 id 迁移

| 方案 | 优点 | 代价 |
|---|---|---|
| **A. 判定改绑 `(form_id, dialect)`，id 相等作快速路径** | 结构任意切换引用自动跟随；零迁移；新旧双兼容 | 需保证不误判（同一 form 下多变体语义可辨） |
| B. 只在转换时迁移变体 id | 判定逻辑不动 | 跨角色 id 复用会污染语义（common 的 id 一会儿是 uk 一会儿是 us）；无法覆盖"先建 uk/us 再合并"的存量引用 |

选 A。id 迁移作为**辅助**（无引用时按 preference 分配，减少无意义 id 漂移），但不是主手段。

### 2.2 变体语义坐标：`(form_id, dialect)`（选定）

`form_id` 定位到具体词形，`dialect ∈ {common, uk, us}` 定位到该词形下的方言侧：

- common 模式：只有 `Dialect::Common` 侧；
- uk_us 模式：有 `Dialect::Uk` / `Dialect::Us` 侧。

判定规则（结构无关）：

```
引用成立 ⟺ 目标 form 存在
          ∧ 该 form 在当前方言模式下存在 dialect 对应侧
          ∧ 该侧拼写归一后与引用片段匹配（例句）
```

**结构切换时**：
- common→uk_us：原 common 引用（dialect=common）需落到 uk 或 us。**用来源方言决定**（见 §3.3）。
- uk_us→common：原 uk/us 引用（dialect=uk/us）落到 common 侧。
- 1↔2（只音标不同）：拼写共用，判定天然成立。
- 1↔3 / 2↔3：拼写随方言侧变，按各自侧匹配。

### 2.3 词形类型：保护式放开（选定）

- 改类型**不换 `form.id`**，故引用可原地存活；
- `phrase_component_matches_target` 去掉 `form.form_type != target_form_type` 硬拒绝，
  改为**记录类型漂移**：类型变了仍算成立，但保存时若该引用指向的词形类型发生变化，
  返回可确认冲突，前端提示「会让 N 处引用失效」，用户确认后放行（见 §4）；
- 删除词形仍锁（引用失去锚点，数据完整性问题）。

## 3. 后端改动

### 3.1 例句引用判定（`shared_target_matches`）

去掉 `id == link.target_variant_id` 硬相等，改为按方言侧定位：

```rust
// 现状：matches(id, spelling, side) 要求 id == target_variant_id
// 改为：按 link 记录的 target 方言（新增字段，见 3.4）或 source_dialect 定位侧
let side_of = |form: &WordConcreteFormV3, dialect: Dialect| -> Option<&Variant> {
    match (&form.regional_variants, dialect) {
        (Common { common }, Dialect::Common) => Some(common),
        (UkUs { uk, .. }, Dialect::Uk) => Some(uk),
        (UkUs { us, .. }, Dialect::Us) => Some(us),
        // 结构漂移：common 引用落到 uk_us（或反向）时，按来源方言取对应侧
        (UkUs { uk, us }, Dialect::Common) => /* 见 3.3：来源侧优先，回退 uk */,
        (Common { common }, Dialect::Uk | Dialect::Us) => Some(common),
    }
};
```

**兼容**：先试 `id == target_variant_id` 快速路径（旧引用、同结构保存），命中即成立；
未命中再走语义坐标。这样存量引用行为不变，只在结构漂移时启用新逻辑。

### 3.2 成分引用判定（`phrase_component_matches_target`）

- `variant_matches`：改为按 `target_dialect` 定位方言侧（`target_dialect` 已存在），
  id 相等作快速路径；
- `form.form_type != target_form_type`：**去掉硬拒绝**，改为返回「成立但类型漂移」标记。

签名调整：返回值从 `bool` 改为枚举，或额外带出 `form_type_drift: bool`。调用方
（`inbound_references::evaluate`）据此决定是 pass / confirm。

### 3.3 结构漂移时的方言落点

common→uk_us 时，`Dialect::Common` 的引用要落到 uk 或 us：

- 有来源方言（例句标注 `source_dialect` 或成分 `target_dialect`）→ 落到对应侧；
- 无来源方言（纯 common）→ 落到 **preference**（前端 `preferredDialect`，默认 us）；
- 判定时 uk / us **任一侧拼写匹配即成立**（放宽，避免因偏好不同误判 stale）。

uk_us→common 时，`Dialect::Uk` / `Dialect::Us` 引用落到 common 侧，拼写按 common 匹配。

### 3.4 契约变更：`TextLinkV3` 补 `target_dialect`

例句引用需要 target 侧方言才能在结构漂移后重定位。当前只有标注 `source_dialect`。

```rust
// dto/v3.rs TextLinkV3 新增（可选，向后兼容）
#[serde(default, skip_serializing_if = "Option::is_none")]
pub target_dialect: Option<Dialect>,
```

- 旧数据缺省 → 判定退化为「来源方言优先 + 任一侧匹配」的宽容策略，不破坏存量；
- 保存/发布时服务端按当前草稿补上（同 `target_publication_id` 的补写模式）；
- OpenAPI 需重新生成（`docs/openapi.json`）。

### 3.5 词形类型漂移：提示而非确认流（实施时降级，见 §0 订正 1）

**未做后端确认 token**。需求口径为「保护式放开」，「改类型会让 N 处引用失效」属于提示，不属于阻断：

- 后端：`phrase_component_matches_target` 去掉 `form_type` 硬拒绝，类型漂移后引用仍成立；
  `target_content_gloss` 的 base 校验只保留「base 词形存在 + 同组」，不再要求当前仍是 `base` 类型。
- 前端：`V3FormGroupCard` 生成 `formTypeChangeHint`（「修改词形类型会让 N 处引用漂移，保存后请核对」），
  经 `V3ConcreteFormRow` → `V3ConcreteFormTypeCell` 展示在类型下拉下方；不阻断 `disabled`。

若后续产品要求「必须确认」才可保存，再按原方案补确认 token（`confirmed_*` 模式）。

## 4. 前端改动

| 文件 | 改动 |
|---|---|
| `referenceGuard.ts:208` `dialectRuleLocks` | 去掉 `split`/`merge` 对开关的阻断（改为仅展示徽标计数） |
| `V3PosTab.tsx:151` | 移除 `splitHint` / `mergeHint` 对 `disabled` 的作用 |
| `V3FormGroupCard.tsx:264` | `formTypeDisabled` 去掉 `formReferences > 0`；`deleteLocked` **保留** |
| `referenceGuard.ts:150` `formReferenceCount` | 拆分为「改类型用」（不阻断）与「删除用」（阻断）两个口径 |
| 类型改动的确认交互 | 保存 409 可确认冲突 → 弹确认框 → 带 token 重放 |

**保留**：拼写一致性（`spellingConflicts`）、发音/规则/成分合并冲突——这些仍硬拦。

## 5. 兼容与迁移

- **零迁移优先**：判定双读（id 快速路径 + 语义坐标）兼容全部存量引用；
- 仅当双读无法覆盖某历史形态时，再补一次性 SQL 修 `target_variant_id` → 语义坐标；
- 已发布快照冻结，判定不得回写历史快照；
- 前端先部署（若新增错误码，顺序同 `draft-visibility-write-guard` §4：后端生成 openapi →
  前端 sync → 前端先部署 → 后端部署）。

## 6. 验收命令

后端：

```bash
cd tsz-rust-task-58
cargo test --test lexicon_handler 2>&1 | tail -30
cargo test 2>&1 | tail -20
cargo clippy --all-targets -- -D warnings 2>&1 | tail -20
```

预期：新增引用解锁用例全绿；既有引用保护用例不回归；clippy 零警告。

前端：

```bash
cd tsz
pnpm --filter @tsz/admin test referenceGuard V3PosTab V3FormGroupCard 2>&1 | tail -30
pnpm typecheck && pnpm lint
```

人工验收（截图场景复现）：

1. 建词条，某词形被 1 处例句标注引用；
2. 进词形步，切「英美拼写是否有区别」为「是」→ 开关应可点，不再置灰；
3. 保存成功，回例句看引用仍指向正确侧；
4. 改该词形类型（原形→过去式）→ 弹出「会让 1 处引用失效」确认；
5. 确认后保存成功；点击删除 → 仍禁用并提示被引用。

## 7. 实施顺序（已全部完成）

1. ✅ 后端：判定改造（§3.1、§3.2）+ 单测（6 个漂移用例）；
2. ✅ 后端：契约补字段（§3.4）；§3.5 降级为前端提示（见 §0 订正 1）；
3. ✅ 后端：`docs/openapi.json` 重新生成；
4. ✅ 前端：sync + 解锁 UI + 漂移提示 + 测试更新；
5. ⏳ 两仓全绿后按 §5 顺序部署（**待用户授权**）。

## 8. 仍待人工验收

自动化测试覆盖了判定与 UI 开关行为，但以下需真实环境人工确认（见 requirements §5 截图场景）：

1. 建词条 → 某词形被 1 处例句标注引用 → 切「英美拼写是否有区别」→ 开关可点、保存成功、引用仍指向正确侧；
2. 改该词形类型 → 类型下拉可点且给漂移提示 → 保存成功；
3. 点删除该词形 → 仍禁用并提示被引用。

