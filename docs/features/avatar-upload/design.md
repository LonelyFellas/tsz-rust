# 头像上传设计评估

本文件为评估方案，尚无实现代码或新迁移。产品范围和已确认的公开可见规则见 [requirements.md](requirements.md)。

## 1. 现状与复用依据

- 后端基线：远程 main `cc1723121a171020d154e0cdc13908fe34c74159`，任务分支 `assess/avatar-upload-20261002`。
- 前端已有 `POST /me/avatar/upload-url → PUT upload.url → POST /me/avatar`，确认时遇到 500 会重试一次。现有响应是 `{ upload }` 与 `{ user }`，不能改成顶层 token 或其他包裹。
- [UserProfile](../../../src/auth/handler.rs) 的 `load_user_profile` 复制数据库 `avatar_url` 并装配角色；`GET /auth/me` 仍返回扁平资料，前端自己包装为 MeResponse。
- [users 迁移](../../../migrations/20260709075649_create_users.up.sql) 已有 `avatar_url TEXT NOT NULL DEFAULT ''`，但没有存储 key 或上传许可；[UserRepository](../../../src/user/repository.rs) 注销账号使用真实 DELETE。
- [ObjectStore](../../../src/platform/storage/service.rs) 已支持 put/read/stat/presign_write/delete；read 有上界，不应再建另一套 OSS SDK。底座没有公有 URL 生成接口，privacy 属性也不修改实际 bucket ACL。
- [教师认证图片](../../../src/teacher_certification/files.rs) 已使用真实格式检查、解码限制与 spawn_blocking，可借鉴技术做法，但其私密材料、权限及数据表不能直接当头像使用。

## 2. 推荐架构

保留现有直传协议，使用独立的 **private avatars 空间**。原图进入 `uploads/avatars/<许可 UUID>/original.<白名单扩展名>`，后端确认时处理为正式 WebP；正式对象 key 使用独立随机 UUID，绝不向客户端签发其写权限。

新增公开头像读取接口：只从数据库定位已确认、仍被用户引用的正式图片，再通过 ObjectStore.read 返回 bytes。这样存储桶仍保持私有，客户端获得稳定版本地址，不需要新增底座 public_url 能力，也不会把短期 GET 签名保存在长期用户资料中。

```text
已登录用户 → 申请许可（绑定用户/大小/类型/到期）
浏览器 → 私有 OSS 临时对象（预签名 PUT）
已登录用户 → 确认（读实际字节 → 解码/处理 → 写正式对象 → 原子更新资料）
头像 img → 公开读取接口（只读取当前已确认的正式头像）
后台任务 → 到期临时文件、未生效候选对象、旧头像的精确 key 清理
```

对象不经后端上传，但确认会受限读取原图；公开读经过后端，增加读带宽开销，接受为首版取舍。不能用老师认证材料的 authenticated/no-store 路由替代公开头像。

不采用：直接公开临时桶、确认时只看 MIME/stat、把用户提供的 URL 写入资料、仅签发 URL 不落许可、用短期 presign_read 作为 avatar_url。直接公有 OSS/CDN 地址并非不可行，但需要另定义地址生成和临时/正式空间隔离，不作为本次前置条件。

## 3. 接口契约

| 方法与路径                          | 身份         | 请求                                              | 成功响应                                                                                                                           |
| ----------------------------------- | ------------ | ------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `POST /api/v1/me/avatar/upload-url` | Web 登录用户 | `{ "content_type": "image/jpeg", "size": 12345 }` | 200 `{ "upload": { "key": string, "url": string, "headers": Record<string,string>, "expires_in": number, "max_bytes": 5242880 } }` |
| `POST /api/v1/me/avatar`            | Web 登录用户 | `{ "key": string }`                               | 200 `{ "user": UserProfile }`，包含最新 avatar_url、id、display_name、roles、active_role；phone/email 保持现有可选语义             |
| `GET /api/v1/avatars/{id}`          | 无需登录     | 服务端生成的头像许可 UUID                         | 200 `image/webp` 二进制；只服务当前正式引用                                                                                        |

