---
name: deploy
description: 仅在 GitHub origin/main 当前提交的 CI 成功后，通过 ssh 把该提交自动化部署到生产服务器 tshb-test（CI 门禁 → 取 CI 产物 → 原子替换二进制 → 重启 → 冒烟验证 → 可回退）。用户说「部署」「deploy」「推到服务器」「上线」「更新服务器代码」或提到 tshb-test / 47.121.142.19 的代码同步时使用。
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
- 本地与服务器都必须持有带 owner 的部署锁；发现已有锁时只读报告 owner 并停止，绝不
  自动抢锁或删除。state、staging、backup 和 server lock 的 owner 必须属于同一 session。

## 1. 准备精确的 main 源码

1. 运行 `git status --porcelain`。存在任何已跟踪或未跟踪改动就停止；不得部署未提交工作区。
2. 安全切换并快进本地 `main`：
   ```bash
   set -euo pipefail
   git switch main
   git fetch origin main
   git merge --ff-only origin/main
   ```
   切换或快进失败就停止；不得 reset、丢弃或覆盖本地改动。
3. 原子取得本地部署锁，记录部署 SHA 和主题，并把 session/state 放在锁目录内：
   ```bash
   set -euo pipefail
   deploy_sha=$(git rev-parse HEAD)
   deploy_tree=$(git rev-parse 'HEAD^{tree}')
   git show -s --format='%H %s' "$deploy_sha"
   test "$(git branch --show-current)" = main
   test "$deploy_sha" = "$(git rev-parse origin/main)"
   test -z "$(git status --porcelain)"

   # mkdir 是本地互斥原语；已有锁可能代表另一个部署或待恢复事故，只读报告后停止。
   mkdir -p ~/.config/tsz-rust
   deploy_lock_dir=~/.config/tsz-rust/deploy.lock
   if ! mkdir "$deploy_lock_dir"; then
     printf '已有部署锁，owner=%s\n' "$(cat "$deploy_lock_dir/owner" 2>/dev/null || printf unknown)"
     exit 1
   fi
   deploy_session=$(python3 -c 'import uuid; print(uuid.uuid4())')
   state_file="$deploy_lock_dir/state.env"
   session_tools_root=~/.cache/tsz-rust-deploy-tools
   session_tools="$session_tools_root/$deploy_session"
   mkdir -p "$session_tools_root"
   mkdir "$session_tools"
   git show "$deploy_sha:.claude/skills/deploy/scripts/require-green-main.sh" \
     > "$session_tools/require-green-main.sh"
   chmod 0755 "$session_tools/require-green-main.sh"
   printf '%s\n' "$deploy_session" > "$deploy_lock_dir/owner"
   printf 'deploy_session=%q\ndeploy_sha=%q\ndeploy_tree=%q\nsession_tools=%q\n' \
     "$deploy_session" "$deploy_sha" "$deploy_tree" "$session_tools" > "$state_file"
   ```

   此后**每个** Bash 步骤都从锁目录的 state 读回状态，并用独立 owner 文件核对 session。
   state 不再与另一个部署共享；stale lock 必须人工核对服务器 manifest/binary/backup 后处理：
   ```bash
   set -euo pipefail
   set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
   test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
   test -n "${deploy_session:-}" && test -n "${deploy_sha:-}"
   test "$deploy_sha" = "$(git rev-parse HEAD)"   # 残留或串号立即暴露
   ```

   在**尚未取得服务器锁**时若 CI、artifact、SSH 或只读预检阻断，核对 owner 后只删除本
   session 的 state/owner 并 `rmdir` 空锁目录；不得 `rm -rf`，unique staging 保留供诊断：
   ```bash
   set -euo pipefail
   set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
   test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
   rm -f ~/.config/tsz-rust/deploy.lock/state.env ~/.config/tsz-rust/deploy.lock/owner
   rmdir ~/.config/tsz-rust/deploy.lock
   ```

## 2. 强制等待 GitHub main CI 成功

调用技能内的确定性门禁脚本；默认最多等待 30 分钟、每 15 秒查询一次：

```bash
set -euo pipefail
set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
test -x "$session_tools/require-green-main.sh"
"$session_tools/require-green-main.sh" "$deploy_sha"
```

