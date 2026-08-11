# 词性配置后端设计

> 状态：首期已实现（catalog migration、九个管理接口与 OpenAPI 已落地）。智能词库尚未落地，
> 因此 publication 引用保护和真实 usage 聚合仍按 §10 留待 lexicon 域实现。
>
> 本模块只负责管理基本词性和细分词性目录，不包含智能词库词条、内置词典映射、发布流程、
> TTS 或搜索能力。最终接口契约以 `docs/openapi.json` 为准。

## 1. 目标与范围

管理后台“系统设置 → 词性配置”需要维护两级目录：

- 基本词性，例如 `noun`、`verb`；
- 细分词性，例如 `N-COUNT`、`V-T`，每项必须归属一个基本词性。

首期能力：

- 所有状态正常的管理员可以读取完整 catalog；
- 只有 `super_admin` 可以查看管理列表以及新增、修改、删除配置；
- 编码创建后不可修改；
- 中文名、英文名、缩写和排序可以修改；
- 被词条引用的配置不能删除；
- catalog 内容变化后版本号单调递增；
- 不使用 Redis、Worker、Outbox 或独立缓存。

“被词条引用”同时包括当前草稿关系表和仍保留的不可变 publication。不能只检查当前草稿，
否则管理员从新草稿移除词性后，会允许删除当前线上版本仍在使用的目录项。publication 引用模型
见 §10。

## 2. 数据库结构

新建 PostgreSQL schema：

```sql
CREATE SCHEMA catalog;
```

catalog 模块自身只需要三张表；未来保护 publication 的引用表归 lexicon 域，见 §10。

### 2.1 `catalog.metadata`

保存完整 catalog 的全局版本号。

| 字段         | 类型        | 约束                                           |
| ------------ | ----------- | ---------------------------------------------- |
| `id`         | BOOLEAN     | PK，固定为 `TRUE`，使用 CHECK 阻止插入 `FALSE` |
| `version`    | BIGINT      | NOT NULL，默认 1，必须大于 0                   |
| `updated_at` | TIMESTAMPTZ | NOT NULL，默认 `now()`                         |

初始化时插入一行：

```sql
CREATE TABLE catalog.metadata (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id IS TRUE),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO catalog.metadata (id, version) VALUES (TRUE, 1);
```

任何基本词性或细分词性的新增、修改、删除，都必须在同一事务中执行：

```sql
UPDATE catalog.metadata
SET version = version + 1, updated_at = now()
WHERE id = TRUE;
```

### 2.2 `catalog.parts_of_speech`

| 字段                  | 类型        | 约束与说明                                                 |
| --------------------- | ----------- | ---------------------------------------------------------- |
| `id`                  | UUID        | PK，服务端生成 UUID v7                                     |
| `code`                | TEXT        | NOT NULL，由固定名字的唯一索引保证唯一，创建后不可修改     |
| `name_zh`             | TEXT        | NOT NULL                                                   |
| `name_en`             | TEXT        | NOT NULL                                                   |
| `abbreviation`        | TEXT        | NOT NULL                                                   |
| `sort_order`          | INTEGER     | NOT NULL，默认 0                                           |
| `revision`            | BIGINT      | NOT NULL，默认 1，必须大于 0                               |
| `created_by_admin_id` | UUID        | NULL，FK `admins.id` `ON DELETE RESTRICT`；系统种子为 NULL |
| `updated_by_admin_id` | UUID        | NULL，FK `admins.id` `ON DELETE RESTRICT`                  |
| `created_at`          | TIMESTAMPTZ | NOT NULL，默认 `now()`                                     |
| `updated_at`          | TIMESTAMPTZ | NOT NULL，默认 `now()`                                     |

校验：

- `code`：`^[a-z][a-z0-9_]{0,31}$`，数据库 CHECK 与 Rust 校验同时执行；
- `name_zh`、`name_en`：服务端 trim 后长度 1–64；数据库 CHECK 同时保证已 trim 且长度合法；
- `abbreviation`：服务端 trim 后长度 1–16；数据库 CHECK 同时保证已 trim 且长度合法；
- `code` 全局唯一；
- `name_zh` 全局唯一；
- `name_en` 忽略大小写后全局唯一；
- `abbreviation` 忽略大小写后全局唯一；
- `sort_order` 接受完整的 PostgreSQL `INTEGER`（有符号 32 位整数）值域并允许重复；读取时按
  `sort_order, created_at, id` 稳定排序。负数用于把项目排到默认项之前，不属于业务校验错误。

