#!/bin/sh
# Static ABI/file-handoff contract test for the supervised HA confirmation retest.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
HEADER=$ROOT/platform/common/nlclient/src/ot_supply_handoff.h
NATIVE=$ROOT/platform/common/nlclient/src/custom_screen.c
RUST=$ROOT/crates/nestlocal/src/opentherm_supply.rs
need() {
    grep -Fq -- "$2" "$1" || {
        echo "FAIL: expected contract not found in $1: $2" >&2
        exit 1
    }
}
need "$NATIVE" '#define OT_COMMAND RUN "ot-managed."'
need "$NATIVE" 'rename_setting(OT_COMMAND "request", OT_COMMAND "taken")'
need "$NATIVE" 'rename_setting(OT_COMMAND "arm", OT_COMMAND "arm.taken")'
need "$NATIVE" 'rename_setting(OT_COMMAND "ack.tmp", OT_COMMAND "ack")'
need "$HEADER" '#define OT_MANAGED_ARM_MAGIC 0x314d544fu'
need "$HEADER" '#define OT_MANAGED_REQ_MAGIC 0x3143544fu'
need "$HEADER" '#define OT_ACK_MAGIC 0x314b544fu'
need "$HEADER" '#define OT_FIXED 65536u'
need "$RUST" 'const ARM_MAGIC: u32 = 0x314d544f'
need "$RUST" 'const REQ_MAGIC: u32 = 0x3143544f'
need "$RUST" 'const ACK_MAGIC: u32 = 0x314b544f'
need "$RUST" 'const FIXED: u32 = 65536'
need "$RUST" '2 if before == self.before * FIXED && after == self.target * FIXED => Outcome::Confirmed'
echo 'PASS: managed ARM/request/ACK ABI and native claim filenames match nestlocal.'