运行期间持续向用户报告等待状态，不要让用户超过 60 秒收不到进展。仅退出码 `0` 允许继续：

- `0`：精确 SHA 的最新 `CI` 工作流已完成，且结论为 `success`；
- `1`：CI 已完成但失败或为非成功结论，停止部署；
- `2`：工作区/分支不安全、无 CI、查询/认证/状态异常或等待超时，停止部署；
- `3`：等待期间 `origin/main` 变化，回到第 1 步同步新提交并重新检查。

门禁成功后、任何服务器写操作前，再做一次防竞态复核：

```bash
set -euo pipefail
set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
git fetch origin main
test "$(git branch --show-current)" = main
test "$(git rev-parse HEAD)" = "$deploy_sha"
test "$(git rev-parse origin/main)" = "$deploy_sha"
test -z "$(git status --porcelain)"
```

任一检查失败就停止并重新走 CI 门禁。

同时读取门禁刚验证的精确 CI run，供部署 manifest 使用；结果必须与门禁的 success run一致：

```bash
set -euo pipefail
set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
ci_run=$(
  gh api "repos/LonelyFellas/tsz-rust/actions/runs?branch=main&head_sha=$deploy_sha&per_page=100" \
    --jq '[.workflow_runs[] | select(.name == "CI")] | sort_by(.created_at) | last | [.id, .run_attempt, .status, .conclusion, .html_url] | @tsv'
)
IFS=$'\t' read -r ci_run_id ci_run_attempt ci_status ci_conclusion ci_run_url <<<"$ci_run"
case "$ci_run_id" in ''|*[!0-9]*) exit 1 ;; esac
case "$ci_run_attempt" in ''|*[!0-9]*) exit 1 ;; esac
test "$ci_run_id" -gt 0 && test "$ci_run_attempt" -gt 0
test "$ci_status" = completed
test "$ci_conclusion" = success
test "$ci_run_url" = "https://github.com/LonelyFellas/tsz-rust/actions/runs/$ci_run_id"

printf 'ci_run_id=%q\nci_run_attempt=%q\nci_run_url=%q\n' \
  "$ci_run_id" "$ci_run_attempt" "$ci_run_url" \
  >> ~/.config/tsz-rust/deploy.lock/state.env
```

## 3. 其他前置检查

1. **CI 产物下载与完整验证（动服务器状态之前必查）**：本次部署装的是精确 run attempt
   的产物。必须在任何服务器写入之前完成唯一性、过期状态、SHA256、manifest、SHA/tree/run/
   attempt 验证；任一项失败都释放本地锁并停止：
   ```bash
   set -euo pipefail
   set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
   test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
   artifact_name="tsz-rust-x86_64-linux-gnu-$deploy_sha-attempt-$ci_run_attempt"
   [[ "$artifact_name" =~ ^tsz-rust-x86_64-linux-gnu-[0-9a-f]{40}-attempt-[1-9][0-9]*$ ]]
   artifact_record=$(
     gh api "repos/LonelyFellas/tsz-rust/actions/runs/$ci_run_id/artifacts" \
       --jq ".artifacts[] | select(.name == \"$artifact_name\") | [.name, .expired, .size_in_bytes] | @tsv"
   )
   test "$(printf '%s\n' "$artifact_record" | sed '/^$/d' | wc -l | tr -d ' ')" = 1
   IFS=$'\t' read -r found_name artifact_expired artifact_size <<<"$artifact_record"
   test "$found_name" = "$artifact_name"
   test "$artifact_expired" = false
   test "$artifact_size" -gt 0
   printf 'artifact=%s size=%s bytes\n' "$found_name" "$artifact_size"
   staging_root=~/.cache/tsz-rust-artifacts
   staging="$staging_root/$deploy_session"
   mkdir -p "$staging_root"
   mkdir "$staging"
   tools="$staging/tools"
   mkdir "$tools"
   for tool_path in \
     ops/ci_fingerprint.py \
     ops/ci_metrics.py \
     ops/deployment_manifest.py \
     ops/deployment_preflight.py \
     ops/release_artifact_manifest.py
   do
     git show "$deploy_sha:$tool_path" > "$tools/${tool_path##*/}"
   done
   python3 "$tools/ci_metrics.py" run --name deploy-artifact-download -- \
     gh run download "$ci_run_id" --repo LonelyFellas/tsz-rust \
       --name "$artifact_name" --dir "$staging"
   test -s "$staging/tsz-rust"
   chmod +x "$staging/tsz-rust"
   ( cd "$staging" && shasum -a 256 -c tsz-rust.sha256 )
   python3 "$tools/release_artifact_manifest.py" verify \
     --manifest "$staging/tsz-rust.manifest.json" \
     --artifact "$staging/tsz-rust" \
     --expected-git-sha "$deploy_sha" \
     --expected-git-tree "$deploy_tree" \
     --expected-run-id "$ci_run_id" \
     --expected-run-attempt "$ci_run_attempt"
   printf 'artifact_name=%q\nartifact_size=%q\nstaging=%q\ntools=%q\n' \
     "$artifact_name" "$artifact_size" "$staging" "$tools" \
     >> ~/.config/tsz-rust/deploy.lock/state.env
   ```
   无输出或 `expired` 为 `true` 就停止：前者说明该提交早于「CI 产物部署」上线（或
   release-artifact job 被跳过），后者说明超过了 30 天保留期。两种情况都不要绕过校验硬来，
   先把 main 前进到含该 job 的提交、或重跑 CI 生成新产物。
   本地 `.sqlx/` 此时不得刷新或修改——工作区必须仍等于已验证提交，第 5.1 节的 `git_tree` 依赖它。
