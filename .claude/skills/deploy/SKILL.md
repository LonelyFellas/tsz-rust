---
name: deploy
description: 仅在 GitHub origin/main 当前提交的 CI 成功后，通过 ssh 把该提交自动化部署到生产服务器 tshb-test（CI 门禁 → rsync → 服务器编译 → 重启 → 冒烟验证 → 可回退）。用户说「部署」「deploy」「推到服务器」「上线」「更新服务器代码」或提到 tshb-test / 47.121.142.19 的代码同步时使用。
---

# tsz-rust 部署到 tshb-test

把 GitHub **当前 `origin/main` 上 CI 成功的精确提交**部署到 Aliyun ECS
`tshb-test`（47.121.142.19，ssh 别名已配密钥）。服务器上
`/opt/tsz-rust` 不是 git 仓库，唯一同步方式就是本流程。

## 红线

- 只部署干净工作区中的本地 `main`，且 `HEAD` 必须等于 GitHub `origin/main`。
- 只把 CI `conclusion == success` 视为通过；`skipped`、`neutral` 也不允许部署。
- CI 为 `queued`、`in_progress`、`pending`、`requested` 或 `waiting` 时等待，不得提前部署。
- CI 失败、取消、超时、无运行记录、GitHub API/认证异常或状态未知时，一律不部署。
- 等待期间 `origin/main` 变化时，重新同步并从新 SHA 开始检查，绝不沿用旧提交的绿色结果。
- CI 门禁通过前，不得备份、rsync、编译、重启或执行任何会改变服务器状态的命令。
- 跨步骤的值（`deploy_sha` / `ci_run_id` / `deploy_backup_dir` 等）一律经 state 文件传递。
  Bash 工具每次调用都是新 shell，shell 变量不跨步骤保留；直接引用会展开成空串，
  最严重的后果是第 6 节回退拿不到备份目录而无法执行。

## 1. 准备精确的 main 源码

1. 运行 `git status --porcelain`。存在任何已跟踪或未跟踪改动就停止；不得部署未提交工作区。
2. 安全切换并快进本地 `main`：
   ```bash
   git switch main
   git fetch origin main
   git merge --ff-only origin/main
   ```
   切换或快进失败就停止；不得 reset、丢弃或覆盖本地改动。
3. 记录部署 SHA 和主题，并**写入 state 文件**（后续每一步都从这里读回）：
   ```bash
   deploy_sha=$(git rev-parse HEAD)
   deploy_tree=$(git rev-parse 'HEAD^{tree}')
   git show -s --format='%H %s' "$deploy_sha"
   test "$(git branch --show-current)" = main
   test "$deploy_sha" = "$(git rev-parse origin/main)"
   test -z "$(git status --porcelain)"

   # 覆盖写（不是追加）：确保不会沿用上一次部署的残留值
   mkdir -p ~/.config/tsz-rust
   cat > ~/.config/tsz-rust/deploy-state.env <<EOF
deploy_sha=$deploy_sha
deploy_tree=$deploy_tree
EOF
   ```

   此后**每个** Bash 步骤都以这三行开头读回状态；第三行兼作防残留/防串号校验
   （state 文件路径一律写字面量，它自己也是跨不了步骤的；放用户私有目录而非 `/tmp`，
   因为 `.` 会把文件内容当 shell 代码执行，与 `deploy-smoke.env` 同目录也更一致）：
   ```bash
   set -a; . ~/.config/tsz-rust/deploy-state.env; set +a
   test -n "${deploy_sha:-}"
   test "$deploy_sha" = "$(git rev-parse HEAD)"   # 残留或串号立即暴露
   ```

## 2. 强制等待 GitHub main CI 成功

调用技能内的确定性门禁脚本；默认最多等待 30 分钟、每 15 秒查询一次：

```bash
set -a; . ~/.config/tsz-rust/deploy-state.env; set +a
.claude/skills/deploy/scripts/require-green-main.sh "$deploy_sha"
```

运行期间持续向用户报告等待状态，不要让用户超过 60 秒收不到进展。仅退出码 `0` 允许继续：

- `0`：精确 SHA 的最新 `CI` 工作流已完成，且结论为 `success`；
- `1`：CI 已完成但失败或为非成功结论，停止部署；
- `2`：工作区/分支不安全、无 CI、查询/认证/状态异常或等待超时，停止部署；
- `3`：等待期间 `origin/main` 变化，回到第 1 步同步新提交并重新检查。