以下唯一索引和普通索引的名字属于数据库错误映射契约，migration 必须使用这些固定名字，不能
依赖 PostgreSQL 根据匿名 `UNIQUE` 自动生成名字：

```sql
CREATE UNIQUE INDEX catalog_parts_of_speech_code_unique_idx
ON catalog.parts_of_speech (code);

CREATE UNIQUE INDEX catalog_parts_of_speech_name_zh_unique_idx
ON catalog.parts_of_speech (name_zh);

CREATE UNIQUE INDEX catalog_parts_of_speech_name_en_unique_idx
ON catalog.parts_of_speech (lower(name_en));

CREATE UNIQUE INDEX catalog_parts_of_speech_abbreviation_unique_idx
ON catalog.parts_of_speech (lower(abbreviation));

CREATE INDEX catalog_parts_of_speech_order_idx
ON catalog.parts_of_speech (sort_order, created_at, id);
```

建表时至少包含以下 CHECK，不能只在 Service 校验：

```sql
CHECK (code ~ '^[a-z][a-z0-9_]{0,31}$'),
CHECK (name_zh = btrim(name_zh) AND char_length(name_zh) BETWEEN 1 AND 64),
CHECK (name_en = btrim(name_en) AND char_length(name_en) BETWEEN 1 AND 64),
CHECK (
    abbreviation = btrim(abbreviation)
    AND char_length(abbreviation) BETWEEN 1 AND 16
),
CHECK (revision > 0)
```

### 2.3 `catalog.sub_parts_of_speech`

| 字段                  | 类型        | 约束与说明                                                 |
| --------------------- | ----------- | ---------------------------------------------------------- |
| `id`                  | UUID        | PK，服务端生成 UUID v7                                     |
| `part_of_speech_id`   | UUID        | NOT NULL，FK `catalog.parts_of_speech(id)`                 |
| `code`                | TEXT        | NOT NULL，由固定名字的唯一索引保证唯一，创建后不可修改     |
| `name_zh`             | TEXT        | NOT NULL                                                   |
| `name_en`             | TEXT        | NOT NULL                                                   |
| `sort_order`          | INTEGER     | NOT NULL，默认 0                                           |
| `revision`            | BIGINT      | NOT NULL，默认 1，必须大于 0                               |
| `created_by_admin_id` | UUID        | NULL，FK `admins.id` `ON DELETE RESTRICT`；系统种子为 NULL |
| `updated_by_admin_id` | UUID        | NULL，FK `admins.id` `ON DELETE RESTRICT`                  |
| `created_at`          | TIMESTAMPTZ | NOT NULL，默认 `now()`                                     |
| `updated_at`          | TIMESTAMPTZ | NOT NULL，默认 `now()`                                     |

父级外键使用：

```sql
REFERENCES catalog.parts_of_speech(id) ON DELETE CASCADE
```

只有未被词条引用的基本词性才允许删除，因此父级删除成功后可以安全级联删除其细分词性。
未来智能词库引用本表时使用 `ON DELETE RESTRICT`。

校验：

- `code`：`^[A-Z][A-Z0-9_-]{0,31}$`，数据库 CHECK 与 Rust 校验同时执行；
- `name_zh`、`name_en`：服务端 trim 后长度 1–64；数据库 CHECK 同时保证已 trim 且长度合法；
- `code` 全局唯一；
- 同一基本词性下 `name_zh` 唯一；
- 同一基本词性下 `name_en` 忽略大小写唯一；
- `sort_order` 接受完整的 PostgreSQL `INTEGER`（有符号 32 位整数）值域并允许重复；读取时按
  `sort_order, created_at, id` 稳定排序。

以下索引名同样固定：

```sql
CREATE UNIQUE INDEX catalog_sub_parts_code_unique_idx
ON catalog.sub_parts_of_speech (code);

CREATE UNIQUE INDEX catalog_sub_parts_name_zh_unique_idx
ON catalog.sub_parts_of_speech (part_of_speech_id, name_zh);

CREATE UNIQUE INDEX catalog_sub_parts_name_en_unique_idx
ON catalog.sub_parts_of_speech (part_of_speech_id, lower(name_en));

CREATE INDEX catalog_sub_parts_order_idx
ON catalog.sub_parts_of_speech (part_of_speech_id, sort_order, created_at, id);
```

建表时至少包含以下 CHECK：

```sql
CHECK (code ~ '^[A-Z][A-Z0-9_-]{0,31}$'),
CHECK (name_zh = btrim(name_zh) AND char_length(name_zh) BETWEEN 1 AND 64),
CHECK (name_en = btrim(name_en) AND char_length(name_en) BETWEEN 1 AND 64),
CHECK (revision > 0)
```

