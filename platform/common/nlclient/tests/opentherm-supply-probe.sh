#!/usr/bin/env bash
# Isolated ARM execution only: no thermostat, firmware installation or boiler.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
cat > native.S <<'ASM'
.syntax unified
.arm
.text
.global native_get_heatlink_heat,native_get_supply,_start
/* Instructions verified against the recovered stock and installed binary.
 * The getters are leaf functions: use their actual instructions, not mocks. */
native_get_heatlink_heat:
    add r0,r0,#4096
    ldr r0,[r0,#1600]
    bx lr
native_get_supply:
    add r0,r0,#4096
    ldr r0,[r0,#1620]
    bx lr
_start:
    bl test_main
    mov r7,#1
    svc #0
.section .note.GNU-stack,"",%progbits
ASM
cat > mock.c <<'C'
typedef unsigned int u32;
static u32 globals[1500], bucket[8], record[4], staging[4];
static int commits, opens, writes, closes, renames, failure;
static void *expected_bucket;
#define CHECK(x) do { if (!(x)) fail(__LINE__); } while (0)
static __attribute__((noreturn)) void fail(int line) {
    register int r0 __asm__("r0") = line;
    register int r7 __asm__("r7") = 1;
    __asm__ volatile("svc #0" : : "r"(r0), "r"(r7) : "memory");
    __builtin_unreachable();
}
static int same(const char *a,const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return *a == *b;
}
int native_getpid(void) { return 123; }
void *native_fopen(const char *p,const char *m) {
    CHECK(same(p,"/var/run/unfr0zen/ot-supply-probe.tmp") && same(m,"wb"));
    opens++;
    return failure == 1 ? (void *)0 : staging;
}
u32 native_fwrite(const u32 *p,u32 sz,u32 n,void *f) {
    CHECK(sz==1 && n==16 && f==staging);
    writes++;
    for(int i=0;i<4;i++) staging[i]=p[i];
    return failure==2 ? 8 : 16;
}
int native_fclose(void *f) {
    CHECK(f==staging); closes++; return failure==3 ? -1 : 0;
}
int native_rename(const char *a,const char *b) {
    CHECK(same(a,"/var/run/unfr0zen/ot-supply-probe.tmp"));
    CHECK(same(b,"/var/run/unfr0zen/ot-supply-probe.state"));
    renames++;
    if(failure==4) return -1;
    for(int i=0;i<4;i++) record[i]=staging[i];
    return 0;
}
int existing_device_commit(void *b) {
    CHECK(b==expected_bucket); commits++; return 47;
}
extern int supply_probe_commit(void *);
static void observe(void *b) {
    expected_bucket=b;
    int prev=commits;
    CHECK(supply_probe_commit(b)==47 && commits==prev+1);
}
int test_main(void) {
    observe(0); observe(bucket); CHECK(opens==0);
    bucket[7]=(u32)globals;
    globals[5696/4]=1;
    globals[5716/4]=60u<<16;
    observe(bucket);
    CHECK(record[0]==0x3153544f && record[1]==123 && record[2]==1 && record[3]==60u<<16);
    CHECK(opens==1 && writes==1 && closes==1 && renames==1);
    globals[5716/4]=(55u<<16)+32768u;
    observe(bucket);
    CHECK(record[3]==(55u<<16)+32768u);
    CHECK(globals[5716/4]==(55u<<16)+32768u);
    globals[5696/4]=0; observe(bucket);
    CHECK(record[2]==0 && record[3]==0xffffffffu);
    globals[5696/4]=1; globals[5716/4]=101u<<16; observe(bucket);
    CHECK(record[2]==1 && record[3]==0xffffffffu);
    globals[5716/4]=0xffffffffu; observe(bucket);
    CHECK(record[3]==0xffffffffu);
    globals[5716/4]=100u<<16; observe(bucket);
    CHECK(record[3]==100u<<16);
    globals[5716/4]=0; observe(bucket); CHECK(record[3]==0);
    globals[5716/4]=65u<<16;
    for(failure=1;failure<=4;failure++) {
        int prev=renames;
        observe(bucket);
        CHECK(renames==prev+(failure==4));
        CHECK(record[3]==0 && globals[5716/4]==65u<<16);
    }
    return 0;
}
C
clang --target=armv7-linux-gnueabihf -marm -c "$here/opentherm-supply-probe.S" -o probe.o
clang --target=armv7-linux-gnueabihf -marm -c native.S -o native.o
clang --target=armv7-linux-gnueabihf -marm -ffreestanding -fno-builtin -fno-stack-protector -fno-pic -fno-pie -O1 -c mock.c -o mock.o
ld.lld -m armelf_linux_eabi -static -e _start probe.o native.o mock.o -o mock-arm
if qemu-arm -cpu cortex-a8 ./mock-arm; then
    echo 'PASS: native leaf getters, read-only observation, failed IO, original commit result'
else
    status=$?
    echo "FAIL: ARM probe exit $status" >&2
    exit "$status"
fi