门禁成功后、任何服务器写操作前，再做一次防竞态复核：

```bash
set -a; . ~/.config/tsz-rust/deploy-state.env; set +a
git fetch origin main
test "$(git branch --show-current)" = main
test "$(git rev-parse HEAD)" = "$deploy_sha"
test "$(git rev-parse origin/main)" = "$deploy_sha"
test -z "$(git status --porcelain)"
```

任一检查失败就停止并重新走 CI 门禁。

同时读取门禁刚验证的精确 CI run，供部署 manifest 使用；结果必须与门禁的 success run一致：

```bash
set -a; . ~/.config/tsz-rust/deploy-state.env; set +a
ci_run=$(
  gh api "repos/LonelyFellas/tsz-rust/actions/runs?branch=main&head_sha=$deploy_sha&per_page=100" \
    --jq '[.workflow_runs[] | select(.name == "CI")] | sort_by(.created_at) | last | [.id, .status, .conclusion, .html_url] | @tsv'
)
IFS=$'\t' read -r ci_run_id ci_status ci_conclusion ci_run_url <<<"$ci_run"
test "$ci_status" = completed
test "$ci_conclusion" = success
test "$ci_run_url" = "https://github.com/LonelyFellas/tsz-rust/actions/runs/$ci_run_id"

cat >> ~/.config/tsz-rust/deploy-state.env <<EOF
ci_run_id=$ci_run_id
ci_run_url=$ci_run_url
EOF
```

## 3. 其他前置检查

1. **sqlx 离线缓存**：服务器用 `SQLX_OFFLINE=true` 编译；CI 已用提交的 `.sqlx/` 离线缓存
   编译并检查。此时不得在本地刷新或修改缓存，否则工作区不再等于已验证提交。
2. **ssh 连通性**：`ssh -o ConnectTimeout=8 tshb-test echo ok`。连不上的两大原因（都遇过）：
   - 本机 Clash/Surge 开着 TUN 模式，把 ssh 劫走非白名单出口 → 让用户关代理或给
     47.121.142.19 加 DIRECT 规则；
   - 家用动态 IP 变了，Aliyun 安全组白名单失效 → 让用户去控制台给新 IP 加 /32。

   这一项必须排在下面的 manifest 核对**之前**：ssh 连不上时 `cat` 会失败并被兜底成
   空 JSON，看起来就像「服务器没有 manifest」，从而放行一次本该被拦下的空跑。
3. **现有部署来源核对（动服务器状态之前必做）**：读服务器上的正式 manifest，
   由脚本判定是否与本次目标相同——不要人眼比对两个 40 字符 SHA：
   ```bash
   set -a; . ~/.config/tsz-rust/deploy-state.env; set +a
   deployed_sha=$(ssh tshb-test 'cat /opt/tsz-deploy-manifests/api.json 2>/dev/null || echo "{}"' \
     | python3 -c 'import json,sys; print(json.load(sys.stdin).get("source",{}).get("git_sha",""))')
   echo "服务器当前: ${deployed_sha:-<无 manifest>}"
   echo "本次目标:   $deploy_sha"
   if [ "$deployed_sha" = "$deploy_sha" ]; then
     echo "!! 该提交已经部署过 —— 停下来向用户报告并确认是否仍要重跑"
   else
     echo "OK 与服务器当前来源不同，可继续"
   fi
   ```
   打印 `!!` 那行就**停下来**向用户确认，不要默认继续。多个 session 并行时，你对生产
   当前版本的记忆会过期——别用记忆代替这一步。空跑一轮的代价不只是白费时间：第 4 节
   撤下旧 manifest 后，服务器会陷入「有二进制、无来源记录」的半状态，必须重启服务走完
   全流程才能恢复（2026-08-20 实际发生过）。

## 4. 部署步骤