2. **ssh 连通性**：`ssh -o ConnectTimeout=8 tshb-test echo ok`。连不上的两大原因（都遇过）：
   - 本机 Clash/Surge 开着 TUN 模式，把 ssh 劫走非白名单出口 → 让用户关代理或给
     47.121.142.19 加 DIRECT 规则；
   - 家用动态 IP 变了，Aliyun 安全组白名单失效 → 让用户去控制台给新 IP 加 /32。

   这一项必须排在下面的 manifest 核对**之前**：ssh 连不上时 `cat` 会失败并被兜底成
   空 JSON，看起来就像「服务器没有 manifest」，从而放行一次本该被拦下的空跑。
3. **数据库只读预检（动服务器状态之前必做）**：必须运行仓库内的确定性工具，不能把
   多个 `SELECT` 拼进一个 `psql -c` 后再把空输出解释成 0。工具会对每个标量独立调用
   psql，并把空值、非整数、多行或查询失败全部视为阻断；只输出计数 JSON，不输出 DSN：
   ```bash
   set -euo pipefail
   set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
   test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
   test -s "$tools/deployment_preflight.py"
   deployment_preflight=$(
     ssh tshb-test 'set -a; . /opt/tsz-rust/.env; set +a; python3 -' \
       < "$tools/deployment_preflight.py"
   )
   python3 -c '
import json,sys
value=json.load(sys.stdin)
expected={"schema_version","entries","v3_entries","successful_migrations","latest_migration"}
assert set(value)==expected
print(json.dumps(value,sort_keys=True,separators=(",",":")))
' <<<"$deployment_preflight"
   ```
   这份快照是事实记录，不代表自动要求 entries 为 0；实际 migration 约束仍由 migration
   自身 fail closed，不能为了让部署通过而在预检中写库或清数据。
4. **现有部署来源核对（动服务器状态之前必做）**：读服务器上的正式 manifest，
   由脚本判定是否与本次目标相同——不要人眼比对两个 40 字符 SHA：
   ```bash
   set -euo pipefail
   set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
   test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
   deployed_sha=$(ssh tshb-test 'cat /opt/tsz-deploy-manifests/api.json 2>/dev/null || echo "{}"' \
     | python3 -c 'import json,sys; print(json.load(sys.stdin).get("source",{}).get("git_sha",""))')
   if [ -n "$deployed_sha" ] && ! [[ "$deployed_sha" =~ ^[0-9a-f]{40}$ ]]; then
     echo "服务器 manifest git_sha 非法，停止"
     exit 1
   fi
   echo "服务器当前: ${deployed_sha:-<无 manifest>}"
   echo "本次目标:   $deploy_sha"
   if [ "$deployed_sha" = "$deploy_sha" ]; then
     echo "!! 该提交已经部署过 —— 停下来向用户报告并确认是否仍要重跑"
   else
     echo "OK 与服务器当前来源不同，可继续"
   fi
   printf 'deployed_sha=%q\n' "$deployed_sha" >> ~/.config/tsz-rust/deploy.lock/state.env
   ```
   打印 `!!` 那行就**停下来**向用户确认，不要默认继续。多个 session 并行时，你对生产
   当前版本的记忆会过期——别用记忆代替这一步。空跑一轮的代价不只是白费时间：第 4 节
   撤下旧 manifest 后，服务器会陷入「有二进制、无来源记录」的半状态，必须重启服务走完
   全流程才能恢复（2026-08-20 实际发生过）。

