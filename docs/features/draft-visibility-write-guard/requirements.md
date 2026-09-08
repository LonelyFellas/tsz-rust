# 草稿可见性放开 + 草稿写权限收口

状态：**已实施（2026-09-08）**，尚未部署。评估基线 tsz-rust `b9c2faa`、tsz `d7210a5`。

落点：
- 后端写侧守卫 §3.1 → tsz-rust `ef99ba1`
- 前端只读态 §3.4 → tsz `29cca03`
- 撞名检测放开 §3.3 → tsz-rust `901020b`

§4 的部署顺序仍然有效且尚未执行：`entry_edit_forbidden` 是新错误码，**前端必须先部署**。

实施中与本文的两处出入（都是实测后的订正，不是遗漏）：
- §3.1 原写「守卫放在幂等键消费之前」。实际幂等键的登记发生在事务提交时，守卫返回 Err
  即整体回滚，键自然不会被消费——验收 #5 已按此断言并通过，无需额外调整顺序。
- §3.3 的判别力落在**词形步**而不是发布期：词形步 acknowledge 过一次之后，发布期不再
  重复要求确认。这是既有的确认语义，测试断言已相应下移。
- §2.2 写「V2 侧 surface-match 维持现状」**没有做到**：入站关系那条 SQL 是 V2/V3 共用的，
  放开 V3 就等于放开 V2。V2 候选 SQL（`SURFACE_SOURCES_QUERY`）本来就不按创建者过滤，
  所以结果是 V2 侧「草稿词面可见、草稿关系不可见」的旧不对称消失了——方向与新口径一致，
  且生产无 V2 数据。要保住那条不对称就得给共用 SQL 加分叉参数，不值得。

另有一处本文没预料到的坑，已在实现中处理：前端不能用 `status === "draft"` 判断「未发布」，
归档优先于发布态，归档了的草稿状态是 `archived`。判定改用 `published_revision` 是否缺省，
与后端 `current_publication_id IS NULL` 同口径。

## 1. 产品口径（2026-09-08 用户定盘）

> 草稿的词条**能**让其他管理员看到，但**不能**让其他普通管理员操作。
> 草稿状态只是「相对 C 端未发布」，在 admin 内部不是私密内容。
> **超管不受任何限制。**

这条口径**推翻**了 2026-09-01 定下的「未发布草稿只对创建者可见（超管不豁免）」——
但只推翻「看见」那一半，「引用」那一半按下面 §2 保留。上游文档
`tsz-core/docs/surface-match-draft-visibility.md`、
`tsz-core/docs/relation-prebinding-field-unification.md` §4.5/§4.6 的表述需随实施更新
（那两份不在版本控制内，属本地工作笔记）。

### 四项配套拍板

| # | 问题 | 决定 |
|---|---|---|
| 1 | 「不能操作」管草稿还是所有词条 | **只收草稿**。已发布词条（含带未发布修改的）仍全员可编辑 |
| 2 | 关联词搜索 / 例句目标发现要不要搜得到别人的草稿 | **不要**。草稿只能被看见，不能被直接引用 |
| 3 | 成分目标搜索（短语成分） | 维持只搜已发布，本次不动 |
| 4 | 建条向导 step-1 撞名检测 | **跟着放开**，见 §3.3 |

## 2. 范围边界

### 2.1 已经满足、无需改动

- 词条管理列表 `GET /admin/lexicon/entries`：本就无 actor 过滤，且返回
  `created_by` / `created_by_name`（`repository/query.rs:340`）。
- 词条详情 `GET /entries/{id}`：`get_draft_any(id)` 不带 actor，本就全员可见。

### 2.2 明确不改（防止实施时扩大范围）

- **引用类入口维持现有的创建者过滤**：
  - `related-search` 草稿分支（`repository/query.rs:108`）
  - `sentence-targets/resolve`（`repository/sentence_target_discovery.rs:152`）
  - 相应测试 `draft_candidates_are_visible_only_to_their_creator`（`tests/lexicon_handler.rs:13215`）
    **保留不动**，只把注释里「只对创建者可见」改写成「只对创建者**可引用**」，
    避免下次 review 依据旧措辞误判。
