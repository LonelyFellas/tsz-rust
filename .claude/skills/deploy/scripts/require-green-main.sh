#!/usr/bin/env bash
set -euo pipefail

expected_sha="${1:-}"
timeout_seconds="${CI_WAIT_TIMEOUT_SECONDS:-1800}"
poll_seconds="${CI_POLL_INTERVAL_SECONDS:-15}"

if [[ ! "$expected_sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "CI gate: 需要传入完整的 40 位 origin/main commit SHA" >&2
  exit 2
fi

if [[ ! "$timeout_seconds" =~ ^[1-9][0-9]*$ ]] || [[ ! "$poll_seconds" =~ ^[1-9][0-9]*$ ]]; then
  echo "CI gate: 等待和轮询时间必须是正整数" >&2
  exit 2
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "CI gate: 未安装 GitHub CLI (gh)，无法确认 CI，禁止部署" >&2
  exit 2
fi

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

current_branch="$(git branch --show-current)"
current_head="$(git rev-parse HEAD)"

if [[ "$current_branch" != "main" ]]; then
  echo "CI gate: 当前分支是 ${current_branch}，不是 main，禁止部署" >&2
  exit 2
fi

if [[ "$current_head" != "$expected_sha" ]]; then
  echo "CI gate: 本地 HEAD $current_head 与待验证 SHA $expected_sha 不同，禁止部署" >&2
  exit 3
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "CI gate: 工作区存在未提交改动，部署源不等于已验证提交，禁止部署" >&2
  exit 2
fi

origin_url="$(git remote get-url origin)"
case "$origin_url" in
  git@github.com:*) github_repo="${origin_url#git@github.com:}" ;;
  ssh://git@github.com/*) github_repo="${origin_url#ssh://git@github.com/}" ;;
  https://github.com/*) github_repo="${origin_url#https://github.com/}" ;;
  *)
    echo "CI gate: origin 不是受支持的 GitHub 地址：$origin_url" >&2
    exit 2
    ;;
esac
github_repo="${github_repo%.git}"

if [[ ! "$github_repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "CI gate: 无法从 origin 解析 GitHub owner/repo：$origin_url" >&2
  exit 2
fi

remote_main_sha() {
  git ls-remote --exit-code origin refs/heads/main | awk 'NR == 1 { print $1 }'
}

deadline=$(( $(date +%s) + timeout_seconds ))

while true; do
  if ! current_main_sha="$(remote_main_sha)" || [[ -z "$current_main_sha" ]]; then
    echo "CI gate: 无法读取 GitHub origin/main，禁止部署" >&2
    exit 2
  fi

  if [[ "$current_main_sha" != "$expected_sha" ]]; then
    echo "CI gate: 等待期间 origin/main 已变化（${expected_sha} -> ${current_main_sha}），必须重新同步并检查新提交" >&2
    exit 3
  fi

  if ! run="$(
    gh api "repos/$github_repo/actions/runs?branch=main&head_sha=$expected_sha&per_page=100" \
      --jq '[.workflow_runs[] | select(.name == "CI")] | sort_by(.created_at) | last | if . == null then empty else [.id, .status, (.conclusion // "-"), .html_url] | @tsv end'
  )"; then
    echo "CI gate: GitHub API 查询失败，无法确认 CI，禁止部署" >&2
    exit 2
  fi

  if [[ -z "$run" ]]; then
    echo "CI gate: 尚未发现 $expected_sha 对应的 CI，等待工作流创建..."
  else
    IFS=$'\t' read -r run_id run_status run_conclusion run_url <<< "$run"
    case "$run_status" in
      completed)
        if [[ "$run_conclusion" != "success" ]]; then
          echo "CI gate: CI 已结束但结论为 ${run_conclusion}，禁止部署：${run_url}" >&2
          exit 1
        fi

        if ! current_main_sha="$(remote_main_sha)" || [[ "$current_main_sha" != "$expected_sha" ]]; then
          echo "CI gate: CI 成功后 origin/main 已变化，必须重新同步并检查新提交" >&2
          exit 3
        fi

        echo "CI gate: origin/main $expected_sha 的 CI 成功：$run_url"
        exit 0
        ;;
      queued|in_progress|pending|requested|waiting)
        echo "CI gate: CI 状态为 ${run_status}，等待成功：${run_url}"
        ;;
      *)
        echo "CI gate: 未识别的 CI 状态 ${run_status}，禁止部署：${run_url}" >&2
        exit 2
        ;;
    esac
  fi

  if (( $(date +%s) >= deadline )); then
    echo "CI gate: 等待 CI 超过 ${timeout_seconds}s，禁止部署" >&2
    exit 2
  fi

  sleep "$poll_seconds"
done
