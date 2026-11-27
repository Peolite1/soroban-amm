#!/usr/bin/env bash
# common.test.sh — offline tests for the assertion and ledger-clock helpers in
# common.sh. No network: reads and the stellar CLI are stubbed.
# Run: bash scripts/e2e/common.test.sh
set -Eeuo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

E2E_READ_WAIT_SECS=0
COUNTER="$(mktemp)"
INVOKES="$(mktemp)"
trap 'rm -f "$COUNTER" "$INVOKES"' EXIT
FAILED=0

# calls_until_ok N OK_VALUE STALE_VALUE — prints STALE_VALUE for the first N-1
# calls and OK_VALUE afterwards. State lives in a file because a deferred read
# runs in a subshell.
calls_until_ok() {
  local n=$(( $(cat "$COUNTER") + 1 ))
  echo "$n" > "$COUNTER"
  if (( n < $1 )); then printf '%s\n' "$3"; else printf '%s\n' "$2"; fi
}

# a "transaction": reports submission on stderr like the stellar CLI
send_tx() {
  echo "$(( $(cat "$COUNTER") + 1 ))" > "$COUNTER"
  echo "Transaction submitted successfully!" >&2
  printf '0\n'
}

expect_ok() {
  local name="$1"; shift
  echo 0 > "$COUNTER"
  if ( "$@" ) >/dev/null 2>&1; then echo "ok   - $name"; else echo "FAIL - $name"; FAILED=1; fi
}

expect_die() {
  local name="$1"; shift
  echo 0 > "$COUNTER"
  if ( "$@" ) >/dev/null 2>&1; then echo "FAIL - $name (should have failed)"; FAILED=1; else echo "ok   - $name"; fi
}

expect_calls() {
  local name="$1" want="$2"
  if [[ "$(cat "$COUNTER")" == "$want" ]]; then echo "ok   - $name ($want call(s))"; else echo "FAIL - $name: $(cat "$COUNTER") call(s), wanted $want"; FAILED=1; fi
}

# literals behave exactly as before
expect_ok  "literal eq passes"           assert_eq "t" "5" "5"
expect_die "literal eq mismatch dies"    assert_eq "t" "5" "6"
expect_ok  "literal gt passes"           assert_gt "t" "5" 4
expect_die "literal gt dies"             assert_gt "t" "5" 5
expect_die "non-numeric gt dies"         assert_gt "t" "abc" 1
expect_ok  "literal between passes"      assert_between "t" "5" 1 10
expect_ok  "literal contains passes"     assert_contains "t" "hello world" "wor"
expect_ok  "literal not_contains passes" assert_not_contains "t" "hello" "xyz"
expect_ok  "lte_abs passes"              assert_lte_abs "t" "-3" 3

# a stale read settles on a later attempt
expect_ok  "eq settles"        assert_eq "t" '@cmd:calls_until_ok 3 7 0' "7"
expect_ok  "gt settles"        assert_gt "t" '@cmd:calls_until_ok 3 7 0' 5
expect_ok  "between settles"   assert_between "t" '@cmd:calls_until_ok 3 7 0' 5 9
expect_ok  "contains settles"  assert_contains "t" '@cmd:calls_until_ok 3 revoked pending' "revoked"
expect_ok  "not_contains settles" assert_not_contains "t" '@cmd:calls_until_ok 3 gone present' "present"
expect_ok  "lte_abs settles"   assert_lte_abs "t" '@cmd:calls_until_ok 3 1 500' 5
expect_ok  "settled wrapper"   assert_eq_settled "t" "7" calls_until_ok 3 7 0
expect_ok  "contains wrapper"  assert_contains_settled "t" "ok" calls_until_ok 3 ok no

# a fresh read is not repeated
echo 0 > "$COUNTER"; assert_eq "t" '@cmd:calls_until_ok 1 7 0' "7" >/dev/null
expect_calls "fresh read costs one call" 1

# a read that never settles dies after exactly E2E_READ_ATTEMPTS reads
expect_die "stale forever dies" assert_eq "t" '@cmd:calls_until_ok 999 7 0' "7"
echo 0 > "$COUNTER"; ( assert_eq "t" '@cmd:calls_until_ok 999 7 0' "7" ) >/dev/null 2>&1 || true
expect_calls "gives up after the attempt limit" "$E2E_READ_ATTEMPTS"

# a deferred transaction is never repeated
echo 0 > "$COUNTER"; ( assert_eq "t" '@cmd:send_tx' "1" ) >/dev/null 2>&1 || true
expect_calls "a write is not re-run on mismatch" 1

# the deferred code sees the caller's local variables
local_scope() { local who="alice"; assert_eq "t" '@cmd:printf %s "$who"' "alice"; }
expect_ok "deferred code sees caller locals" local_scope

# a failing deferred command counts as an empty read, not a crash
expect_die "failing command dies via the assertion" assert_eq "t" '@cmd:false' "x"

# ── ledger clock (issue #1488) ──────────────────────────────────────────────
# e2e_wait_for_ledger_time must wait on the network's ledger close time. The
# twal_consumer flow used to poll the AMM pool's get_liquidity_cumulative()
# timestamp instead, which is the time of the pool's last write: nothing wrote
# to the pool while the flow waited, so it sat at the deposit's ledger and
# every testnet run timed out with "pool clock stuck".

