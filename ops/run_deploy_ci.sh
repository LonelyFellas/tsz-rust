#!/usr/bin/env bash
set -euo pipefail

root="$1"
shift
mkdir -p "$root"
for retry in 1 2; do
  directory="$root/attempt-$retry"
  mkdir -p "$directory"
  set +e
  python3 -u ops/deploy_ci.py deploy \
    --sha "$DEPLOY_SHA" --run-id "$DEPLOY_CI_RUN" --attempt "$DEPLOY_CI_ATTEMPT" \
    --directory "$directory" "$@" \
    2>&1 | tee "$root/controller-$retry.log"
  status=${PIPESTATUS[0]}
  set -e
  if [[ "$status" -eq 0 ]]; then
    exit 0
  fi
  recovery_error=""
  failed_stage=""
  if [[ -f "$directory/result.json" ]]; then
    recovery_error="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("recovery_error", ""))' "$directory/result.json")"
    failed_stage="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("failed_stage", ""))' "$directory/result.json")"
  fi
  if [[ "$recovery_error" != "remote_operation_busy" && "$failed_stage" != "artifact_upload_failed" && "$failed_stage" != "artifact_pull_failed" ]]; then
    exit "$status"
  fi
  echo "backend deployment retry $retry after $failed_stage/$recovery_error" >&2
  if [[ "$retry" -eq 1 ]]; then
    sleep 30
  fi
done
exit 1
