#!/bin/sh
# Offline only: no firmware patch or installation.
set -eu
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
${CC:-cc} -std=c11 -Wall -Wextra -Werror -I"$dir/../src" "$dir/ot_supply_handoff_test.c" -o "$tmp/guard"
"$tmp/guard"
for mode in 0 1 2; do
    UNFR0ZEN_OT_SUPPLY_EXPERIMENT=$mode sh "$dir/../build.sh" "$tmp/$mode"
    readelf -Ws "$tmp/$mode/custom-screen.o" > "$tmp/$mode/symbols"
done
if grep -q native_ot_ "$tmp/0/symbols"; then echo "normal build references experimental API" >&2; exit 1; fi
grep -q native_ot_heat_get "$tmp/1/symbols"
grep -q native_ot_supply_get "$tmp/1/symbols"
if grep -q native_ot_supply_set "$tmp/1/symbols"; then echo "dry-run references setter" >&2; exit 1; fi
grep -q ot_dry_event "$tmp/1/symbols"
grep -q native_ot_supply_set "$tmp/2/symbols"
echo 'ARM objects: normal, dry-run and isolated write mode compiled; symbol gates passed'