```bash
# 1) 把当前二进制与配套 manifest 成组备份（回退保险）
deploy_backup_dir=$(ssh tshb-test '
  set -eu
  base=/opt/tsz-rust/deploy-backups
  install -d -m 0755 "$base"
  dir=$(mktemp -d "$base/$(date -u +%Y%m%dT%H%M%SZ).XXXXXX")
  cp /opt/tsz-rust/target/release/tsz-rust "$dir/tsz-rust"
  if test -f /opt/tsz-deploy-manifests/api.json; then
    cp /opt/tsz-deploy-manifests/api.json "$dir/api.json"
  else
    : > "$dir/manifest.absent"
  fi
  printf "%s\n" "$dir"
')
test -n "$deploy_backup_dir"
# 立刻落盘：第 5.1 节 validate/verify 与第 6 节回退都要用，丢了就没法回退
cat >> ~/.config/tsz-rust/deploy-state.env <<EOF
deploy_backup_dir=$deploy_backup_dir
EOF

# 2) 先安装本提交的原子 restore/manifest 工具，再撤下旧正式 manifest。
#    从这里到新 manifest verify 完成，任一步失败都必须立即执行第 6 节 restore 并停止。
ssh tshb-test 'install -d -m 0755 /opt/tsz-deploy-tools /opt/tsz-deploy-manifests'
rsync -az ops/deployment_manifest.py tshb-test:/opt/tsz-deploy-tools/backend-deployment-manifest.py
ssh tshb-test 'python3 /opt/tsz-deploy-tools/backend-deployment-manifest.py --help >/dev/null'
ssh tshb-test "python3 /opt/tsz-deploy-tools/backend-deployment-manifest.py validate-backup \
  --backup-dir '$deploy_backup_dir'"
ssh tshb-test 'rm -f /opt/tsz-deploy-manifests/api.json'

# 3) 推代码。排除项一个都不能少：
#    .env 是生产密钥(600 权限,含 JWT_SECRET/COOKIE_SECURE)——rsync 覆盖或 --delete 掉它=事故;
#    target 巨大且服务器要自己编译; 绝不使用 --delete;
#    .claude/worktrees 是本机 worktree 的整份源码副本(数百个文件),服务器只编译仓库根,
#    推上去纯属浪费并会在服务器留下另一个分支的代码,容易误读;
#    rust-toolchain.toml 钉的是本地与 CI 的编译器版本,服务器只跑 cargo build 不跑 lint,
#    版本一致对它没有价值。推上去反而会让 rustup 去下一整套具名工具链——服务器上只有
#    stable 一套,任何具名版本(哪怕版本号相同)都算另一套安装,而它到 static.rust-lang.org
#    实测只有约 29 KB/s,几小时起步。更糟的是这一步在「撤下旧 manifest」之后,卡住会把
#    服务器留在「有二进制、无来源记录」的半状态。
#    ⚠️ 由此带来一个有意接受的缺口:服务器用它自己的 stable(现为 1.97.0),与 CI 验证过的
#    1.98.0 不是同一个编译器。若出现「CI 全绿但服务器编译失败」,先想到这一点,别一头扎进
#    代码里找原因。
rsync -az --exclude .git --exclude target --exclude .env --exclude .vscode \
  --exclude .claude/worktrees --exclude rust-toolchain.toml \
  /Users/darwish/Dev/tsz-core/tsz-rust/ tshb-test:/opt/tsz-rust/

# 4) 服务器编译(后台跑,增量 1-2 分钟,全量约 7 分钟)。
#    机器只有 3.4G 内存+2G swap,JOBS=2 防 OOM,别调高。
ssh tshb-test 'cd /opt/tsz-rust && bash -o pipefail -c "CARGO_BUILD_JOBS=2 SQLX_OFFLINE=true ~/.cargo/bin/cargo build --release 2>&1 | tail -3"'

# 5) 重启 + 基础确认
ssh tshb-test 'systemctl restart tsz-rust && sleep 2 && systemctl is-active tsz-rust && curl -s http://127.0.0.1:8383/healthz'
```

编译步骤用后台执行（Bash run_in_background），完成后再继续,不要阻塞等待。

## 5. 冒烟验证（部署不验证 = 没部署）

服务器本机跑,B=`http://127.0.0.1:8383/api/v1`：

1. `healthz` → `{"status":"ok"}`；
2. `readyz` → **200 `{"status":"ready"}`**，同时证明 DB 与 Redis 可用；
3. `POST $B/auth/refresh`（无 cookie）→ **401 RFC 9457 Problem，稳定判据是
   `code == "invalid_refresh_token"`**（响应体早已迁到 Problem Details，不再是
   `{"error":...}`；判断形状而不是逐字比对 `detail`）。若拿到 422 或旧的
   `{"error":...}` 形状，说明跑的不是本次构建；注意 422 现在的语义是
   `invalid_request_body`，见 `docs/api-errors.md`；
