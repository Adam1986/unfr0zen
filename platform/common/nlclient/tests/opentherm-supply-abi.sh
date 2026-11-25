#!/usr/bin/env bash
# Test the reviewed ARM candidate in a freestanding process with MOCK native
# functions. No firmware image is read, patched, executed or produced.
set -euo pipefail
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
cat > candidate.S <<'ASM'
.syntax unified
.arm
.text
.align 2
.global register_supply_candidate
.type register_supply_candidate,%function
register_supply_candidate:
    push {r4,r5,r6,lr}
    sub sp,sp,#16
    mov r4,r0
    ldr r12,[sp,#32]
    str r12,[sp]
    bl native_register_event
    mov r6,r0
    mov r0,#168
    bl native_allocate
    cmp r0,#0
    beq registration_done
    mov r5,r0
    ldr r12,supply_delta0
    add r12,pc,r12
supply_pc0 = . - 4
    str r12,[sp]
    str r4,[sp,#4]
    mov r12,#1
    str r12,[sp,#8]
    movw r1,#366
    mov r2,#2
    mov r3,#1
    bl native_property_construct
    ldr r0,[r4,#0xe0]
    ldr r1,supply_delta1
    add r1,pc,r1
supply_pc1 = . - 4
    mov r2,r5
    bl native_property_insert
    mov r0,r4
    ldr r1,supply_delta2
    add r1,pc,r1
supply_pc2 = . - 4
    mov r2,#77
    mov r3,#56
    mov r12,#0
    str r12,[sp]
    bl native_register_event
registration_done:
    mov r0,r6
    add sp,sp,#16
    pop {r4,r5,r6,pc}
.size register_supply_candidate,.-register_supply_candidate
.align 2
.global supply_delta0,supply_delta1,supply_delta2
supply_delta0: .word native_supply_name - (supply_pc0 + 8)
supply_delta1: .word native_supply_name - (supply_pc1 + 8)
supply_delta2: .word native_supply_name - (supply_pc2 + 8)
.align 2
.global guard_supply_candidate
.type guard_supply_candidate,%function
guard_supply_candidate:
    cmp r0,#0
    beq rejected
    cmp r2,#0
    ble rejected
    cmp r3,#6553600
    bhi rejected
    cmp r2,r3
    bhi rejected
    cmp r1,r2
    blo rejected
    cmp r1,r3
    bhi rejected
    uxth r12,r1
    cmp r12,#0
    bne rejected
    uxth r12,r2
    cmp r12,#0
    bne rejected
    uxth r12,r3
    cmp r12,#0
    bne rejected
    push {r4,r5,r6,lr}
    mov r4,r0
    mov r5,r1
    bl native_get_heatlink_heat
    cmp r0,#1
    bne guard_rejected
    mov r0,r4
    mov r1,r5
    bl native_supply_setter
    mov r0,#1
    pop {r4,r5,r6,pc}
guard_rejected:
    mov r0,#0
    pop {r4,r5,r6,pc}
rejected:
    mov r0,#0
    bx lr
.size guard_supply_candidate,.-guard_supply_candidate
.global event77_candidate
.type event77_candidate,%function
event77_candidate:
    ldr r0,[r5,#0x1c]
    ldr r1,[r4,#0x30]
    mov r2,#0
    mov r3,#0
    bl guard_supply_candidate
    b native_dispatch_return
.size event77_candidate,.-event77_candidate

/* Test-only dispatcher continuation and Linux process entry. */
.global call_adapter
.type call_adapter,%function
call_adapter:
    push {r4,r5,r8,lr}
    mov r5,r0
    mov r4,r1
    adr r8,adapter_return
    b event77_candidate
adapter_return:
    pop {r4,r5,r8,pc}
.global native_dispatch_return
native_dispatch_return:
    bx r8
.global _start
_start:
    bl test_main
    mov r7,#1
    svc #0
.section .note.GNU-stack,"",%progbits
ASM
cat > mock.c <<'C'
typedef unsigned int u32;
static int step, failed_alloc, getters, setters, heat_type;
static u32 setting;
static u32 bucket[64], property[42], globals[1500], event[13], dispatcher[8];
const char native_supply_name[] = "boiler_supply_temperature";
static const char previous_name[] = "existing";
#define CHECK(expr) do { if (!(expr)) fail(__LINE__); } while (0)
static __attribute__((noreturn)) void fail(int line) {
    register int r0 __asm__("r0") = line;
    register int r7 __asm__("r7") = 1;
    __asm__ volatile("svc #0" : : "r"(r0), "r"(r7) : "memory");
    __builtin_unreachable();
}
u32 native_register_event(void *b, const char *name, int id, int desc, int flag) {
    CHECK(b == bucket);
    if (step == 0) {
        CHECK(name == previous_name && id == 106 && desc == 91 && flag == 7);
        step++; return 0x1234;
    }
    CHECK(step == 4 && name == native_supply_name && id == 77 && desc == 56 && flag == 0);
    step++; return 0x5678;
}
void *native_allocate(int n) {
    CHECK(step == 1 && n == 168);
    step++; return failed_alloc ? (void *)0 : property;
}
void native_property_construct(void *p, int id, int type, int flags, const char *name, void *b, int final) {
    CHECK(step == 2 && p == property && id == 366 && type == 2 && flags == 1 && name == native_supply_name && b == bucket && final == 1);
    step++;
}
void native_property_insert(void *map, const char *name, void *p) {
    CHECK(step == 3 && map == &bucket[60] && name == native_supply_name && p == property);
    step++;
}
int native_get_heatlink_heat(void *g) { CHECK(g == globals); getters++; return heat_type; }
void native_supply_setter(void *g, u32 t) { CHECK(g == globals); setters++; setting = t; }
extern u32 register_supply_candidate(void *, const char *, int, int, int);
extern int guard_supply_candidate(void *, u32, u32, u32);
extern int call_adapter(void *, void *);
#define FP(x) ((u32)(x) << 16)
int test_main(void) {
    bucket[56] = (u32)&bucket[60];
    CHECK(register_supply_candidate(bucket, previous_name, 106, 91, 7) == 0x1234 && step == 5);
    step = 0; failed_alloc = 1;
    CHECK(register_supply_candidate(bucket, previous_name, 106, 91, 7) == 0x1234 && step == 2);
    /* Synthetic limits for this mock test only, not installation defaults. */
    heat_type = 1;
    CHECK(!guard_supply_candidate(0, FP(55), FP(40), FP(60)));
    CHECK(!guard_supply_candidate(globals, FP(55), 0, 0));
    CHECK(!guard_supply_candidate(globals, FP(55), FP(60), FP(40)));
    CHECK(!guard_supply_candidate(globals, FP(55), FP(40), FP(101)));
    CHECK(!guard_supply_candidate(globals, FP(39), FP(40), FP(60)));
    CHECK(!guard_supply_candidate(globals, FP(61), FP(40), FP(60)));
    CHECK(!guard_supply_candidate(globals, FP(55)+1, FP(40), FP(60)));
    CHECK(!guard_supply_candidate(globals, FP(55), FP(40)+1, FP(60)));
    CHECK(!guard_supply_candidate(globals, FP(55), FP(40), FP(60)+1));
    CHECK(!guard_supply_candidate(globals, 0xffffffffu, FP(40), FP(60)));
    CHECK(getters == 0 && setters == 0);
    heat_type = 0;
    CHECK(!guard_supply_candidate(globals, FP(55), FP(40), FP(60)) && getters == 1 && setters == 0);
    heat_type = 1;
    CHECK(guard_supply_candidate(globals, FP(55), FP(40), FP(60)) && getters == 2 && setters == 1 && setting == FP(55));
    CHECK(guard_supply_candidate(globals, FP(40), FP(40), FP(60)) && setting == FP(40));
    CHECK(guard_supply_candidate(globals, FP(60), FP(40), FP(60)) && setting == FP(60));
    dispatcher[7] = (u32)globals; event[12] = FP(55);
    CHECK(!call_adapter(dispatcher, event));
    CHECK(getters == 4 && setters == 3 && setting == FP(60));
    return 0;
}
C
clang --target=armv7-linux-gnueabihf -marm -c candidate.S -o candidate.o
clang --target=armv7-linux-gnueabihf -marm -ffreestanding -fno-builtin -fno-stack-protector -fno-pic -fno-pie -O1 -c mock.c -o mock.o
ld.lld -m armelf_linux_eabi -static -e _start candidate.o mock.o -o mock-arm
readelf -h mock-arm
readelf -r mock-arm
if qemu-arm -cpu cortex-a8 ./mock-arm; then
    echo 'PASS: mock ARM registration ABI, guard and disabled adapter'
else
    status=$?
    echo "FAIL: ARM harness exit $status (assertion line or signal)" >&2
    exit "$status"
fi