# stellar — offline stand-in for the CLI. `ledger latest` reports sequence
# STUB_SEQ; each `ledger fetch` (counted in $COUNTER) reports a close time
# STUB_STEP seconds later than the previous one, starting at STUB_CLOSE.
# STUB_FETCH_FAIL=1 fails every fetch; STUB_LAG=1 fails the fetch of STUB_SEQ
# only, like a load-balanced node a ledger behind. `contract invoke` is logged
# to $INVOKES and returns a pool clock that never moves.
STUB_SEQ=100
STUB_CLOSE=1795524937
STUB_STEP=5
STUB_FETCH_FAIL=0
STUB_LAG=0
stellar() {
  case "$1 $2" in
    "ledger latest")
      printf '{"id":"ab12","protocolVersion":23,"sequence":%s}\n' "$STUB_SEQ"
      ;;
    "ledger fetch")
      local seq="$3" n close
      if [[ "$STUB_FETCH_FAIL" == 1 ]]; then echo "error: RPC unavailable" >&2; return 1; fi
      if [[ "$STUB_LAG" == 1 && "$seq" == "$STUB_SEQ" ]]; then
        echo "error: start ledger must be between the oldest and latest ledger" >&2
        return 1
      fi
      n=$(cat "$COUNTER"); echo $(( n + 1 )) > "$COUNTER"
      close=$(( STUB_CLOSE + n * STUB_STEP ))
      # latestLedgerCloseTime is deliberately different: the helper must
      # report the close time of the ledger it asked for.
      printf '{"ledgers":[{"hash":"cd34","sequence":%s,"ledgerCloseTime":"%s","headerXdr":"AAAA","metadataXdr":"AAAA"}],"latestLedger":%s,"latestLedgerCloseTime":"%s","oldestLedger":1,"oldestLedgerCloseTime":"1","cursor":"%s"}\n' \
        "$seq" "$close" "$STUB_SEQ" "$(( close + 999 ))" "$seq"
      ;;
    "contract invoke")
      echo invoke >> "$INVOKES"
      printf '["0","%s"]\n' "$STUB_CLOSE"
      ;;
    *)
      echo "stellar stub: unexpected call: $*" >&2
      return 1
      ;;
  esac
}

expect_out() {
  local name="$1" want="$2" got
  shift 2
  echo 0 > "$COUNTER"
  got=$( "$@" 2>/dev/null ) || got="<failed>"
  if [[ "$got" == "$want" ]]; then echo "ok   - $name"; else echo "FAIL - $name: got '$got', wanted '$want'"; FAILED=1; fi
}

# expect_die_with NAME PATTERN CMD... — CMD must fail and say PATTERN on stderr.
expect_die_with() {
  local name="$1" pattern="$2" err
  shift 2
  echo 0 > "$COUNTER"
  if err=$( ( "$@" ) 2>&1 >/dev/null ); then
    echo "FAIL - $name (should have failed)"; FAILED=1
  elif [[ "$err" == *"$pattern"* ]]; then
    echo "ok   - $name"
  else
    echo "FAIL - $name: stderr lacks '$pattern': $err"; FAILED=1
  fi
}

expect_out "ledger time is the fetched ledger's close time" "$STUB_CLOSE" e2e_ledger_time
STUB_LAG=1
expect_out "ledger time falls back a ledger when the node lags" "$STUB_CLOSE" e2e_ledger_time
STUB_LAG=0
STUB_FETCH_FAIL=1
expect_die "ledger time fails when RPC cannot be read" e2e_ledger_time
STUB_FETCH_FAIL=0

E2E_LEDGER_POLL_SECS=0
E2E_LEDGER_WAIT_SECS=30
: > "$INVOKES"
expect_ok "wait returns once the ledger clock reaches the target" \
  e2e_wait_for_ledger_time $(( STUB_CLOSE + 15 ))
expect_calls "wait polls until the target ledger closes" 4
if [[ -s "$INVOKES" ]]; then
  echo "FAIL - wait must not read a contract's stored timestamp ($(wc -l < "$INVOKES") invoke(s))"; FAILED=1
else
  echo "ok   - wait reads no contract's stored timestamp"
fi
expect_ok "wait returns at once when the target has passed" \
  e2e_wait_for_ledger_time $(( STUB_CLOSE - 60 ))
expect_calls "a passed target costs one read" 1

E2E_LEDGER_WAIT_SECS=0
STUB_STEP=0
expect_die_with "wait dies when the ledger clock never reaches the target" \
  "ledger clock at $STUB_CLOSE after 0s, waited for $(( STUB_CLOSE + 15 ))" \
  e2e_wait_for_ledger_time $(( STUB_CLOSE + 15 ))
STUB_FETCH_FAIL=1
expect_die_with "wait dies naming RPC when the ledger time cannot be read" \
  "could not read the latest ledger close time" \
  e2e_wait_for_ledger_time $(( STUB_CLOSE + 15 ))
STUB_FETCH_FAIL=0
STUB_STEP=5

exit "$FAILED"