## 3. 默认数据

migration 中初始化当前前端 mock 使用的 11 个基本词性。英文名大小写和排序值也是初始展示契约，
不得在后端自行改成 Title Case：

| code           | 中文名 | 英文名       | 缩写  | sort_order |
| -------------- | ------ | ------------ | ----- | ---------: |
| `noun`         | 名词   | NOUN         | n.    |         10 |
| `pronoun`      | 代词   | PRONOUN      | pron. |         20 |
| `verb`         | 动词   | VERB         | v.    |         30 |
| `adjective`    | 形容词 | ADJECTIVE    | adj.  |         40 |
| `adverb`       | 副词   | ADVERB       | adv.  |         50 |
| `preposition`  | 介词   | PREPOSITION  | prep. |         60 |
| `article`      | 冠词   | ARTICLE      | art.  |         70 |
| `determiner`   | 限定词 | DETERMINER   | det.  |         80 |
| `conjunction`  | 连词   | CONJUNCTION  | conj. |         90 |
| `numeral`      | 数词   | NUMERAL      | num.  |        100 |
| `interjection` | 感叹词 | INTERJECTION | int.  |        110 |

同时初始化当前 19 个细分词性，完整值与前端 fixture 一致：

| 基本词性       | code        | 中文名     | 英文名            | sort_order |
| -------------- | ----------- | ---------- | ----------------- | ---------: |
| `verb`         | `V-T`       | 及物动词   | Transitive verb   |         10 |
| `verb`         | `V-I`       | 不及物动词 | Intransitive verb |         20 |
| `verb`         | `V-LINK`    | 系动词     | Linking verb      |         30 |
| `verb`         | `AUX`       | 助动词     | Auxiliary verb    |         40 |
| `verb`         | `MODAL`     | 情态动词   | Modal verb        |         50 |
| `adjective`    | `ADJ`       | 形容词     | Adjective         |         60 |
| `adverb`       | `ADV`       | 副词       | Adverb            |         70 |
| `noun`         | `N-COUNT`   | 可数名词   | Countable noun    |         80 |
| `noun`         | `N-UNCOUNT` | 不可数名词 | Uncountable noun  |         90 |
| `noun`         | `N-PROPER`  | 专有名词   | Proper noun       |        100 |
| `noun`         | `N-PLURAL`  | 复数名词   | Plural noun       |        110 |
| `noun`         | `N-SING`    | 单数名词   | Singular noun     |        120 |
| `pronoun`      | `PRON`      | 代词       | Pronoun           |        130 |
| `preposition`  | `PREP`      | 介词       | Preposition       |        140 |
| `conjunction`  | `CONJ`      | 连词       | Conjunction       |        150 |
| `determiner`   | `DET`       | 限定词     | Determiner        |        160 |
| `article`      | `ART`       | 冠词       | Article           |        170 |
| `numeral`      | `NUM`       | 数词       | Numeral           |        180 |
| `interjection` | `INT`       | 感叹词     | Interjection      |        190 |

种子数据的 `created_by_admin_id` 为 NULL。API 映射为：

```json
{ "id": "system", "display_name": "系统" }
```

运行时 ID 由 Rust `Uuid::now_v7()` 生成。当前开发数据库是 PostgreSQL 16，migration 不得依赖
数据库内不存在的 UUID v7 函数；种子 ID 使用在编写 migration 时预生成并提交的固定 UUID v7
字面量，保证所有环境完全一致。

## 4. Rust 模块结构

模块保持简单：

```text
src/catalog/
├── mod.rs
├── model.rs
├── repository.rs
├── service.rs
├── handler.rs
└── router.rs
```

职责：

- `model.rs`：请求、响应、SQL Row 和校验值对象；
- `repository.rs`：SQL 查询和事务内 CRUD；
- `service.rs`：权限之外的业务规则、revision、删除引用检查；
- `handler.rs`：管理员鉴权、参数解析、错误映射；
- `router.rs`：挂载 `/api/v1/admin/settings/parts-of-speech`。

首期不需要 Provider trait、缓存层、Worker 或事件消费者。

现有 `require_active_admin` / `require_super_admin` 位于 accounts handler 私有函数，catalog
模块不能直接复用。实现本模块前先将两者抽到 `src/admin/authorization.rs`（或等价共享模块），
继续按 `subject` 回库核对管理员的最新状态、`must_change_password` 和角色。不得只相信 access
token 中可能已经过期的 role claim。

## 5. 权限

沿用现有 admin 身份域：

