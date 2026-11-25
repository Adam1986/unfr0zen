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
    puts("OpenTherm handoff: all guard and mock setter assertions passed");
}
