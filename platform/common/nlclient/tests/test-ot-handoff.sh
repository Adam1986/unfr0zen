#!/bin/sh
# Offline only: no firmware patch or installation.
set -eu
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
${CC:-cc} -std=c11 -Wall -Wextra -Werror -I"$dir/../src" "$dir/ot_supply_handoff_test.c" -o "$tmp/guard"
"$tmp/guard"
for mode in 0 1 2 3; do
    UNFR0ZEN_OT_SUPPLY_EXPERIMENT=$mode sh "$dir/../build.sh" "$tmp/$mode"
    readelf -Ws "$tmp/$mode/custom-screen.o" > "$tmp/$mode/symbols"
    # The in-memory linker accepts only ABS32, REL32, CALL and JUMP24
    # in the retained .text and .rodata sections; it discards EHABI.
    readelf -Wr "$tmp/$mode/custom-screen.o" > "$tmp/$mode/relocs"
    awk '
        /^Relocation section/ {kept=($3 ~ /^\047\.rel\.(text|rodata)/)}
        kept && $3 ~ /^R_ARM_/ && $3 !~ /^R_ARM_(ABS32|REL32|CALL|JUMP24)$/ {
            print "Unsupported firmware relocation: " $0; bad=1
        }
        END {exit bad}
    ' "$tmp/$mode/relocs"

done
if grep -q native_ot_ "$tmp/0/symbols"; then echo "normal build references experimental API" >&2; exit 1; fi
grep -q native_ot_heat_get "$tmp/1/symbols"
grep -q native_ot_supply_get "$tmp/1/symbols"
if grep -q native_ot_supply_set "$tmp/1/symbols"; then echo "dry-run references setter" >&2; exit 1; fi
grep -q ot_dry_event "$tmp/1/symbols"
grep -q native_ot_supply_set "$tmp/2/symbols"
grep -q ot_write_event "$tmp/2/symbols"
if grep -q ot_dry_event "$tmp/2/symbols"; then echo "write object has wrong callback" >&2; exit 1; fi
if grep -q ot_write_event "$tmp/1/symbols"; then echo "dry-run has write callback" >&2; exit 1; fi
grep -q native_ot_heat_get "$tmp/3/symbols"
grep -q native_ot_supply_get "$tmp/3/symbols"
grep -q ot_auto_event "$tmp/3/symbols"
if grep -Eq 'native_ot_supply_set|ot_write_event' "$tmp/3/symbols"; then echo "auto-start object can write" >&2; exit 1; fi
echo 'ARM objects: normal, dry-run, isolated write and auto-start modes compiled; symbol gates passed'
