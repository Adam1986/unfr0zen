#include <assert.h>
#include <stdio.h>
#include "ot_supply_handoff.h"
struct globals { uint32_t heat, supply, calls; int ignore; };
static uint32_t heat(void *p) { return ((struct globals *)p)->heat; }
static uint32_t get(void *p) { return ((struct globals *)p)->supply; }
static void set(void *p, uint32_t v) { struct globals *g = p; g->calls++; if (!g->ignore) g->supply = v; }
#define DEG(v) ((v)*OT_FIXED)
int main(void) {
    const struct ot_arm a = {OT_ARM_MAGIC, 42, 123, 1030, DEG(50), DEG(55)};
    const struct ot_request r = {OT_REQ_MAGIC, 42, 123, 1020, DEG(50), DEG(55)};
    struct globals g = {1, DEG(50), 0, 0};
    assert(ot_apply(&a,&r,42,1000,&g,heat,get,set,0).status == OT_VALIDATED && g.calls == 0);
    assert(ot_apply(&a,&r,42,1000,&g,heat,get,set,1).status == OT_SETTER_RETURNED && g.calls == 1 && g.supply == DEG(55));
    assert(ot_apply(&a,&r,42,1000,&g,heat,get,set,1).status == OT_REJECTED && g.calls == 1); /* expected-value mismatch */
    g.supply = DEG(50); g.calls = 0;
    struct ot_arm bad_a; struct ot_request bad_r;
#define BAD_A(field,val) do { bad_a=a; bad_a.field=(val); assert(ot_apply(&bad_a,&r,42,1000,&g,heat,get,set,1).status==OT_REJECTED); } while(0)
#define BAD_R(field,val) do { bad_r=r; bad_r.field=(val); assert(ot_apply(&a,&bad_r,42,1000,&g,heat,get,set,1).status==OT_REJECTED); } while(0)
    BAD_A(magic,0); BAD_A(pid,43); BAD_A(token,0); BAD_A(token,124); BAD_A(expires,999); BAD_A(expires,1031); BAD_A(lower,0); BAD_A(lower,DEG(56)); BAD_A(lower,DEG(50)+1); BAD_A(upper,DEG(101)); BAD_A(upper,DEG(54)); BAD_A(upper,DEG(49));
    BAD_R(magic,0); BAD_R(pid,43); BAD_R(token,124); BAD_R(expires,999); BAD_R(expires,1031); BAD_R(expected,DEG(51)); BAD_R(target,0); BAD_R(target,DEG(20)); BAD_R(target,DEG(56)); BAD_R(target,DEG(55)+1); BAD_R(target,DEG(50));
    assert(ot_apply(&a,&r,43,1000,&g,heat,get,set,1).status == OT_REJECTED);
    assert(ot_apply(&a,&r,42,1000,0,heat,get,set,1).status == OT_REJECTED);
    g.heat=0; assert(ot_apply(&a,&r,42,1000,&g,heat,get,set,1).status == OT_REJECTED); g.heat=1;
    g.supply=DEG(20); assert(ot_apply(&a,&r,42,1000,&g,heat,get,set,1).status == OT_REJECTED); g.supply=DEG(50);
    assert(g.calls==0);
    g.ignore=1; assert(ot_apply(&a,&r,42,1000,&g,heat,get,set,1).status == OT_READBACK_MISMATCH && g.calls==1);
    /* Mode 2 must never accept the earlier dry-run authorisation, or
     * any other temperature transition, even if the envelope is valid. */
    const struct ot_arm wa = {OT_WRITE_ARM_MAGIC, 42, 123, 1030, DEG(50), DEG(55)};
    const struct ot_request wr = {OT_WRITE_REQ_MAGIC, 42, 123, 1020, DEG(55), DEG(50)};
    g = (struct globals){1, DEG(55), 0, 0};
#define WRITE(a,r,pid,now) ot_apply_isolated_write((a),(r),(pid),(now),&g,heat,get,set)
    bad_a=wa; bad_a.magic=OT_ARM_MAGIC;
    assert(WRITE(&bad_a,&wr,42,1000).status==OT_REJECTED);
    bad_r=wr; bad_r.magic=OT_REQ_MAGIC;
    assert(WRITE(&wa,&bad_r,42,1000).status==OT_REJECTED);
    bad_a=wa; bad_a.lower=DEG(20);
    assert(WRITE(&bad_a,&wr,42,1000).status==OT_REJECTED);
    bad_r=wr; bad_r.target=DEG(51);
    assert(WRITE(&wa,&bad_r,42,1000).status==OT_REJECTED);
    bad_r=wr; bad_r.target=DEG(55); bad_r.expected=DEG(50);
    assert(WRITE(&wa,&bad_r,42,1000).status==OT_REJECTED);
    assert(WRITE(&wa,&wr,43,1000).status==OT_REJECTED);
    assert(WRITE(&wa,&wr,42,1021).status==OT_REJECTED);
    assert(WRITE(&wa,&wr,42,999).status==OT_REJECTED);
    bad_r=wr; bad_r.token=124;
    assert(WRITE(&wa,&bad_r,42,1000).status==OT_REJECTED);
    g.heat=0;
    assert(WRITE(&wa,&wr,42,1000).status==OT_REJECTED);
    g.heat=1; g.supply=DEG(54);
    assert(WRITE(&wa,&wr,42,1000).status==OT_REJECTED);
    assert(g.calls==0);
    struct ot_ack pre_dispatch = ot_pre_dispatch_rejection(42, 99);
    assert(pre_dispatch.magic == OT_ACK_MAGIC && pre_dispatch.pid == 42
        && pre_dispatch.token == 99 && pre_dispatch.status == OT_REJECTED
        && pre_dispatch.before == pre_dispatch.after);

    g.supply=DEG(55);
    struct ot_ack written=WRITE(&wa,&wr,42,1000);
    assert(written.status==OT_SETTER_RETURNED && written.before==DEG(55)
        && written.after==DEG(50) && g.calls==1);
    assert(WRITE(&wa,&wr,42,1000).status==OT_REJECTED && g.calls==1);
    g.supply=DEG(55); g.ignore=1;
    written=WRITE(&wa,&wr,42,1000);
    assert(written.status==OT_READBACK_MISMATCH && written.after==DEG(55) && g.calls==2);
    /* Managed mode is separate from both supervised and dry-run modes.
     * Test both directions and intermediate whole degrees in mocks only. */
    const struct ot_arm ma = {OT_MANAGED_ARM_MAGIC,42,123,1030,DEG(50),DEG(55)};
    struct ot_request mr = {OT_MANAGED_REQ_MAGIC,42,123,1020,DEG(55),DEG(50)};
    g = (struct globals){1, DEG(55), 0, 0};
#define MANAGED(a,r) ot_apply_managed((a),(r),42,1000,&g,heat,get,set)
    assert(MANAGED(&wa,&wr).status == OT_REJECTED && g.calls == 0);
    assert(MANAGED(&a,&r).status == OT_REJECTED && g.calls == 0);
    bad_r=mr; bad_r.target=DEG(49);
    assert(MANAGED(&ma,&bad_r).status == OT_REJECTED && g.calls == 0);
    bad_a=ma; bad_a.upper=DEG(101);
    assert(MANAGED(&bad_a,&mr).status == OT_REJECTED && g.calls == 0);
    bad_a=ma; bad_a.lower=0;
    assert(MANAGED(&bad_a,&mr).status == OT_REJECTED && g.calls == 0);
    bad_r=mr; bad_r.target=DEG(52)+1;
    assert(MANAGED(&ma,&bad_r).status == OT_REJECTED && g.calls == 0);
    bad_r=mr; bad_r.token++;
    assert(MANAGED(&ma,&bad_r).status == OT_REJECTED && g.calls == 0);
    assert(MANAGED(&ma,&mr).status == OT_SETTER_RETURNED && g.supply == DEG(50));
    assert(MANAGED(&ma,&mr).status == OT_REJECTED && g.calls == 1);
    mr.expected=DEG(50); mr.target=DEG(53);
    assert(MANAGED(&ma,&mr).status == OT_SETTER_RETURNED && g.supply == DEG(53));
    mr.expected=DEG(53); mr.target=DEG(55);
    assert(MANAGED(&ma,&mr).status == OT_SETTER_RETURNED && g.supply == DEG(55) && g.calls == 3);
    const struct ot_arm ma60 = {OT_MANAGED_ARM_MAGIC,42,124,1030,DEG(50),DEG(60)};
    mr.pid=42; mr.token=124; mr.expires=1020; mr.expected=DEG(55); mr.target=DEG(60);
    g.supply=DEG(55);
    assert(MANAGED(&ma60,&mr).status == OT_SETTER_RETURNED && g.supply == DEG(60) && g.calls == 4);
    mr.expected=DEG(60); mr.target=DEG(61);
    assert(MANAGED(&ma60,&mr).status == OT_REJECTED && g.calls == 4);
    puts("OpenTherm handoff: all guard, isolated-write and mock setter assertions passed");
}
