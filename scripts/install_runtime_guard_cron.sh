#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
STATE_DIR="${REPO_ROOT}/runtime_guard"
mkdir -p "${STATE_DIR}"

TAG="# jawas-runtime-guard"
CRON_LINE="*/15 * * * * cd ${REPO_ROOT} && ${REPO_ROOT}/scripts/runtime_guard.sh >> ${STATE_DIR}/cron.log 2>&1 ${TAG}"

current_crontab="$(crontab -l 2>/dev/null || true)"
filtered_crontab="$(printf '%s\n' "${current_crontab}" | awk -v tag="${TAG}" 'index($0, tag) == 0')"

{
  printf '%s\n' "${filtered_crontab}"
  printf '%s\n' "${CRON_LINE}"
} | crontab -

printf 'Installed runtime guard cron entry:\n%s\n' "${CRON_LINE}"
