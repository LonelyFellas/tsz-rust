#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${GH_TOKEN:-}" || ! "${DEPLOY_REPO:-}" =~ ^[a-zA-Z0-9-]+/[a-zA-Z0-9_.-]+$ ]]; then
  echo 'Private deployment repository or dispatch token is missing or invalid' >&2
  exit 1
fi
if [[ ! "${DEPLOY_SHA:-}" =~ ^[0-9a-f]{40}$ || ! "${DEPLOY_CI_RUN:-}" =~ ^[1-9][0-9]*$ || ! "${DEPLOY_CI_ATTEMPT:-}" =~ ^[1-9][0-9]*$ ]]; then
  echo 'Invalid deployment candidate identity' >&2
  exit 1
fi
if [[ "$(gh api "repos/$DEPLOY_REPO" --jq '.private == true and .default_branch == "main"')" != true ]]; then
  echo 'The deployment repository must be private and use main as its default branch' >&2
  exit 1
fi

run_id=$(gh api --method POST "repos/$DEPLOY_REPO/actions/workflows/deploy-tshb-test-vpc.yml/dispatches" \
  -H "X-GitHub-Api-Version: 2026-03-10" --jq '.workflow_run_id | numbers | select(. > 0)' \
  -f ref=main \
  -f "inputs[sha]=$DEPLOY_SHA" \
  -f "inputs[run_id]=$DEPLOY_CI_RUN" \
  -f "inputs[attempt]=$DEPLOY_CI_ATTEMPT")
if [[ ! "$run_id" =~ ^[1-9][0-9]*$ ]]; then
  echo 'GitHub did not return the private workflow run ID. Check the private repository before re-dispatching.' >&2
  exit 1
fi
echo "Private deployment workflow dispatched as run $run_id. Its run records deployment results."