4. 全链路（使用常驻冒烟账号；先执行
   `set -a; source ~/.config/tsz-rust/deploy-smoke.env; set +a`，再从环境变量
   `TSZ_SMOKE_IDENTIFIER` / `TSZ_SMOKE_PASSWORD` 读取凭据；禁止写入仓库、命令参数或日志）：
   login 请求体字段是 **`identifier`**（统一承接手机号/邮箱，不叫 `phone`——用错必 422）：
   ```json
   {"identifier": "$TSZ_SMOKE_IDENTIFIER", "password": "$TSZ_SMOKE_PASSWORD"}
   ```
   login 200 且 Set-Cookie 含 `HttpOnly; SameSite=Lax; Path=/api/v1/auth`（**不含
   Secure**——服务器 .env 设了 COOKIE_SECURE=false，见下）→ 拿 cookie 刷新 200 且
   轮换出新值 → 带新 cookie logout 204；
5. 若本次改动含**新迁移**：`journalctl -u tsz-rust -n 50 | grep -i migrat` 确认
   「database migrations applied」（启动自动迁移,连的是外部 RDS）。

把冒烟结果整理成表格报告给用户。

### 5.1 发布并验证 API 部署 manifest

只有第 5 节所有 smoke 全部 PASS 后才执行。所有变量均来自上面的严格校验，不接收自由文本：

```bash
set -a; . ~/.config/tsz-rust/deploy-state.env; set +a
test "$deploy_sha" = "$(git rev-parse HEAD)"
test -n "$ci_run_id" && test -n "$deploy_tree"

ssh tshb-test \
  "python3 /opt/tsz-deploy-tools/backend-deployment-manifest.py create \
    --component api \
    --repository LonelyFellas/tsz-rust \
    --git-sha '$deploy_sha' \
    --git-tree '$deploy_tree' \
    --ci-run-id '$ci_run_id' \
    --ci-run-url '$ci_run_url' \
    --artifact /opt/tsz-rust/target/release/tsz-rust \
    --artifact-path /opt/tsz-rust/target/release/tsz-rust \
    --output /opt/tsz-deploy-manifests/api.json"

ssh tshb-test '
  python3 /opt/tsz-deploy-tools/backend-deployment-manifest.py verify \
    --manifest /opt/tsz-deploy-manifests/api.json \
    --artifact /opt/tsz-rust/target/release/tsz-rust
'
```

create 使用同目录临时文件 + 原子 rename；manifest 只包含 Git/CI/制品摘要，不包含环境变量或认证材料。输出中的 `git_sha`、`ci_run_id`、`artifact_sha256`、`accepted_at` 必须纳入部署报告。

## 6. 回退

从 state 文件读回第 4 节记录的精确 `deploy_backup_dir`，不得用通配符猜备份。
本节可能在紧急情况下单独执行，因此代码块自带加载：

```bash
set -a; . ~/.config/tsz-rust/deploy-state.env; set +a
test -n "${deploy_backup_dir:-}"   # 为空说明 state 丢失，停下来人工确认备份目录，不要猜

ssh tshb-test "set -eu
  python3 /opt/tsz-deploy-tools/backend-deployment-manifest.py restore \
    --backup-dir '$deploy_backup_dir' \
    --artifact /opt/tsz-rust/target/release/tsz-rust \
    --manifest /opt/tsz-deploy-manifests/api.json
  systemctl restart tsz-rust"
```

restore 会先验证备份组，再撤下正式 manifest，以同目录临时文件 + `os.replace` 原子换回二进制，最后才恢复旧 manifest；这避免覆盖运行中二进制的 `Text file busy` 和半恢复错配。第 4 节撤下旧 manifest 后，编译、重启、任一 smoke、create 或 verify 失败都必须执行本节，不得保留“新二进制 + 旧 manifest”。

回退后重跑 health/ready/auth smoke；若恢复了 `api.json`，必须执行 `deployment_manifest.py verify`。若旧部署没有 manifest，回退后的来源状态明确为 UNKNOWN/BLOCKED，不能伪造一个 SHA。

代码文件不用回退（下次 rsync 会覆盖），二进制换回即服务回退。含迁移的改动回退要慎重——
迁移已作用于 RDS，回退二进制前先确认旧代码兼容新 schema。

## 7. 环境变量

`/opt/tsz-rust/.env`（rsync 永远排除，改它只能 ssh 手动）当前含：
PORT / DATABASE_URL（外部 Aliyun RDS）/ JWT_SECRET / REDIS_URL / **COOKIE_SECURE=false**
（域名备案中、无 TLS 的临时项；备案后上 Caddy TLS 时删除恢复默认 true）。
新增配置项时记得同步：`.env.example`、`docs/deployment.md` §3、以及服务器 .env。