- 许可 key 保持 `uploads/` 前缀，但客户端必须视为不透明值，不解析所有者或目录。
- size 是精确整数，范围 1–5242880；过大返回 413，空文件/非法格式返回具体输入错误。JSON 语法/反序列化错误沿用 ApiJson 的 400/422。
- expires_in 返回实际空间 TTL，头像空间配置必须不超过 600 秒；许可 expires_at 与签名期限协调，清理加短宽限。
- headers 保留签名所需值。浏览器会按 File 自动提供 Content-Length，不依赖 JS 强设禁止的 header；必须用真实浏览器验证与 OSS V4 签名一致。
- 公开响应包含 `Content-Type: image/webp`、`X-Content-Type-Options: nosniff`、`Cache-Control: public, max-age=300`。地址随新许可变化，缓存不超过 5 分钟，不承诺收回已下载副本；错误响应使用 no-store，避免未确认图片的 404 被负缓存。
- 通过配置的公开地址基线生成绝对 URL，不信任客户端 Host/X-Forwarded-Host，不接受任意存储 key 或外部 URL 作为图片读取目标。

错误使用 [现有 Problem Details](../../api-errors.md) 的 code/field 规则；保留已有可识别的英文 detail，同时前端优先按 code 映射。

| 状态 / 新 code                                     | 语义与对应前端处理                                                         |
| -------------------------------------------------- | -------------------------------------------------------------------------- |
| 400 `unsupported_avatar_content_type`              | 非白名单或声明与实际格式不一致；detail `unsupported avatar content type`   |
| 400 `invalid_avatar_size` / `avatar_invalid_image` | 空文件、大小不符、损坏或解码资源超限；前端补明确中文提示                   |
| 413 `avatar_file_too_large`                        | 实际或声明大小超限；detail `avatar file too large`                         |
| 400 `invalid_avatar_key`                           | 未知、他人、过期且未确认许可统一处理；detail `invalid avatar key`          |
| 409 `avatar_upload_not_completed`                  | 临时对象尚未完成上传，可重新尝试确认；detail `avatar upload not completed` |
| 429 `avatar_upload_rate_limited`                   | 发放频率或有效未结束许可超限                                               |
| 501 `avatar_storage_not_configured`                | 未启用头像存储/公开地址配置；detail `avatar storage not configured`        |
| 503 `avatar_storage_unavailable`                   | 已配置但存储不可用；前端补中文映射，不冒充未配置                           |
| 500 `internal_error`                               | 不公开内部 cause；确认重试必须安全                                         |

认证/禁用错误沿用既有身份规则；公开读的未知、未确认、已替换或已注销对象统一 404，不泄露原始文件状态。

## 4. 数据与事务

建议增加一对 up/down 迁移，命名在实施时按最新迁移序列确定：

- `avatar_uploads`：id、user_id、source_key、declared_type、size_bytes、expires_at、state（pending/confirmed/invalid）、canonical_key、confirmed_at、created_at。
- source_key 与正式 key 唯一；合法类型、大小及 confirmed 元数据有 CHECK。user_id 引用 users，`ON DELETE SET NULL` 保留对象元数据供清理；非本人/无所有者记录不能确认。
- `users.avatar_upload_id` 可空，与已有 avatar_url 在同一事务更新。采用 `(avatar_upload_id, users.id) → (avatar_uploads.id, avatar_uploads.user_id)` 复合 FK（许可表增加对应唯一约束），限制被引用许可删除，数据库直接阻止引用他人的许可；不使用会连带清空 users.id 的复合 ON DELETE SET NULL。验收必须验证许可所有者 SET NULL 与用户硬删除的组合。
- `avatar_cleanup_tasks`：精确 object_key、任务类型、关联许可、not_before、处理 lease、尝试次数和错误码；不得随用户或许可 cascade 删除。唯一约束避免相同清理任务重复堆积。

申请许可：短事务锁定有效用户，核对当前 security_version，统计有效 pending 及最近一分钟发放数；首版建议最多 3 个有效 pending、每分钟最多 3 次。写入许可及到期原图清理任务后生成签名。签名失败留下的许可会到期回收，不生成已生效头像。

确认分两段，不跨图片处理/存储 I/O 持有数据库锁：