## 4. 部署步骤

```bash
set -euo pipefail
set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
test -n "${deploy_sha:-}" && test -n "${deploy_tree:-}"
test -n "${ci_run_id:-}" && test -n "${ci_run_attempt:-}"
test -n "${artifact_name:-}" && test -n "${staging:-}" && test -n "${tools:-}"
test -s "$staging/tsz-rust" && test -s "$staging/tsz-rust.manifest.json"
test -s "$tools/deployment_manifest.py" && test -s "$tools/ci_metrics.py"

# 1) artifact 已在第 3 节下载并完整验证。现在才取得服务器互斥锁，这是本流程第一次
#    服务器写入；已有锁只读报告 owner 后停止，绝不抢锁。
server_lock_result=$(ssh tshb-test "set -eu
  lock=/opt/tsz-rust/deploy.lock
  if mkdir \"\$lock\"; then
    printf '%s\n' '$deploy_session' > \"\$lock/owner\"
    printf '%s\n' '$deploy_sha' > \"\$lock/git_sha\"
    printf acquired
  else
    printf 'busy owner=%s\n' \"\$(cat \"\$lock/owner\" 2>/dev/null || printf unknown)\" >&2
    exit 1
  fi")
test "$server_lock_result" = acquired
cat >> ~/.config/tsz-rust/deploy.lock/state.env <<EOF
server_lock_acquired=true
EOF

# 锁内重读正式 manifest，堵住只读预检与取得锁之间的竞态。
locked_deployed_sha=$(ssh tshb-test 'cat /opt/tsz-deploy-manifests/api.json 2>/dev/null || echo "{}"' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin).get("source",{}).get("git_sha",""))')
if [ "$locked_deployed_sha" != "$deployed_sha" ]; then
  ssh tshb-test "set -eu; test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'; \
    rm -f /opt/tsz-rust/deploy.lock/owner /opt/tsz-rust/deploy.lock/git_sha; \
    rmdir /opt/tsz-rust/deploy.lock"
  rm -f ~/.config/tsz-rust/deploy.lock/state.env ~/.config/tsz-rust/deploy.lock/owner
  rmdir ~/.config/tsz-rust/deploy.lock
  echo "服务器版本在锁前发生变化；释放本 session 锁并从头重新门禁"
  exit 1
fi

# 2) 把当前二进制与配套 manifest 成组备份（回退保险）
deploy_backup_dir=$(ssh tshb-test "
  set -eu
  test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'
  base=/opt/tsz-rust/deploy-backups
  install -d -m 0755 \"\$base\"
  dir=\$(mktemp -d \"\$base/\$(date -u +%Y%m%dT%H%M%SZ).XXXXXX\")
  cp /opt/tsz-rust/target/release/tsz-rust \"\$dir/tsz-rust\"
  if test -f /opt/tsz-deploy-manifests/api.json; then
    cp /opt/tsz-deploy-manifests/api.json \"\$dir/api.json\"
  else
    : > \"\$dir/manifest.absent\"
  fi
  printf '%s\n' \"\$dir\"")
test -n "$deploy_backup_dir"
[[ "$deploy_backup_dir" =~ ^/opt/tsz-rust/deploy-backups/[A-Za-z0-9._-]+$ ]]
# 立刻落盘：第 5.1 节 validate/verify 与第 6 节回退都要用，丢了就没法回退
printf 'deploy_backup_dir=%q\n' "$deploy_backup_dir" \
  >> ~/.config/tsz-rust/deploy.lock/state.env

# 3) 先安装本提交的原子 restore/manifest 工具，再撤下旧正式 manifest。
#    从这里到新 manifest verify 完成，任一步失败都必须立即执行第 6 节 restore 并停止。
deploy_tool_incoming="/opt/tsz-deploy-tools/backend-deployment-manifest.incoming.$deploy_session"
ssh tshb-test "set -eu
  test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'
  install -d -m 0755 /opt/tsz-deploy-tools /opt/tsz-deploy-manifests"
rsync -az "$tools/deployment_manifest.py" "tshb-test:$deploy_tool_incoming"
ssh tshb-test "set -eu
  test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'
  python3 '$deploy_tool_incoming' --help >/dev/null
  python3 '$deploy_tool_incoming' validate-backup --backup-dir '$deploy_backup_dir'
  mv -f '$deploy_tool_incoming' /opt/tsz-deploy-tools/backend-deployment-manifest.py
  rm -f /opt/tsz-deploy-manifests/api.json"

# 4) 安装到服务器。**不能直接 cp 覆盖运行中的二进制**（ETXTBSY），
#    先传到同目录临时名，再用 mv 原子替换（rename 会解除旧 inode 的链接，进程不受影响）。
binary_incoming="/opt/tsz-rust/target/release/tsz-rust.incoming.$deploy_session"
ssh tshb-test "set -eu
  test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'
  install -d -m 0755 /opt/tsz-rust/target/release"
python3 "$tools/ci_metrics.py" run --name deploy-binary-rsync -- \
  rsync -az "$staging/tsz-rust" "tshb-test:$binary_incoming"
ssh tshb-test "set -eu
  test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'
  chmod 0755 '$binary_incoming'
  mv -f '$binary_incoming' /opt/tsz-rust/target/release/tsz-rust"

# 服务器不再编译；固定记录 0，避免把下载/rsync 时间误报成服务器 build。
python3 "$tools/ci_metrics.py" record --name deploy-server-build \
  --value 0 --unit milliseconds

# 5) 重启 + 基础确认
ssh tshb-test "set -eu
  test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'
  systemctl restart tsz-rust
  sleep 2
  systemctl is-active tsz-rust
  curl -s http://127.0.0.1:8383/healthz"
```

