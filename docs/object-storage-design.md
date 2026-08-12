# 通用对象存储底座设计

## 1. 目标与边界

本模块只提供“按空间隔离的对象操作”，供头像、TTS、附件等领域复用。它不持有业务元数据，
不决定数据库事务，也不执行领域补偿。领域层负责先后顺序、数据库记录、失败补偿和孤儿对象治理。

默认配置不启用任何空间，也不会连接远端存储；现有 HTTP API 行为保持不变。

## 2. ObjectKey

`ObjectKey` 是相对于空间 `root` 的逻辑键，不是 URL、文件系统路径或用户文件名。它采用可移植的
保守字符集，只允许 ASCII 字母、数字、`-`、`_`、`.` 和 `/`，并拒绝空键、绝对路径、反斜杠、
空段、`.` / `..` 段、控制字符、查询串/片段以及超长键。

生产代码应由服务端生成键：稳定的领域命名空间 + UUIDv7 + 经白名单验证的扩展名。客户端文件名
只能作为数据库元数据，绝不能直接成为对象键。读取数据库中既有键时仍须重新通过 `ObjectKey`
校验。

## 3. StorageSpace 与策略

每个 `StorageSpace` 独立绑定一个后端、bucket、root 和策略：

- `bucket` / `root`：adapter 构建时固定；所有调用只接受相对 `ObjectKey`，不能越过 root。
- OSS `root` 还会为最长 `ObjectKey` 预留空间，确保组合后的物理对象名不超过 1023 字节。
- OSS endpoint 与 bucket 会按最终虚拟主机联合校验，确保完整 DNS 主机名不超过 253 字节。
- `privacy`：`private` 或 `public-read`，作为领域可见的空间属性；底座不修改 bucket ACL。
- `max_object_size`：`put`、`read`、`copy` 和预签名写均执行限制。
- `presign_ttl`：每个空间固定，必须在安全范围内；调用方不能临时放大 TTL。
- `cache_control`：空间固定的响应缓存策略，在写入及预签名写时绑定。

registry 以 `StorageSpace` 查找已绑定的 `ObjectStore`，因此同一个逻辑键在不同空间中互不可见。
一个进程可同时绑定多个空间，且每个空间可以使用不同 bucket、root 和策略。
启动校验拒绝两个空间映射到同一 OSS bucket 中相同或祖先/后代关系的 root；相邻但不重叠的
root 可以共享 bucket。该约束也覆盖同一 bucket 的内外网 endpoint 混用，避免配置失误破坏隔离。

## 4. ObjectStore 契约

稳定能力只有：

- `put`
- `read`
- `stat`
- `presign_read`
- `presign_write`
- `copy`
- `delete`

不提供 clear bucket、delete prefix、任意全桶扫描/list 或批量删除。未来新增高风险管理能力必须在
独立的运维边界中设计，不能扩充本业务接口。

`delete` 采用幂等语义；`copy` 仅限同一空间，并使用同一个 GET 响应的正文与元数据，再通过最多
`max_object_size + 1` 字节的受限读取后写目标，避免源对象并发替换时混用版本元数据或留下超限
副本。跨空间复制由领域层显式执行“读 + 写”并处理补偿。
普通 `read` 使用同一受限流式读取机制，超限内容不会先被完整缓冲。
预签名请求属于秘密信息：类型的 `Debug` 会隐藏 URL 和全部 header 值，业务日志也不得记录签名
URL、AccessKey、请求/响应对象内容。

## 5. 配置与启动

未设置任何 `OBJECT_STORAGE_*` 环境变量时 registry 为空，不创建 OSS client。显式设置时，以
`OBJECT_STORAGE_SPACES` 声明空间列表；每个空间必须一次性提供完整 OSS 连接字段和策略字段。缺字段、
重复空间、孤儿空间变量、非法 root/策略或未知字段均导致启动期失败，不允许部分启用。

错误信息只包含空间名和缺失字段名，不回显 AccessKey 值。普通测试使用内存 adapter；真实 OSS
冒烟测试必须显式 `--ignored` 运行并从专用环境变量读取凭据。

每个空间的环境变量如下；`<SPACE>` 是空间名转大写并将 `-` 替换成 `_` 后的值：

```text
OBJECT_STORAGE_SPACES=avatars,speech
OBJECT_STORAGE_<SPACE>_BACKEND=oss
OBJECT_STORAGE_<SPACE>_OSS_ENDPOINT=https://oss-cn-hangzhou.aliyuncs.com
OBJECT_STORAGE_<SPACE>_OSS_REGION=cn-hangzhou
OBJECT_STORAGE_<SPACE>_OSS_BUCKET=example-bucket
OBJECT_STORAGE_<SPACE>_OSS_ROOT=/production/avatars
OBJECT_STORAGE_<SPACE>_OSS_ACCESS_KEY_ID=...
OBJECT_STORAGE_<SPACE>_OSS_ACCESS_KEY_SECRET=...
OBJECT_STORAGE_<SPACE>_PRIVACY=private
OBJECT_STORAGE_<SPACE>_MAX_OBJECT_SIZE_BYTES=1048576
OBJECT_STORAGE_<SPACE>_PRESIGN_TTL_SECONDS=300
OBJECT_STORAGE_<SPACE>_CACHE_CONTROL=private, max-age=60
```

不需要 `Cache-Control` 时显式写 `none`。所有字段均为必填，这使“声明了空间但漏配一半”的部署
在启动期整体失败，而不是静默构造半可用 registry。

OSS `presign_write` 使用区域感知的 V4 签名，将精确 `Content-Length`、空间固定 `Cache-Control` 和
调用方声明的 `Content-Type` 一并纳入签名请求。客户端必须原样发送返回的 headers，修改或省略
受签字段会导致 OSS 验签失败。上传后的 `stat` 仍是领域确认业务元数据前的纵深校验；若不符合
领域预期，应删除精确对象键。后续直传业务 PR 还应配置 bucket/RAM 权限最小化策略。

## 6. 领域层职责

对象存储不等于业务提交。领域层必须：

1. 生成对象键并写对象；
2. 在数据库事务中保存对象键、内容类型、大小、校验信息等元数据；
3. 数据库失败时尝试删除新对象；
4. 替换/删除业务实体时，在事务提交后清理旧对象；
5. 对补偿失败建立可观测的重试/孤儿回收机制。

底座不会扫描 bucket 来推断业务状态，也不会替领域层写数据库。
