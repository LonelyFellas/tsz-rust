# 依赖安全检查

CI 在 PR、main push 和手动运行时使用固定版本 `cargo-audit 0.22.2` 检查完整 `Cargo.lock`，包括可选功能和构建依赖。新漏洞、撤回版本及依赖维护警告会使总门禁失败。每周一 03:23 UTC 另跑一次审计，以发现锁文件未变时新增的公告；定时运行不重复构建和数据库测试。

本地复验：

```bash
cargo install cargo-audit --version 0.22.2 --locked
cargo audit --deny warnings
```

工具默认更新 RustSec 数据库；不要用过期数据库或 `--no-yanked` 代替正常门禁。例外统一放在 [`.cargo/audit.toml`](../.cargo/audit.toml)，不能通过降低严重级别或忽略全部警告放行。

## JWT 加密后端与当前例外

当前没有忽略的安全公告。

2026-10-24 将 `jsonwebtoken 10.4.0` 的加密后端从 `rust_crypto` 切换为上游支持的 `aws_lc_rs`，复用 TLS 依赖中已有的 `aws-lc-rs`。`rust_crypto` 引入的 `p256/p384 → elliptic-curve → crypto-bigint 0.5.5` 命中 [RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387.html)；当前最新 `jsonwebtoken 11.1.0` 的 `rust_crypto` 仍使用同一代曲线依赖，不能靠直接升级 JWT 大版本解除。

切换后锁文件不再包含 `crypto-bigint` 和 `rsa`，一并删除原先针对 [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html) 的例外。没有强行替换传递依赖的大版本，也没有降低审计门禁。

Web 和 admin 的 `TokenManager` 继续只签发、接受 HS256。`auth::token::tests::both_realms_only_accept_hs256` 固定算法白名单；`hs256_signatures_remain_compatible_with_existing_tokens` 使用独立 HMAC-SHA256 固定向量验证签名、既有令牌验签，并拒绝错密钥、跨 realm 和篡改签名。更改 JWT 算法、密钥类型或加密后端时须重新验证这些契约。

2026-10-22 已将 `utoipa-swagger-ui` 升至 10.0.2，其构建依赖使用 `zip 9`，无需忽略 `RUSTSEC-2026-0367`、`RUSTSEC-2026-0368` 或 `RUSTSEC-2026-0369`。Swagger 仍只在显式启用 `swagger` 功能时提供；升级时须验证 OpenAPI 生成、前端契约及页面资源。

## 2026-10-28 更新评估

本轮仅将 Swagger 构建依赖 `zip 9.1.2` 更新到 `9.1.3`，其余依赖保留当前版本。`cargo audit --deny warnings` 通过；安全审计通过不代表所有更新都已完成兼容验证。

| 依赖 | 当前 → 候选 | 处理与后续验收 |
| --- | --- | --- |
| `base64` | `0.22.1` → `0.23.1` | 新版本默认增加 `simd-unsafe` 功能；JWT 和 SQLx 仍使用 `0.22`，单独升级会保留两套版本。需要升级时单独核实功能选择，回归 refresh token、签名和分页游标的固定编码与非法输入。 |
| `jsonwebtoken` | `10.4.0` → `11.1.0` | 上游包含 Header、Key 和 Validation API 的不兼容变化。本轮保留通过审计的 `aws_lc_rs` 配置；后续升级须通过上述 HS256 固定向量、realm 隔离及既有 token 兼容回归。 |
| `quick-xml` | `0.41.0` → `0.42.0` | 名称和事件 API 从字节改为字符串，移除 `decode()` / `Reader::decoder()`；当前手写 SSML 校验直接使用这些 API。需要单独迁移并回归合法 XML、实体、命名空间、深度及长度限制。OpenDAL 仍引用 `0.41`。 |
| `opendal` | `0.58.2` → `0.59.5` | 上游调整 Metadata、删除选项和条件冲突错误语义。单独核对 OSS 元数据、条件读写、预签名及错误映射；内存存储测试不能替代真实 OSS 专项验收。 |
| `matchit` | `0.8.4` → `0.8.6` | Axum `0.8.9` 精确依赖 `=0.8.4`。跟随上游兼容版本，不通过 patch 或强制覆盖绕过约束。 |
| `crypto-common` | `0.1.6` → `0.1.7` | 传递路径为 SQLx → SHA-256 → digest。指定版本的 dry run 同时要求 `generic-array 0.14.9` 降至 `0.14.7`；本轮保留常规解析结果，不为单个版本号引入额外降级。 |

版本信息来自 `cargo update --dry-run --verbose`、`cargo tree --all-features --invert <package>` 及对应上游发布说明；这是本次评估快照，执行后续升级前应重新核实。