取得服务器锁后、撤下正式 manifest 前若备份或工具安装失败，先确认 binary/manifest 未变，
再按 owner 校验释放服务器锁和本地锁；备份与 unique staging 保留供诊断。撤下正式 manifest
之后的任何失败必须走第 6 节回退，回退 smoke 全绿前不得释放锁。执行任务异常中断时锁必须
保留，后续任务只读检查 owner/state/backup/manifest 后请求决定，绝不自动抢锁。

**不再向服务器推送源码**：产物由 CI 编译，而 `include_str!` 与 `sqlx::migrate!` 都是编译期内嵌，
服务器运行期只需要二进制与 `/opt/tsz-rust/.env`。这一并消掉了原先那串排除项
（`.claude/worktrees`、`rust-toolchain.toml` 等）和「服务器编译器与 CI 不同版本」的缺口。
`/opt/tsz-rust/target/` 现在只是二进制的存放目录，不再有构建产物写入。

制品路径仍是 `/opt/tsz-rust/target/release/tsz-rust`：`ops/deployment_manifest.py` 里
`EXPECTED_ARTIFACT_PATH` 硬编码了它，第 6 节的备份/回退也依赖它。路径语义有点怪
（一个不再构建的 `target/`），但换路径要同时动 manifest 契约与既有备份，不值得。

部署报告必须保留上面三个 JSON 指标：artifact 下载耗时、rsync 耗时和
`deploy-server-build=0 milliseconds`，并同时报告下载 artifact 的 GitHub 元数据大小与
manifest 中的原始 binary 字节数。两者分别是压缩上传包与二进制大小，不能混为一个值。

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
set -euo pipefail
set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
test "$deploy_sha" = "$(git rev-parse HEAD)"
test -n "$ci_run_id" && test -n "$ci_run_attempt" && test -n "$deploy_tree"