- 成分目标 `component-targets/search`：维持只搜已发布。
- V2 侧 surface-match：维持现状（2026-09-01 已拍板 V2 不做，生产无 V2 数据包袱）。
- 词面唯一 / 同名绑定 `bind-existing`：维持跨管理员可用（词面唯一、同名即同词，
  拦掉会让后建者进死路）。
- 归档词条不进任何候选：维持。

## 3. 要做的改动

### 3.1 后端写侧：草稿态归属守卫

判定式（**只在从未发布的草稿上生效**）：

```
record.current_publication_id.is_none()
  && !is_super_admin
  && record.created_by_admin_id != actor_id
  → 403
```

抽一个 helper（放 `service/helpers.rs` 或 `service/lifecycle.rs` 邻近处），接入点全部
已在事务里通过 `entry_by_id_for_update` 持有 record，`created_by_admin_id` 与
`current_publication_id` 都在手上：

| 服务位置 | 端点 | 备注 |
|---|---|---|
| `service/editing.rs:175` | `PUT /entries/{id}/steps/forms`（V2） | handler 需加传 `is_super_admin` |
| `service/editing.rs:653` | `PUT /entries/{id}/steps/meanings`（V2） | 同上 |
| `service/v3.rs:1633` | `save_forms_v3` | 同上 |
| `service/v3.rs:1937` | `save_meanings_v3` | 同上 |
| `service/publishing.rs:106` | `POST /entries/{id}/publications` | 同上 |
| `service/lifecycle.rs:305` | `POST /entries/{id}/archive` | 同上 |
| `service/lifecycle.rs:330` | `POST /entries/{id}/restore` | 同上 |
| `service/lifecycle.rs:461` | `archive-batch` / `restore-batch` | 整批原子拒绝 |
| `content_completion` | `POST /entries/{id}/content-completion-jobs` | AI 内容当前未启用，顺手接上 |

**不需要改**：

- `DELETE /entries/{id}` 与 `delete-batch`：已有归属校验（`service/lifecycle.rs:254`），
  且它对**所有**词条生效（比新口径更严）——这是既有的删除保护，不要放宽。
- `.../publications/{pid}/activate`：只对已有 publication 的词条可达，草稿态不适用。
- `PATCH /entries/{id}/annotation`：已有校验，见 §5 遗留项。

**批量语义**：一批里混进别人的草稿 → 整批 403（与现有 `delete_batch` 一致）。守卫必须
在幂等键消费**之前**，避免「请求被拒但幂等键已被吃掉」。

**新错误码**：`entry_edit_forbidden`（若按操作细分则另议）。加进 `error.rs` 的
`ErrorCode`，并在受影响端点的 utoipa 注解里补 403 描述。

### 3.2 后端读侧

一行 SQL 都不改（见 §2.1、§2.2）。唯一动作是订正注释措辞。

### 3.3 后端 surface-match：撞名检测放开（新增范围）

现状（2026-09-01 收口后的实测行为，见 `tsz-core/docs/surface-match-draft-visibility.md` §3）：

- B 建档撞上 A 的 V3 草稿（已有词形）→ **无警告直接建档**，两份同名草稿静默共存
- B 撞上 A 的 V3 **无词形**草稿 → `reject_hidden` 硬 409，**且不给原因**
- B 发布词形与 A 草稿词形同面 → 不再要求 acknowledge

新口径下这三条都要回到「能看见」的行为：

- V3 候选材料 SQL：`service/v3_surface.rs:1772` `v3_surface_material_in` 的 draft 分支去掉
  actor 过滤，恢复 acknowledge 确认流。
- 入站关系 `SURFACE_INBOUND_RELATIONS_QUERY`（`repository/surfaces.rs:98`，V2/V3 共用）：
  该查询的 draft 分支只对源词条创建者可见，且第二分支的去重条件
  （`source_entry.created_by_admin_id <> $2`）是为配合第一分支的过滤而写的。放开时
  **两个分支要一起改**，否则会出现「草稿行放开了、发布行被去重吞掉」或反过来的两头落空。
  实施前先补一个断言把这条去重不变量钉住。
- `reject_hidden` 409：改为给出真实原因（撞上一个未发布草稿），而不是掩盖存在性。
- V2 侧不动。

这块的测试断言在 `tests/lexicon_handler.rs` 与 surface 相关用例里，需按 §3 撞名矩阵**反向**
更新——注意这是本次唯一需要反转既有断言的地方。

