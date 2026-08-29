# 部署指南（裸机二进制 + systemd）

面向单机/少量实例的生产部署。应用是单个静态性较强的二进制，配置全走环境变量，
**启动时自动跑数据库迁移**（migrations 已编译期内嵌进二进制），无需单独执行 `sqlx migrate run`。

> 分布式/多实例、TLS 终止、扩缩容超出本文范围，见文末「后续演进」。

## 0. 前置依赖（服务器上要有）

| 依赖 | 说明 |
|------|------|
| PostgreSQL | 生产库。可本机装、Docker，或用托管（RDS/云数据库）；服务端大版本以实际部署为准。**不要用** compose 里的 `postgres/postgres` dev 凭据。 |
| Redis 7 | OTP / 限流。数据到期即弃、无需持久化。 |
| （构建期）Rust | 若在服务器上构建则需 rustup；也可本地/CI 交叉编译后只传二进制。 |

## 1. 构建 release 二进制

`.sqlx/` 离线缓存已提交，构建期**不需要连数据库**，用 `SQLX_OFFLINE=true` 即可：

```bash
SQLX_OFFLINE=true cargo build --release
# 产物：target/release/tsz-rust
```

- **服务器是 Linux、开发机是 macOS**：不能直接把 mac 上编的二进制拷过去。二选一：
  - **在服务器上构建**（最省事）：服务器装 rustup → `git clone` → 上面这条命令。
  - **交叉编译**：本地用 [`cross`](https://github.com/cross-rs/cross)（需 Docker）编 `x86_64-unknown-linux-gnu` 或 `musl`，只传二进制。
- **不带 swagger**：release 默认不开 `swagger` feature，二进制不含 UI、不暴露接口清单（安全）。内网调试再 `--features swagger`。

> **tshb-test 不走上面任何一条**：它装的是 CI 产物。`.github/workflows/ci.yml` 的
> `release-artifact` job 在 `ubuntu:22.04` 容器里（与服务器 glibc 2.35 一致）编出
> `tsz-rust-x86_64-linux-gnu-<git-sha>-attempt-<run-attempt>`，并附带 SHA256 与严格的 build manifest。部署时由
> `deploy` skill 用 `gh run download` 取回，同时验证 run/SHA/tree/toolchain/features/SQLx
> 指纹和二进制摘要后原子替换。服务器不再编译、也不再需要源码与 rustup。新开环境若发行版不是 Ubuntu 22.04，
> 必须同步改 workflow 里的 `container:`——判据是容器 glibc 不得高于目标机 glibc。

## 2. 放置文件

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin tsz   # 专用非 root 用户
sudo mkdir -p /opt/tsz-rust /etc/tsz-rust
sudo cp target/release/tsz-rust /opt/tsz-rust/
sudo chown -R tsz:tsz /opt/tsz-rust
```

> migrations/ **不需要**随二进制部署——已在编译期内嵌。

## 3. 环境变量文件

写 `/etc/tsz-rust/tsz-rust.env`（systemd 用 `EnvironmentFile` 读，进程直接拿到；
无需在工作目录放 `.env`）。**权限收紧到 600**，里面有密钥：

```ini
# /etc/tsz-rust/tsz-rust.env
PORT=8383
DATABASE_URL=postgres://tsz_app:<强密码>@127.0.0.1:5432/tsz_rust
REDIS_URL=redis://127.0.0.1:6379/0
# ⚠️ 生产必须换成强随机值，别用 dev 的 "secret"。生成：openssl rand -base64 48
JWT_SECRET=<openssl rand -base64 48 的输出>
# 以下有默认值，按需覆盖
ACCESS_TOKEN_TTL_MINUTES=15
REFRESH_TOKEN_TTL_DAYS=30
OTP_TTL_MINUTES=10
OTP_COOLDOWN_SECONDS=60
OTP_DAILY_LIMIT=10
OTP_MAX_ATTEMPTS=5
# refresh cookie 的 Secure 标志。默认 true（只随 https 发送）。
# ⚠️ 前面没有 TLS（纯 http 部署、备案未下来等）必须显式设 false，
# 否则浏览器直接丢弃 Set-Cookie——登录看似成功但 /auth/refresh 永远 401，
# 且服务端无任何报错（§5 上了 HTTPS 后删掉这行恢复默认）。
# COOKIE_SECURE=false
```

```bash
sudo chown tsz:tsz /etc/tsz-rust/tsz-rust.env
sudo chmod 600 /etc/tsz-rust/tsz-rust.env
```

**缺任一必填项（`DATABASE_URL` / `JWT_SECRET` / `REDIS_URL`）应用启动即失败**——配错会当场暴露，不会跑到发验证码时才炸。

## 4. systemd unit

写 `/etc/systemd/system/tsz-rust.service`：

```ini
[Unit]
Description=tsz-rust API server
After=network-online.target
Wants=network-online.target
# 若 Postgres/Redis 也用 systemd 本机托管，取消下面两行注释保证启动顺序：
# After=postgresql.service redis-server.service
# Wants=postgresql.service redis-server.service

[Service]
Type=simple
User=tsz
Group=tsz
WorkingDirectory=/opt/tsz-rust
EnvironmentFile=/etc/tsz-rust/tsz-rust.env
ExecStart=/opt/tsz-rust/tsz-rust
Restart=on-failure
RestartSec=5

# 优雅停机：应用已接 SIGTERM，停止时放在途请求跑完再退。给足超时窗口。
KillSignal=SIGTERM
TimeoutStopSec=30

# 安全加固（应用只需读配置、连 DB/Redis，不写本地文件）
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

启用并启动：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now tsz-rust
sudo systemctl status tsz-rust
journalctl -u tsz-rust -f          # 看日志：应见 "database migrations applied" → "listening on 0.0.0.0:8383"
```

验证：

```bash
curl localhost:8383/healthz        # {"status":"ok"}    存活（不碰库）
curl localhost:8383/readyz         # {"status":"ready"} 就绪（探 DB+Redis）
```

**发音人目录**：`speech.voices` 是运营数据，不随 migration 建立。新建库或从备份恢复后要重跑一次种子，
否则试听目录为空，前端「获取语音」按钮会全部禁用：

```bash
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f /opt/tsz-rust/ops/speech-voice-catalog/seed.sql
```

幂等，可反复执行（一致时不写库，也不会启用运维停用过的发音人）；语义与改目录的注意事项见
`ops/speech-voice-catalog/README.md`。

**升级流程**：`systemctl stop` → 覆盖二进制 → `systemctl start`。新迁移会在启动时自动应用（多实例同时启动有 advisory lock 兜底，只一个真正执行）。

涉及数据库写入前，先按 [PostgreSQL 备份与恢复验证](postgresql-backup-restore.md) 创建并验证当前恢复点。客户端主版本不得低于数据库服务端主版本。

### 部署来源 manifest

服务器目录不是 Git checkout。每次部署只有在精确 `origin/main` 的 CI success、二进制编译/重启和 health/ready/auth smoke 全部通过后，才使用仓库工具发布正式 manifest：

```bash
python3 ops/deployment_manifest.py create \
  --component api \
  --repository LonelyFellas/tsz-rust \
  --git-sha <40位精确main SHA> \
  --git-tree <40位tree SHA> \
  --ci-run-id <精确CI run ID> \
  --ci-run-url https://github.com/LonelyFellas/tsz-rust/actions/runs/<run ID> \
  --artifact /opt/tsz-rust/target/release/tsz-rust \
  --artifact-path /opt/tsz-rust/target/release/tsz-rust \
  --output /opt/tsz-deploy-manifests/api.json

python3 ops/deployment_manifest.py verify \
  --manifest /opt/tsz-deploy-manifests/api.json \
  --artifact /opt/tsz-rust/target/release/tsz-rust
```

manifest schema version 1 记录 repository、Git commit/tree、远端 main ref、CI workflow/run、实际二进制 SHA-256和 UTC 验收时间；API 的 `excluded_paths` 固定为空。工具严格拒绝额外/缺失字段、JSON 布尔值冒充整数、非 success CI、错仓库/URL/路径和摘要不一致；写入使用同目录临时文件与原子 rename。manifest 不含环境变量、凭据或自由文本，也不通过 HTTP 暴露。

`api.json` 与实际二进制是一组部署制品。升级前必须成组备份，并在改变当前部署前用只读 `validate-backup` 验证备份二进制与 manifest 摘要一致（或明确是 `manifest.absent`）；验证失败时当前部署保持不动。验证通过后，在编译可能改变当前二进制前撤下正式 manifest；编译、重启、health/ready/auth smoke、create 或 verify 任一步失败，都必须使用 `restore` 子命令从精确备份目录恢复。restore 再次验证备份组，通过同目录临时文件和 `os.replace` 原子替换运行二进制，最后恢复配套 manifest，避免 `Text file busy` 与错配。旧版本没有 manifest 时应报告来源 UNKNOWN/BLOCKED，不能手工补写。该机制用于发现误部署或陈旧部署，不抵抗已取得服务器 root/GitHub 管理权限的恶意篡改。

## 5. TLS / 反向代理（必须）

应用只监听明文 HTTP `0.0.0.0:PORT`。**不要直接对公网暴露**，前面挡一层做 HTTPS：

- **Caddy**（最省事，自动签 Let's Encrypt）：

  ```
  api.example.com {
      reverse_proxy 127.0.0.1:8383
  }
  ```

- **nginx**：certbot 签证书，并显式覆盖客户端传入的 `X-Forwarded-For`：

  ```nginx
  location / {
      proxy_pass http://127.0.0.1:8383;
      proxy_set_header X-Forwarded-For $remote_addr;
  }
  ```

注册接口把该头最左段记录为 `users.registration_ip`。Caddy 默认忽略客户端传入的
`X-Forwarded-For`；nginx 必须使用上面的覆盖配置，不能原样透传不可信请求头。

**暂时无法上 TLS 时**（如域名备案审核中）：env 文件里显式设 `COOKIE_SECURE=false`（见 §3），
否则 refresh cookie 带 Secure 会被浏览器在 http 源静默丢弃，登录后刷新永远 401。
TLS 就位后删掉该行恢复默认 true，无需改代码。

建议把 `PORT` 只绑到 `127.0.0.1`（改 `run()` 里的 bind 地址）或用防火墙只放行反代——目前 bind 的是 `0.0.0.0`，靠防火墙隔离即可。

## 6. 上线前必看的功能缺口

- **OTP 短信是 Mock**（`OtpSender::Mock`，固定验证码 `000000`，只打日志不真发）。**该逻辑仅供内部测试，生产不可用**。正式启用手机验证码登录 / 找回密码前，必须接入真实短信供应商并恢复随机验证码。
- **无 CORS 层**：前端若独立域名，浏览器跨域会被拦。需在应用里加 `tower-http` 的 CORS 中间件（业务侧改动）。

## 后续演进

- 多实例 / 负载均衡：应用本身无状态（session 存 DB、OTP 存 Redis），可水平扩，反代做 LB 即可。
- 迁移与部署解耦（多实例场景）：若不想每个实例启动都尝试迁移，可拆成部署流水线里单跑 `sqlx migrate run`，应用只连不迁。当前默认「启动即迁移」对单/少实例最省心。
- 可观测性：目前 `tracing_subscriber::fmt` 输出到 stdout，由 journald 收。需要结构化日志/指标再接。