| 操作                     | 权限                               |
| ------------------------ | ---------------------------------- |
| 读取完整 catalog         | 任意状态正常且无需强制改密的管理员 |
| 管理分页列表             | `super_admin`                      |
| 新增、修改、删除基本词性 | `super_admin`                      |
| 新增、修改、删除细分词性 | `super_admin`                      |

Handler 复用抽取后的 active admin / super admin 守卫，不新建权限系统。

## 6. API

### 6.1 读取完整 catalog

```http
GET /api/v1/admin/settings/parts-of-speech/catalog
```

返回全部基本词性和嵌套细分词性，不分页，不返回 usage 和审计详情。成功状态为 200：

```json
{
  "catalog_version": 12,
  "items": [
    {
      "id": "019f...",
      "code": "noun",
      "name_zh": "名词",
      "name_en": "NOUN",
      "abbreviation": "n.",
      "sort_order": 10,
      "sub_parts": [
        {
          "id": "019f...",
          "code": "N-COUNT",
          "name_zh": "可数名词",
          "name_en": "Countable noun",
          "sort_order": 10
        }
      ]
    }
  ]
}
```

数据量很小，每次直接查询 PostgreSQL。`catalog_version` 是不透明的单调变化标识；当前前端
仍使用 TanStack Query 的 5 分钟 `staleTime`，并在本机 mutation 成功后失效缓存，并未主动
轮询或比较版本。后续若要跨管理员立即刷新，可基于该值增加 ETag/条件请求，不能在文档中假定
前端已经实现。

`catalog_version` 和 `items` 必须来自同一个 PostgreSQL MVCC 快照，不能先后使用两个普通查询
拼接响应。Repository 应优先使用一条查询读取 metadata、基本词性和细分词性；如果拆成多条查询，
必须放进同一个 `REPEATABLE READ READ ONLY` 事务。这样并发写入时不会返回“旧 version + 新 items”
或“新 version + 旧 items”。

服务端已按 `sort_order, created_at, id` 排好顺序。catalog DTO 不包含 `created_at`，客户端必须
保留服务端对相同 `sort_order` 项的相对顺序，不能再按 `id` 重排并覆盖服务端顺序。

### 6.2 基本词性管理

```text
GET    /api/v1/admin/settings/parts-of-speech
POST   /api/v1/admin/settings/parts-of-speech
PATCH  /api/v1/admin/settings/parts-of-speech/{id}
DELETE /api/v1/admin/settings/parts-of-speech/{id}?base_revision={base_revision}
```

列表查询：

- `q`：trim 后做忽略大小写的字面子串匹配，覆盖 code、中文名、英文名、缩写；`%`、`_`
  等字符不作为 SQL 通配符；
- `page`：默认 1；
- `page_size`：默认 10，范围 1–100。

非法分页参数返回 400 `invalid_query`，不静默 clamp。成功状态为 200，响应固定使用后台分页
信封：

```json
{
  "items": [],
  "pagination": {
    "page": 1,
    "page_size": 10,
    "total": 0,
    "total_pages": 0
  }
}
```

创建请求：

```json
{
  "code": "particle",
  "name_zh": "小品词",
  "name_en": "Particle",
  "abbreviation": "part.",
  "sort_order": 120
}
```

修改请求：

```json
{
  "base_revision": 3,
  "name_zh": "小品词",
  "name_en": "Particle",
  "abbreviation": "part.",
  "sort_order": 120
}
```

PATCH 的 `base_revision` 必须为正整数；缺失或类型错误属于 422 `invalid_request_body`，值小于 1
属于 400 `invalid_part_of_speech`，顶层 `field` 为 `base_revision`。

POST 成功返回 201 和完整 `PartOfSpeechConfig`；PATCH 成功返回 200，revision 加一并返回完整
新记录。DELETE 必须携带当前记录的正整数 `base_revision` 查询参数，成功返回 204 空 body；缺失、
类型错误或小于 1 返回 400 `invalid_query`，过期 revision 返回 409，不能删除其他管理员刚修改过
的配置。

PATCH 不接受 `code`。所有写 DTO 使用 `#[serde(deny_unknown_fields)]`（或等价严格解析），因此
PATCH 携带 `code`、创建人、usage 等只读/未知字段时返回 422 `invalid_request_body`，不能静默
忽略。语法正确但字段值违反 code 或字符串长度规则时返回 400 `invalid_part_of_speech`。

### 6.3 细分词性管理

```text
GET    /api/v1/admin/settings/parts-of-speech/{id}/sub-parts
POST   /api/v1/admin/settings/parts-of-speech/{id}/sub-parts
PATCH  /api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}
DELETE /api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}?base_revision={base_revision}
```