1. 验证本人许可与期限，受限 read 原图；以同一次 read 的实际 bytes 验证大小、魔数及解码。先应用可识别方向，居中处理为 512×512 静态 WebP，剥离 EXIF 等元数据。解码参考既有 8192 边长/64 MiB 分配上限，图像处理全局最多 4 个并发任务，存储操作有有界超时。
2. 为本次确认生成独立候选 key，**写对象前**持久登记清理任务，延迟到请求超时及宽限后执行；不同确认尝试不得覆盖同一个正式 key。
3. 写入候选对象后，短事务按「用户 → 许可 → 候选清理任务」顺序加锁，再次核对账号/security_version/所有者/到期；候选任务尚未被领取清理（领取次数为 0，即使 lease 到期也不能重置历史次数）时，pending 许可才能提交 canonical_key、用户头像引用与 URL，并在同一事务撤销该候选回收任务、登记旧头像清理。任务已进入清理时不能让该候选变成正式引用，需重试创建新候选。
4. 若另一确认已成功，返回当前用户资料，不重设头像；当前候选对象回收。已确认的旧许可重试也不恢复旧头像。事务失败保留旧头像，候选由清理任务回收。
5. 公开读要求 state=confirmed、owner 仍存在且 users 当前引用等于该许可；因此未提交候选、替换后的旧对象或注销用户均不可公开读取。

清理任务领取使用有界批次与 lease，崩溃后可再次领取；领取候选任务与确认提交必须争用同一任务行，使「开始删除」和「成为正式引用」不能同时成立，不能只做一次无锁引用查询后删除。已撤销的候选任务不再领取，旧头像用独立任务回收；执行删除前仍检查对象没有正式当前引用。底座 delete 幂等，失败记录留在数据库重试，不扫描 bucket，也不删除 prefix。

原图任务的 not_before 至少为签名期限 + 120 秒；不在有效 PUT 链接仍可重传时把一次删除当作清理完成。候选写入超时也不能直接忘记任务，宽限和重试必须覆盖迟到写入。部署时建议为 avatars root 下仅 `uploads/avatars/` 临时前缀配置 1 天 OSS 生命周期作为迟到写入兜底（实际回收有提供方执行延迟）；不覆盖 `images/`、教师材料或其他空间，也不通过业务底座自动修改 bucket 规则。该配置需独立授权和验收，不能把它当作业务清理已经成功。

## 5. 替换、注销与迁移回退

- 在 [账号注销事务](../../../src/auth/handler.rs) 调用 UserService.delete_account_in **之前**，将该用户所有头像原图/正式对象纳入清理任务；任务与用户 DELETE 同事务提交，不能等 cascade 后再寻找 key。
- 历史 avatar_url 原样保留，新增引用为 NULL；本期不导入、不删除无记录的旧外链头像，不根据 URL 反向猜测 bucket key。
- up 是新增表和可空字段，不重写既有资料。旧 API 继续读取 avatar_url，现有 UserProfile wire 结构不变。
- 推荐回滚代码时保留 up 数据和清理元数据；旧后端无新头像读接口，新版本图片可能暂时回退为文字头像，需明确作为回退影响。
- down 必须成对，但会丢许可/对象清理记录和新头像引用，不能自动执行。先停止新上传、完成/导出未完成清理并确认数据处理方案；只清理可确认属于新功能的引用，不能清空所有历史 avatar_url。

## 6. 配置与修改位置

- 复用已有 `OBJECT_STORAGE_SPACES`，追加 `avatars`，不覆盖老师认证等空间；头像 root 不得与已有空间重叠，实际 bucket 必须私有。
- `OBJECT_STORAGE_AVATARS_` 下沿用 BACKEND、OSS_ENDPOINT/REGION/BUCKET/ROOT/ACCESS_KEY_ID/ACCESS_KEY_SECRET、PRIVACY、MAX_OBJECT_SIZE_BYTES、PRESIGN_TTL_SECONDS、CACHE_CONTROL；建议 private、5242880、600、none。
- 新增 `AVATAR_PUBLIC_BASE_URL`：对应公开头像读接口的绝对地址前缀，须指向实际外部 API/前端代理，不能填服务器内部 loopback 地址用于生产。禁止凭据、query、fragment；生产 HTTPS，本地 HTTP 按现有本地规范。
- avatars 未声明时功能关闭且无远端连接；声明后连接/策略/公开 URL 不完整或不安全时启动失败，不能半启用。RPC 或浏览器 CORS 故障属于可用性错误，不等同于无配置。
- RAM 权限只给该 root 的 PutObject/GetObject/DeleteObject；浏览器 PUT CORS 允许实际 Web origin 和实际签名 headers，不开放 bucket 管理能力。

