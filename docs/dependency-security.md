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