细分词性数量较少，GET 不分页。成功状态为 200，响应必须使用前端已经发布的 items 信封，
不能直接返回裸数组：

```json
{
  "items": []
}
```

创建请求：

```json
{
  "code": "N-COLLECTIVE",
  "name_zh": "集合名词",
  "name_en": "Collective noun",
  "sort_order": 60
}
```

修改请求同样带 `base_revision`，并且不接受 `code` 或 `part_of_speech_id`，首期不支持移动到
另一个基本词性。

POST 成功返回 201 和完整 `SubPartOfSpeechConfig`；PATCH 成功返回 200 和新 revision 的完整
记录。DELETE 同样要求正整数 `base_revision` 查询参数，成功返回 204 空 body。细分词性写 DTO 同样
严格拒绝未知字段。

所有 `{id}` / `{sub_id}` 都是 UUID。Handler 必须使用统一 `ApiPath<T>`（或行为完全等价的共享
提取器）把路径反序列化失败转换为 400 `invalid_path_parameter` Problem Details；不得直接让
Axum `Path<Uuid>` rejection 返回默认文本。能够确定具体参数时，顶层 `field` 分别为 `id` 或
`sub_id`。

### 6.4 管理记录 wire

管理接口完整记录固定包含以下字段，字段名保持 snake_case：

```text
PartOfSpeechConfig
  id, code, name_zh, name_en, abbreviation, sort_order,
  usage_count, sub_part_count, revision,
  created_by, created_at, updated_by?, updated_at

SubPartOfSpeechConfig
  id, part_of_speech_id, code, name_zh, name_en, sort_order,
  usage_count, revision,
  created_by, created_at, updated_by?, updated_at

Actor
  id, display_name
```

时间统一输出 RFC 3339。`created_by` 必须存在；系统种子使用 §3 的 system actor。记录尚未修改时
省略 `updated_by`，不输出 `null`；`updated_at` 仍等于创建时间。创建和更新响应与随后 GET
读出的记录必须同形。由于 system actor 的 `id` 是字面量 `"system"`，HTTP Actor DTO 的 `id`
类型必须是 String；数据库 Row 仍保留 `Option<Uuid>`，不能直接把 SQL Row 序列化成 wire。

### 6.5 九个端点的成功契约

| 方法   | 路径                                                                                      | 状态 | 响应                           |
| ------ | ----------------------------------------------------------------------------------------- | ---: | ------------------------------ |
| GET    | `/api/v1/admin/settings/parts-of-speech/catalog`                                          |  200 | `{ catalog_version, items }`   |
| GET    | `/api/v1/admin/settings/parts-of-speech`                                                  |  200 | `{ items, pagination }`        |
| POST   | `/api/v1/admin/settings/parts-of-speech`                                                  |  201 | 完整 `PartOfSpeechConfig`      |
| PATCH  | `/api/v1/admin/settings/parts-of-speech/{id}`                                             |  200 | 完整新 `PartOfSpeechConfig`    |
| DELETE | `/api/v1/admin/settings/parts-of-speech/{id}?base_revision={revision}`                    |  204 | 无 body                        |
| GET    | `/api/v1/admin/settings/parts-of-speech/{id}/sub-parts`                                   |  200 | `{ items }`                    |
| POST   | `/api/v1/admin/settings/parts-of-speech/{id}/sub-parts`                                   |  201 | 完整 `SubPartOfSpeechConfig`   |
| PATCH  | `/api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}`                          |  200 | 完整新 `SubPartOfSpeechConfig` |
| DELETE | `/api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}?base_revision={revision}` |  204 | 无 body                        |

204 响应不得返回 `{}`、`null` 或其他 JSON。上述状态码、信封及 DTO 字段均属于 OpenAPI 和前端
contract test 的稳定契约。

## 7. 响应派生字段

管理列表需要以下派生字段，但不落库：

- 基本词性的 `sub_part_count`：聚合细分词性数量；
- 基本词性的 `usage_count`：当前草稿或任一仍保留 publication 中使用该词性的 distinct
  `entry_id` 数量，同一词条多个 publication 不能重复计数；
- 细分词性的 `usage_count`：当前草稿或任一仍保留 publication 中使用该细分词性的 distinct
  sense node 数量，同一 sense 出现在多个 publication 时不能重复计数；
- `created_by` / `updated_by`：关联 `admins` 后映射为公开 actor。