ssh tshb-test \
  "set -eu
   test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'
   python3 /opt/tsz-deploy-tools/backend-deployment-manifest.py create \
    --component api \
    --repository LonelyFellas/tsz-rust \
    --git-sha '$deploy_sha' \
    --git-tree '$deploy_tree' \
    --ci-run-id '$ci_run_id' \
    --ci-run-url '$ci_run_url' \
    --artifact /opt/tsz-rust/target/release/tsz-rust \
    --artifact-path /opt/tsz-rust/target/release/tsz-rust \
    --output /opt/tsz-deploy-manifests/api.json"

ssh tshb-test "set -eu
  test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'
  python3 /opt/tsz-deploy-tools/backend-deployment-manifest.py verify \
    --manifest /opt/tsz-deploy-manifests/api.json \
    --artifact /opt/tsz-rust/target/release/tsz-rust"

# 新 manifest 验证完成后才释放服务器锁；再释放同 owner 的本地锁。unique staging 保留供审计。
ssh tshb-test "set -eu; test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'; \
  rm -f /opt/tsz-rust/deploy.lock/owner /opt/tsz-rust/deploy.lock/git_sha; \
  rmdir /opt/tsz-rust/deploy.lock"
test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
rm -f ~/.config/tsz-rust/deploy.lock/state.env ~/.config/tsz-rust/deploy.lock/owner
rmdir ~/.config/tsz-rust/deploy.lock
printf '保留已验证 staging: %s\n' "$staging"
```

create 使用同目录临时文件 + 原子 rename；manifest 只包含 Git/CI/制品摘要，不包含环境变量或认证材料。输出中的 `git_sha`、`ci_run_id`、`artifact_sha256`、`accepted_at` 必须纳入部署报告。

## 6. 回退

从本 session 的锁内 state 读回第 4 节记录的精确 `deploy_backup_dir`，不得用通配符猜备份。
本节可能在紧急情况下单独执行，因此代码块自带加载：

```bash
set -euo pipefail
set -a; . ~/.config/tsz-rust/deploy.lock/state.env; set +a
test "$(cat ~/.config/tsz-rust/deploy.lock/owner)" = "$deploy_session"
test -n "${deploy_backup_dir:-}"   # 为空说明 state 丢失，停下来人工确认备份目录，不要猜

ssh tshb-test "set -eu
  test \"\$(cat /opt/tsz-rust/deploy.lock/owner)\" = '$deploy_session'
  python3 /opt/tsz-deploy-tools/backend-deployment-manifest.py restore \
    --backup-dir '$deploy_backup_dir' \
    --artifact /opt/tsz-rust/target/release/tsz-rust \
    --manifest /opt/tsz-deploy-manifests/api.json
  systemctl restart tsz-rust"
```

restore 会先验证备份组，再撤下正式 manifest，以同目录临时文件 + `os.replace` 原子换回二进制，最后才恢复旧 manifest；这避免覆盖运行中二进制的 `Text file busy` 和半恢复错配。第 4 节撤下旧 manifest 后，编译、重启、任一 smoke、create 或 verify 失败都必须执行本节，不得保留“新二进制 + 旧 manifest”。

回退后重跑 health/ready/auth smoke；若恢复了 `api.json`，必须执行 `deployment_manifest.py verify`。若旧部署没有 manifest，回退后的来源状态明确为 UNKNOWN/BLOCKED，不能伪造一个 SHA。只有回退 smoke/manifest 全部通过后，才按第 5.1 节相同的 owner 校验顺序释放服务器锁和本地锁；失败时保留锁、state、staging 与 backup 供恢复，绝不自动抢锁。

服务器上没有源码，二进制换回即服务回退。含迁移的改动回退要慎重——
迁移已作用于 RDS，回退二进制前先确认旧代码兼容新 schema。

## 7. 环境变量

`/opt/tsz-rust/.env`（rsync 永远排除，改它只能 ssh 手动）当前含：
PORT / DATABASE_URL（外部 Aliyun RDS）/ JWT_SECRET / REDIS_URL / **COOKIE_SECURE=false**
（域名备案中、无 TLS 的临时项；备案后上 Caddy TLS 时删除恢复默认 true）。
新增配置项时记得同步：`.env.example`、`docs/deployment.md` §3、以及服务器 .env。
