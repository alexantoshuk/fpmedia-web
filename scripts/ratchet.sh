#!/usr/bin/env bash
# Ownership / type-debt ratchet (`architecture.md` § Ownership & types).
# Counters may only go DOWN vs scripts/ratchet-baseline.txt. Lower the baseline
# in the same commit that reduces a count. Uses grep only (no rg in CI).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BASELINE="scripts/ratchet-baseline.txt"

# count <extended-regex> <path>...  → number of matches (not lines)
count() {
  local pat="$1"
  shift
  { grep -rEo --include='*.rs' -- "$pat" "$@" 2>/dev/null || true; } | wc -l | tr -d ' '
}

declare -A NOW
NOW[rc_dyn_fn]="$(count 'Rc<dyn Fn' components/src)"
NOW[arc]="$(count 'Arc<' engine/src components/src)"
NOW[mutex]="$(count 'Mutex<' engine/src components/src)"
NOW[shared_cells]="$(count '\bCell<' engine/src/video_player/shared.rs)"
NOW[frame_casts]="$(count 'u64::from\(| as u32' components/src)"

if [[ "${1:-}" == "--write" ]]; then
  : >"$BASELINE"
  for k in rc_dyn_fn arc mutex shared_cells frame_casts; do
    echo "$k=${NOW[$k]}" >>"$BASELINE"
  done
  echo "ratchet: baseline written"
  cat "$BASELINE"
  exit 0
fi

fail=0
for k in rc_dyn_fn arc mutex shared_cells frame_casts; do
  base="$(grep -E "^$k=" "$BASELINE" | cut -d= -f2 | tr -d '\r' || true)"
  if [[ -z "$base" ]]; then
    echo "ratchet: missing baseline for $k (run: bash scripts/ratchet.sh --write)"
    fail=1
    continue
  fi
  now="${NOW[$k]}"
  if ((now > base)); then
    echo "ratchet: FAIL $k grew ($base -> $now) — see architecture.md § Ownership & types"
    fail=1
  elif ((now < base)); then
    echo "ratchet: lower baseline: $k=$now (was $base)"
  fi
done

if ((fail)); then
  exit 1
fi
echo "ratchet: ok"