在智能词库表尚未落地时，两个 `usage_count` 固定返回 0。以后通过当前草稿关系表和 §10 的
publication 引用表聚合，删除事务仍以真实 FK/引用检查为准，不能相信响应中的旧计数。

## 8. 写事务

### 8.1 新增

1. 开启事务；
2. 校验并 trim 输入；
3. 插入配置；
4. `catalog.metadata.version + 1`；
5. 提交；
6. 返回完整记录。

唯一约束冲突统一映射为 `part_of_speech_conflict` 或 `sub_part_of_speech_conflict`。

### 8.2 修改

使用条件更新实现乐观锁：

```sql
UPDATE ...
SET ..., revision = revision + 1, updated_at = now()
WHERE id = $id AND revision = $base_revision;
```

更新到 0 行时再区分：

- 记录不存在：404；
- revision 不一致：409 `revision_conflict`。

修改成功后在同一事务增加 `catalog_version`。

### 8.3 删除

1. 开启事务并锁定目标记录；
2. 将锁定行的当前 revision 与必填查询参数 `base_revision` 比较；不一致返回 409
   `revision_conflict`，不继续引用检查或删除；
3. 查询当前草稿与仍保留 publication 的真实引用数量；
4. 有引用则返回 409，不修改数据；
5. 删除细分词性，或删除基本词性并级联其未引用细分词性；
6. `catalog.metadata.version + 1`；
7. 提交并返回 204。

未来智能词库的当前草稿外键和 publication 引用外键都必须使用 `ON DELETE RESTRICT`，即使
应用层检查遗漏，数据库也不能产生悬空引用。词条保存与配置删除并发时由 PostgreSQL FK 锁语义
串行化；删除侧仍显式锁定配置行并查询计数，以返回可理解的 409。最终遇到已知 catalog 引用
外键的 SQLSTATE `23503`，也必须映射为对应的 `*_in_use`，不能退化为 500。

DELETE 语句触发 `23503` 后当前事务已经失败，不能继续在同一事务内查询 usage。正常的显式引用
检查分支必须返回 `meta.usage_count`；极端 FK 兜底分支可以先回滚 savepoint/事务后重新统计，
也可以返回不带 `usage_count` 的 409（`meta` 本来就是可选扩展），但不能尝试在 aborted transaction
中查询，也不能因此返回 500。只有 §10 明确列出的 active draft/publication catalog 外键可以映射
为 `*_in_use`；未知 `23503` 仍按内部错误处理，避免掩盖其他数据模型缺陷。

## 9. 错误码

继续使用项目统一的 `application/problem+json`：

| HTTP | code                           | 场景                                                       |
| ---- | ------------------------------ | ---------------------------------------------------------- |
| 400  | `invalid_part_of_speech`       | JSON 结构正确，但 code、名称、缩写或 PATCH revision 值非法 |
| 400  | `invalid_query`                | q、分页或 DELETE `base_revision` 查询非法                  |
| 400  | `invalid_path_parameter`       | `{id}` 或 `{sub_id}` 不是合法 UUID                         |
| 400  | `invalid_json`                 | JSON 语法非法                                              |
| 401  | 现有 admin 鉴权错误            | token 缺失、失效或账号不存在                               |
| 403  | 现有 admin 权限错误            | 非超级管理员执行管理操作                                   |
| 404  | `part_of_speech_not_found`     | 基本词性不存在                                             |
| 404  | `sub_part_of_speech_not_found` | 细分词性不存在或不属于路径中的父级                         |
| 409  | `part_of_speech_conflict`      | 编码、名称或缩写冲突                                       |
| 409  | `sub_part_of_speech_conflict`  | 编码或同父级名称冲突                                       |
| 409  | `revision_conflict`            | PATCH body 或 DELETE query 的 `base_revision` 已过期       |
| 409  | `part_of_speech_in_use`        | 基本词性已被词条引用                                       |
| 409  | `sub_part_of_speech_in_use`    | 细分词性已被词义引用                                       |
| 422  | `invalid_request_body`         | 字段缺失、类型错误或出现未知/只读字段                      |
| 500  | `internal_error`               | 数据库或未知错误                                           |

错误响应沿用统一 Problem Details，并扩展一个可选 `meta` 对象。`field` 是 Problem Details 的
顶层字段，不放进 `meta`。例如 revision 冲突：

```json
{
  "type": "urn:tsz:problem:revision_conflict",
  "title": "Revision conflict",
  "status": 409,
  "detail": "configuration changed",
  "code": "revision_conflict",
  "field": "base_revision",
  "meta": {
    "current_revision": 4,
    "part_of_speech_id": "019f...",
    "code": "noun"
  }
}
```

