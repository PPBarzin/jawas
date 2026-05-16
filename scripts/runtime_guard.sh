#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
STATE_DIR="${REPO_ROOT}/runtime_guard"
SAMPLES_FILE="${STATE_DIR}/balance_samples.tsv"
SUMMARY_FILE="${STATE_DIR}/hourly_guard.tsv"
EVENTS_FILE="${STATE_DIR}/guard_events.log"
mkdir -p "${STATE_DIR}"

ENV_FILE="${ENV_FILE:-${REPO_ROOT}/.env}"
if [[ -f "${ENV_FILE}" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
fi

CONTAINER_NAME="${JAWAS_GUARD_CONTAINER:-jawas-kamino}"
WINDOW_SECS="${JAWAS_GUARD_WINDOW_SECS:-3600}"
MAX_SOL_SPEND_LAMPORTS="${JAWAS_GUARD_MAX_SOL_SPEND_LAMPORTS:-20000000}"
MAX_BUNDLES_PER_WINDOW="${JAWAS_GUARD_MAX_BUNDLES_PER_WINDOW:-40}"
MIN_SOL_FLOOR_LAMPORTS="${JAWAS_GUARD_MIN_SOL_FLOOR_LAMPORTS:-250000000}"

NOW_EPOCH="$(date +%s)"
NOW_ISO="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
WINDOW_START_EPOCH="$((NOW_EPOCH - WINDOW_SECS))"

KEYPAIR_PATH="${SOLANA_KEYPAIR_PATH:-secrets/keypair.json}"
if [[ "${KEYPAIR_PATH}" != /* ]]; then
  KEYPAIR_PATH="${REPO_ROOT}/${KEYPAIR_PATH}"
fi

RPC_URL="${HUNTER_RPC_URL:-${OBSERVER_RPC_URL:-${RPC_URL:-}}}"
if [[ -z "${RPC_URL}" ]]; then
  echo "${NOW_ISO} status=error reason=missing_rpc_url" >> "${EVENTS_FILE}"
  exit 1
fi

if [[ ! -f "${KEYPAIR_PATH}" ]]; then
  echo "${NOW_ISO} status=error reason=missing_keypair path=${KEYPAIR_PATH}" >> "${EVENTS_FILE}"
  exit 1
fi

wallet_pubkey="$(
  solana-keygen pubkey "${KEYPAIR_PATH}" 2>/dev/null
)"

balance_lamports="$(
  solana balance "${wallet_pubkey}" --url "${RPC_URL}" --lamports 2>/dev/null | awk 'NR==1 {print $1}'
)"

if [[ -z "${balance_lamports}" ]]; then
  echo "${NOW_ISO} status=error reason=balance_unavailable wallet=${wallet_pubkey}" >> "${EVENTS_FILE}"
  exit 1
fi

container_status="$(
  docker inspect -f '{{.State.Status}}' "${CONTAINER_NAME}" 2>/dev/null || true
)"
if [[ -z "${container_status}" ]]; then
  container_status="missing"
fi

printf '%s\t%s\n' "${NOW_EPOCH}" "${balance_lamports}" >> "${SAMPLES_FILE}"
awk -F '\t' -v min_epoch="${WINDOW_START_EPOCH}" '$1 >= min_epoch { print }' \
  "${SAMPLES_FILE}" > "${SAMPLES_FILE}.tmp"
mv "${SAMPLES_FILE}.tmp" "${SAMPLES_FILE}"

baseline_lamports="$(
  awk -F '\t' 'NR==1 {print $2}' "${SAMPLES_FILE}"
)"
if [[ -z "${baseline_lamports}" ]]; then
  baseline_lamports="${balance_lamports}"
fi

delta_lamports="$((balance_lamports - baseline_lamports))"
spent_lamports=0
if (( delta_lamports < 0 )); then
  spent_lamports=$(( -delta_lamports ))
fi

logs=""
if [[ "${container_status}" == "running" ]]; then
  logs="$(docker logs --since "${WINDOW_SECS}s" "${CONTAINER_NAME}" 2>&1 || true)"
fi

count_matches() {
  local pattern="$1"
  if [[ -z "${logs}" ]]; then
    echo 0
  else
    grep -c -- "${pattern}" <<<"${logs}" || true
  fi
}

bundle_sent_count="$(count_matches "BUNDLE SENT")"
firing_count="$(count_matches "FIRING |")"
loop_exited_count="$(count_matches "loop exited")"
hermes_match_count="$(count_matches "hermes stream matched")"

status="ok"
reasons=()

if (( balance_lamports < MIN_SOL_FLOOR_LAMPORTS )); then
  status="trip"
  reasons+=("sol_floor")
fi

if (( spent_lamports >= MAX_SOL_SPEND_LAMPORTS )); then
  status="trip"
  reasons+=("sol_spend")
fi

if (( MAX_BUNDLES_PER_WINDOW > 0 && bundle_sent_count >= MAX_BUNDLES_PER_WINDOW )); then
  status="trip"
  reasons+=("bundle_rate")
fi

if (( loop_exited_count > 0 )); then
  status="trip"
  reasons+=("loop_exited")
fi

reason_csv="none"
if (( ${#reasons[@]} > 0 )); then
  reason_csv="$(IFS=,; echo "${reasons[*]}")"
fi

delta_sol="$(awk "BEGIN { printf \"%.9f\", ${delta_lamports} / 1000000000 }")"
spent_sol="$(awk "BEGIN { printf \"%.9f\", ${spent_lamports} / 1000000000 }")"
balance_sol="$(awk "BEGIN { printf \"%.9f\", ${balance_lamports} / 1000000000 }")"

printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
  "${NOW_ISO}" \
  "${status}" \
  "${container_status}" \
  "${wallet_pubkey}" \
  "${balance_lamports}" \
  "${delta_lamports}" \
  "${bundle_sent_count}" \
  "${firing_count}" \
  "${hermes_match_count}" \
  "${loop_exited_count}" \
  "${reason_csv}" >> "${SUMMARY_FILE}"

if [[ "${status}" != "trip" ]]; then
  exit 0
fi

if [[ "${container_status}" == "running" ]]; then
  docker stop "${CONTAINER_NAME}" >/dev/null
  container_status="stopped"
fi

cat >> "${EVENTS_FILE}" <<EOF
${NOW_ISO} status=trip container=${CONTAINER_NAME} wallet=${wallet_pubkey} balance_sol=${balance_sol} delta_sol=${delta_sol} spent_sol=${spent_sol} bundles=${bundle_sent_count} firings=${firing_count} hermes_matches=${hermes_match_count} reasons=${reason_csv} action=container_stopped
EOF