### 3.4 前端 admin

数据齐备，**后端无需为前端新增任何字段**：`AdminWordV3.created_by`
（`packages/types/src/admin-word-v3.ts:664`）、`AdminWordListItemAny.created_by`、
`useAuthStore.profile`（含 `id` / `role`）都是现成的。

| 文件 | 改动 |
|---|---|
| `word-creation-v3/stepAccess.ts:25` | `resolveV3StepAccess` 的 `readOnly` 加一条：`status === "draft"` 且非本人且非超管。编辑器 `readOnly` 链路（归档态已在用）现成，落到 preview 只读 |
| `wordRouting.ts:44` | 别人的草稿：行动作文案「继续创建」→「查看」 |
| 新增 `entryWritePermission.ts` | 照 `deletePermission.ts` 的套路（含批量分区 `partitionXxx`）。注意该文件顶部注释的定性——**这不是权限，是让管理员点击前就知道结果**，后端才是权威 |
| `SmartDictionary.tsx:861` | 「移入垃圾桶 / 恢 复」按钮对别人的草稿置灰 + Tooltip 给出理由（照永久删除按钮的写法） |
| 批量工具条 | 提交前分区拦截，把不合格条目挑明（照永久删除批量的写法） |
| 403 文案分流 | 照 `annotationPermission.ts` 的 `annotationForbiddenMessage` |

`stepAccess.ts` 需要新增入参（actor + 词条 created_by），它现在是纯函数且被多处调用，
改签名时把调用点一并过一遍。

## 4. 契约与部署顺序（硬约束）

错误码在前端是 **enum**，写死在 `packages/api-client/src/admin-word-v3.runtime-schema.json`
与 `openapi.snapshot.json` 里，前端做严格校验。因此：

1. 后端分支实现新 code → 生成 `docs/openapi.json`
2. 前端 `pnpm --filter @tsz/api-client sync:openapi` 收下新 code，**前端先部署**（或两边同批）
3. 后端再部署

顺序反了的话，新的 403 会在前端变成解析错误，比不做还糟。参见既往教训：
2026-09-02 曾因此连踩两次。

## 5. 遗留待定

标注（annotation）目前对**所有**词条限归属（超管或创建者，2026-09-07 定），与本次
「已发布词条全员可编辑」不一致，会出现「别人已发布的词条，正文能改、标注改不了」。

评估建议**本次不动**（标注是元数据不是内容，更严不违反新口径），用户未反对。
实施时保持现状，不要顺手改。

## 6. 验收标准

后端（集成测试，`tests/lexicon_handler.rs`）：

1. 普通管理员 B 对 A 的**草稿**执行 save_forms / save_meanings / publish / archive /
   restore → 403 `entry_edit_forbidden`；批量入口混入一条 → 整批拒绝且数据零变更。
2. 超管对同样的目标执行以上操作 → 全部成功。
3. B 对 A 的**已发布**词条执行同样操作 → 成功（口径 #1 的反向断言，防收过头）。
4. B 对**自己**的草稿 → 成功。
5. 被拒的批量请求，其幂等键未被消费（同键重放仍走正常校验）。
6. `related-search` / `sentence-targets/resolve` 里 B 仍搜不到 A 的草稿
   （既有断言保持绿）。
7. 撞名检测：B 建档撞 A 的草稿 → 拿到警告材料并可 acknowledge；撞无词形草稿的 409
   带出真实原因。

前端（Vitest）：

8. 别人的草稿行：入口文案为「查看」，进入后编辑器处于只读态，保存类按钮不可用。
9. 别人的草稿行：「移入垃圾桶」置灰且 Tooltip 给出理由；批量选中时被分区挑出。
10. 自己的草稿、以及任何已发布词条：行为不变（防误伤）。

两仓 `pnpm test` / `cargo test` 全绿，`pnpm typecheck` / `pnpm lint` 全绿。

## 7. 实施顺序建议

1. 后端：先写守卫 + 测试（§3.1、§6.1-5），这是需求主体，独立可验收。
2. 后端：surface-match 放开（§3.3），改动面独立，可拆第二个 PR。
3. 前端：sync:openapi + UI（§3.4、§6.8-10）。
4. 部署按 §4 的顺序。