统一错误基础设施新增 `ProblemMeta`，并让 `ProblemDetails` 包含
`meta: Option<ProblemMeta>`（`None` 时省略）。当前 catalog 使用的 `ProblemMeta` 字段固定为
`current_revision`、`usage_count`、`part_of_speech_id` 和 `code`，每个字段自身也可选；后续词库域
已有的 `word_id`、`max_reachable_step`、`affected_node_ids` 继续并入同一个通用类型，不能再把
`HttpError.meta` 限定为 `AdminWordApiErrorMeta`。`AppError` 提供共享 `with_meta` builder，并允许
404 指定领域 ErrorCode；catalog handler 不得私自返回另一套错误外壳。

唯一冲突把具体冲突字段放在顶层 `field`。Repository 必须按固定数据库约束名映射，不能匹配
数据库错误文案：

| 固定 constraint/index 名                          | Problem code                  | 顶层 `field`   |
| ------------------------------------------------- | ----------------------------- | -------------- |
| `catalog_parts_of_speech_code_unique_idx`         | `part_of_speech_conflict`     | `code`         |
| `catalog_parts_of_speech_name_zh_unique_idx`      | `part_of_speech_conflict`     | `name_zh`      |
| `catalog_parts_of_speech_name_en_unique_idx`      | `part_of_speech_conflict`     | `name_en`      |
| `catalog_parts_of_speech_abbreviation_unique_idx` | `part_of_speech_conflict`     | `abbreviation` |
| `catalog_sub_parts_code_unique_idx`               | `sub_part_of_speech_conflict` | `code`         |
| `catalog_sub_parts_name_zh_unique_idx`            | `sub_part_of_speech_conflict` | `name_zh`      |
| `catalog_sub_parts_name_en_unique_idx`            | `sub_part_of_speech_conflict` | `name_en`      |

`src/platform/db.rs` 保留“识别 SQLSTATE + 精确 constraint name、由各领域决定业务映射”的边界，
在现有 `23505` helper 之外补充已知 `23503` 判断。客户端只能按 `status`、`code`、`field` 和
`meta` 分支，不得匹配可调整的 title/detail 文案。

前端 mock 联调时必须同步以下漂移：字段值校验目前使用 422 `invalid_request_body`；细分唯一冲突
目前错误地返回 `part_of_speech_conflict`；404 目前使用通用 `not_found`。真实接口启用前，应把
mock 和页面中文错误映射统一到本节错误码。

## 10. 与智能词库的唯一耦合

智能词库后续落地时，当前 active draft 需要建立两个外键：

```text
lexicon.entry_pos.part_of_speech_id
  -> catalog.parts_of_speech.id ON DELETE RESTRICT

lexicon.senses.sub_part_of_speech_id
  -> catalog.sub_parts_of_speech.id ON DELETE RESTRICT
```

同时由 Rust 服务校验细分词性的父级与当前 entry POS 相同。

上述四类引用外键使用以下固定 constraint 名，既用于数据库迁移审查，也用于 `23503` 精确映射：

| 引用关系                                     | 固定 constraint 名                              |
| -------------------------------------------- | ----------------------------------------------- |
| active draft `entry_pos -> parts_of_speech`  | `lexicon_entry_pos_catalog_pos_fkey`            |
| active draft `senses -> sub_parts_of_speech` | `lexicon_senses_catalog_sub_pos_fkey`           |
| publication 基本词性引用                     | `lexicon_publication_pos_refs_catalog_fkey`     |
| publication 细分词性引用                     | `lexicon_publication_sub_pos_refs_catalog_fkey` |

删除基本词性时，上表任一约束阻止 DELETE 都映射为 `part_of_speech_in_use`；删除单个细分词性时，
active draft senses 和 publication 细分引用两类约束映射为 `sub_part_of_speech_in_use`。正常删除前
检查也必须覆盖目标基本词性的直接引用及其所有子项引用，不能只查 `entry_pos` 后依赖 FK 才发现
sense 引用。

这两个外键不足以保护不可变发布版本。智能词库设计规定具体节点表只保存 active draft；管理员
从新草稿移除已发布 POS/sense 后，具体行会删除，但旧 publication 仍保留。因此发布事务还必须
写入结构化 catalog 引用，推荐由 lexicon 域维护两张表：

```text
lexicon.entry_publication_part_of_speech_refs
  publication_id, entry_id, source_node_id, part_of_speech_id

lexicon.entry_publication_sub_part_of_speech_refs
  publication_id, entry_id, source_node_id, sub_part_of_speech_id
```