主要实施文件：`src/avatar/{mod,dto,handler,service,repository,image,cleanup}.rs`（新增）；`src/lib.rs` 挂载路由和清理 worker；`src/auth/handler.rs` 复用资料装配并接入注销清理；`src/config.rs`、`.env.example`、`src/error.rs`、`src/openapi.rs`、成对迁移和 `.sqlx`。图片处理复用 image crate，存储复用现有 ObjectStore，不为仓储再做 trait/fake 双实现。

关键签名建议：`AvatarService::create_upload(auth, request)`、`AvatarService::confirm(auth, key)`、`AvatarService::read_public(id)`；`AvatarRepository::commit_avatar_in(&mut PgConnection, ...)`、`schedule_user_cleanup_in(&mut PgConnection, user_id)`；`normalize_avatar(bytes, declared_type) -> WebpBytes` 在有并发限制的 spawn_blocking 中执行。

## 7. 契约与发布顺序

- 实现后导出后端 OpenAPI；前端显式 OPENAPI_SOURCE 指向本任务后端版本，同步快照。不得从默认相邻旧 worktree 取契约。
- 只移除两个已实现头像写端点的 PENDING，其他资料/学习设置端点仍按实际状态保留。前端保留三步上传，补新增 code 的中文映射与新契约测试，不重做资料页。
- 优先验证「当前前端 + 新 API」：许可字段与 nested user 逐项一致，me 扁平形状不变，img 能加载公开地址，确认 500 重试与 501 会话关闭行为正常。
- 「前端契约更新 + 旧 API」仍会 404 并降级提示，不应崩溃；「两端候选」需真实上传验收；回退旧 API 的头像读取损失按上一节处理。
- 推荐先发布通过上述兼容验证的后端 additive API 与完整存储配置，再发布前端契约/错误文案更新；501 后前端已缓存不可用状态时需刷新重新探测。若混合版本未验证通过，则不推断顺序可用。
- 发布、存储配置及真实 OSS 验收均需独立授权，本次评估未执行。

## 8. 验收

**本次已验证**：以 Rust 1.98.0、SQLX_OFFLINE=true 执行现有 `cargo test --locked --lib platform::storage`，8 个库测试通过；无数据库变更，无 OSS 请求。它只证明底座现有逻辑，不证明头像功能可用。

下列功能命令在**实现新增测试目标后**、对应后端 worktree 根目录执行。PostgreSQL 必须使用独立可创建测试库的连接；仓储/事务用 sqlx 真库测试，存储正文用现有 memory adapter（预签名 URL 为 fake，不可当真实 OSS 验收）。本次不执行这些迁移或功能测试。

```bash
SQLX_OFFLINE=true cargo test --locked --test avatar_handler
```

预期：新增用例覆盖 Web/Admin/匿名身份、归属、大小/MIME、TTL、额度、完整返回 UserProfile、无存储 501、公开读限制、500 重试、并发确认和旧许可不覆盖新头像。

```bash
SQLX_OFFLINE=true cargo test --locked --test avatar_lifecycle
```

预期：真库事务覆盖失败补偿、候选对象登记、到期后清理重传原图、替换、账号硬删除、worker lease/崩溃重试、当前引用永不误删。

```bash
SQLX_OFFLINE=true cargo test --locked --test avatar_migrations
```

预期：独立库验证 up/down、历史 URL 不改、所有者约束、SET NULL 与删除顺序、旧查询仍可用；down 的数据损失有明确断言。

```bash
cargo fmt --all -- --check
```

```bash
SQLX_OFFLINE=true cargo clippy --locked --all-targets --all-features -- -D warnings
```

预期：格式和静态门禁通过。新测试目标还需登记/核验现有 CI 测试分组。

**真实环境人工验收**：经授权配置专用私有 OSS 与 CORS，浏览器上传 JPG/PNG/WebP，检查实际 PUT 的签名 headers/Content-Length；无签名原图读取应拒绝。确认后匿名 img 地址可读、刷新/重新登录保持、换图版本变化、注销源站 404。核对孤儿对象与清理记录最终收敛、日志不含签名和图片内容；分别验证前后端候选和回退组合。本次真实存储、迁移及整条功能均未验证。
