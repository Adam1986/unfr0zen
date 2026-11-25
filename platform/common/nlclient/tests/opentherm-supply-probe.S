/* Test-only candidate, not linked into production builds.
 * Uses the reviewed 2025 globals pointer at bucket + 28. No property
 * registration, allocation, settings mutation or native setter call.
 * Restore the frame before tail-branching to the existing commit hook. */
.syntax unified
.arm
.text
.align 2
.global supply_probe_commit
.type supply_probe_commit,%function
supply_probe_commit:
    push {r4-r8,lr}
    sub sp,sp,#16
    mov r4,r0
    cmp r4,#0
    beq done
    ldr r5,[r4,#28]
    cmp r5,#0
    beq done
    mov r0,r5
    bl native_get_heatlink_heat
    mov r6,r0
    mvn r7,#0
    cmp r6,#1
    bne record
    mov r0,r5
    bl native_get_supply
    cmp r0,#6553600
    movls r7,r0
record:
    movw r0,#0x544f
    movt r0,#0x3153
    str r0,[sp]
    bl native_getpid
    str r0,[sp,#4]
    str r6,[sp,#8]
    str r7,[sp,#12]
    adr r0,tmp_path
    adr r1,write_mode
    bl native_fopen
    cmp r0,#0
    beq done
    mov r8,r0
    mov r3,r8
    mov r0,sp
    mov r1,#1
    mov r2,#16
    bl native_fwrite
    mov r7,r0
    mov r0,r8
    bl native_fclose
    cmp r7,#16
    cmpeq r0,#0
    bne done
    adr r0,tmp_path
    adr r1,state_path
    bl native_rename
done:
    mov r0,r4
    add sp,sp,#16
    pop {r4-r8,lr}
    b existing_device_commit
.size supply_probe_commit,.-supply_probe_commit
.align 2
tmp_path: .asciz "/var/run/unfr0zen/ot-supply-probe.tmp"
state_path: .asciz "/var/run/unfr0zen/ot-supply-probe.state"
write_mode: .asciz "wb"
.align 2
.section .note.GNU-stack,"",%progbits