- 两表的 catalog 外键均为 `ON DELETE RESTRICT`；
- `source_node_id` 指向发布时的 POS/sense 稳定 node，用于跨 publication 去重 usage；
- publication 创建与引用行写入必须在同一事务；
- publication 仍保留时引用行不得提前删除；将来显式清理 publication 时随 publication 级联删除；
- 删除检查和 usage_count 同时查询 active draft 与这两张表；
- publication canonical snapshot 仍可保存稳定 code 供 wire 使用，但 JSONB 不能作为引用完整性的
  唯一保障。

数据库保存 UUID 外键；API 可以继续保存和返回稳定 `code`，由 Service 在 DTO 与数据库模型之间
映射。配置改名或修改缩写不会触发词条 revision。

## 11. 测试范围

### 11.1 Schema 测试

- 默认种子数量为 11 个基本词性、19 个细分词性；
- 种子的名称、父级和 sort_order 与 §3 完全一致；
- code、名称和缩写唯一约束；
- 唯一索引使用 §2 固定名字，能够稳定映射冲突字段；
- 非法 code 被 CHECK 拒绝；
- 未 trim、空白、超长名称/缩写被 CHECK 拒绝；
- 细分词性不能引用不存在的基本词性；
- `catalog.metadata` 不能插入 `id = FALSE`，version 必须大于 0；
- 删除未引用基本词性会级联删除其细分词性；
- revision 必须大于 0；
- catalog 的管理员审计 FK 使用预定的 RESTRICT 行为。
- lexicon 落地时四类 catalog 引用 FK 使用 §10 固定 constraint 名。

### 11.2 Repository / Service 测试

- catalog 稳定排序和嵌套结构；
- `catalog_version` 与 items 在并发写入期间仍来自同一个数据库快照；
- 创建、修改、删除后 catalog version 恰好加一；
- revision 冲突不会覆盖新数据；
- DELETE 使用过期 `base_revision` 时不删除记录，也不增加 catalog version；
- code 不能通过 PATCH 修改；
- 子词性路径父 ID 不匹配返回 404；
- 被引用配置删除返回 409；
- publication-only 引用也会阻止删除，同一 entry/sense 跨 publication 不重复计算 usage；
- 配置删除与词条保存并发时不会产生悬空引用，FK 失败映射为业务 409；
- 已知 `23503` 兜底返回 `*_in_use`；没有重新统计时允许省略 meta，但不能退化为 500；
- 并发创建相同唯一值时恰好一个成功，失败方返回带正确 field 的 409；
- 同一事务失败时配置和 catalog version 一起回滚。

### 11.3 Handler 测试

- 普通管理员只能读取 catalog；
- `super_admin` 可以执行管理接口；
- 请求和响应保持 snake_case；
- 基本列表使用 `{ items, pagination }`，细分列表使用 `{ items }`；
- POST/PATCH/DELETE 分别返回 201/200/204；
- PATCH 携带 code 或未知字段返回 422，不会被静默忽略；
- DELETE 缺失/非法 `base_revision` 返回 400 `invalid_query`，过期 revision 返回 409；
- 非法 `{id}` / `{sub_id}` 返回 `application/problem+json` 的 400 `invalid_path_parameter`，
  不泄漏 Axum 默认 rejection 文本；
- Problem Details code 与状态码稳定；
- `field` 位于顶层，revision/usage 上下文位于 `meta`，无 meta 时响应省略该键；
- OpenAPI 的 `ProblemDetails` 引用可选 `ProblemMeta` schema，并包含新增 ErrorCode；
- 尚未更新的记录省略 `updated_by`，系统种子映射 system actor；
- OpenAPI 包含全部九个端点。

## 12. 落地顺序

1. 评审本文，并同步 `word-data-model.md`、`frontend-integration.md` 的引用与 wire；
2. 抽取共享 admin authorization 守卫，扩展统一 ErrorCode、`ApiPath`、ProblemDetails/ProblemMeta；
3. 创建 catalog up/down migration 和固定 UUID v7 默认种子；
4. 先写 schema 约束测试；
5. 实现 `src/catalog` model/repository/service；
6. 实现 admin handler/router 和九个端点测试；
7. 生成并提交 `docs/openapi.json`；
8. 同步前端 mock 的状态码/错误码漂移和细分冲突中文映射；
9. 与前端真实接口联调，确认精确响应信封后删除 PENDING 白名单；
10. lexicon 落地时同时实现 active draft FK 与 publication catalog 引用表；在此之前不得宣称
    “已发布词条引用保护”已经完成。
