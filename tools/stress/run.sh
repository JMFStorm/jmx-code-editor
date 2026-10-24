#!/usr/bin/env bash
# Run the stress scenarios headless against sandbox/stress and record the measurements.
#
#   tools/stress/run.sh LABEL [SCENARIO-FILTER]
#
# Needs: tools/make-stress-sandbox.py run once. Results: target/stress/LABEL.jsonl; a table
# compares them with target/stress/baseline.jsonl when that exists.
set -euo pipefail
export LC_NUMERIC=C
cd "$(dirname "$0")/../.."
LABEL=${1:?usage: run.sh LABEL [FILTER]}
FILTER=${2:-}
WS=sandbox/stress/stress.jdcws
# A fresh, identical sandbox every run (scenarios change files; the generator is deterministic).
python3 tools/make-stress-sandbox.py >/dev/null
# The workspace file keeps the session on quit: start every scenario from the original.
cp "$WS" "$WS.pristine"
# C# builds are an external tool's time: make them fail at once (as the dotnet SDK would on
# an unrestored project) while the SDK's API docs still load.
export JDC_DOTNET=/bin/false
DOTNET_BIN=$(command -v dotnet || true)
[ -n "$DOTNET_BIN" ] && export DOTNET_ROOT=$(dirname "$(readlink -f "$DOTNET_BIN")")
cargo build --release --quiet
mkdir -p target/stress/logs
OUT=target/stress/$LABEL.jsonl
: > "$OUT"
for s in tools/stress/s*.jdcs; do
    name=$(basename "$s" .jdcs)
    [[ -n "$FILTER" && "$name" != *"$FILTER"* ]] && continue
    cp "$WS.pristine" "$WS"
    echo "==> $name"
    start=$(date +%s.%N)
    timeout 900 target/release/jdc --headless --size=1500x900 \
        --log-dir target/stress/logs --script "$s" "$WS" 2>/dev/null |
        grep -E '^\{"(perf|timing)"' | sed "s/^{/{\"scenario\":\"$name\",/" >> "$OUT" || true
    end=$(date +%s.%N)
    printf '    %.1f s\n' "$(echo "$end - $start" | bc)"
done
# Undo `touch` changes and restore the workspace file.
cp "$WS.pristine" "$WS"
python3 tools/stress/report.py "$OUT" target/stress/baseline.jsonl
