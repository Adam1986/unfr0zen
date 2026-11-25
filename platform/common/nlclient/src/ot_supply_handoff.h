/* Experimental one-shot native OpenTherm handoff. No default limits.
 * All records are little-endian uint32 words, written atomically by a trusted
 * local producer. This is not a public wire protocol or a remote API. */
#ifndef OT_SUPPLY_HANDOFF_H
#define OT_SUPPLY_HANDOFF_H
#include <stdint.h>
#define OT_ARM_MAGIC 0x3141544fu /* OTA1 */
#define OT_REQ_MAGIC 0x3152544fu /* OTR1 */
#define OT_ACK_MAGIC 0x314b544fu /* OTK1 */
#define OT_FIXED 65536u
#define OT_MAX (100u * OT_FIXED) /* representation bound, NOT operating range */
#define OT_TTL 30u
struct ot_arm { uint32_t magic, pid, token, expires, lower, upper; };
struct ot_request { uint32_t magic, pid, token, expires, expected, target; };
struct ot_ack { uint32_t magic, pid, token, status, before, after; };
enum { OT_REJECTED = 0, OT_VALIDATED = 1, OT_SETTER_RETURNED = 2, OT_READBACK_MISMATCH = 3 };
_Static_assert(sizeof(struct ot_arm) == 24 && sizeof(struct ot_request) == 24 && sizeof(struct ot_ack) == 24,
               "OpenTherm record ABI");
static inline int ot_whole(uint32_t v) { return v && v <= OT_MAX && !(v & (OT_FIXED - 1)); }
static inline int ot_live(uint32_t expires, uint32_t now) {
    return now && expires >= now && expires - now <= OT_TTL;
}
static inline int ot_envelope(const struct ot_arm *a, const struct ot_request *r, uint32_t pid, uint32_t now) {
    return a->magic == OT_ARM_MAGIC && r->magic == OT_REQ_MAGIC && pid > 1 &&
           a->pid == pid && r->pid == pid && a->token && a->token == r->token &&
           ot_live(a->expires, now) && ot_live(r->expires, now) && r->expires <= a->expires &&
           ot_whole(a->lower) && ot_whole(a->upper) && a->lower <= a->upper &&
           ot_whole(r->expected) && r->expected >= a->lower && r->expected <= a->upper &&
           ot_whole(r->target) && r->target >= a->lower && r->target <= a->upper &&
           r->target != r->expected;
}
/* Both the arm and request must already have been claimed before calling.
 * A successful return is only an in-memory observation, NOT a boiler ACK. */
/* Keep native function references as direct ARM CALL relocations. The
 * in-memory firmware linker has no GOT and must not receive GOT_PREL. */
static inline __attribute__((always_inline)) struct ot_ack ot_apply(const struct ot_arm *a, const struct ot_request *r,
                                     uint32_t pid, uint32_t now, void *globals,
                                     uint32_t (*heat)(void *), uint32_t (*get)(void *),
                                     void (*set)(void *, uint32_t), int writes) {
    struct ot_ack ack = {OT_ACK_MAGIC, pid, r->token, OT_REJECTED, 0, 0};
    if (!globals || !ot_envelope(a, r, pid, now) || heat(globals) != 1) return ack;
    ack.before = ack.after = get(globals);
    if (ack.before != r->expected) return ack;
    ack.status = OT_VALIDATED;
    if (writes) {
        set(globals, r->target);
        ack.after = get(globals);
        ack.status = ack.after == r->target ? OT_SETTER_RETURNED : OT_READBACK_MISMATCH;
    }
    return ack;
}

/* Separate authorisation for the first supervised write trial only.
 * Never accept a dry-run record as permission to write. These two settings
 * have already been exercised via the physical menu on the reviewed unit.
 * Do not treat this narrow trial interval as a general operating range. */
#define OT_WRITE_ARM_MAGIC 0x3157544fu /* OTW1 */
#define OT_WRITE_REQ_MAGIC 0x3153544fu /* OTS1 */
static inline __attribute__((always_inline)) struct ot_ack ot_apply_isolated_write(const struct ot_arm *a, const struct ot_request *r,
                                     uint32_t pid, uint32_t now, void *globals,
                                     uint32_t (*heat)(void *), uint32_t (*get)(void *),
                                     void (*set)(void *, uint32_t)) {
    struct ot_ack rejected = {OT_ACK_MAGIC, pid, r->token, OT_REJECTED, 0, 0};
    if (!set || a->magic != OT_WRITE_ARM_MAGIC || r->magic != OT_WRITE_REQ_MAGIC ||
        a->lower != 50u * OT_FIXED || a->upper != 55u * OT_FIXED ||
        r->expected != 55u * OT_FIXED || r->target != 50u * OT_FIXED)
        return rejected;
    struct ot_arm arm = *a;
    struct ot_request request = *r;
    arm.magic = OT_ARM_MAGIC;
    request.magic = OT_REQ_MAGIC;
    return ot_apply(&arm, &request, pid, now, globals, heat, get, set, 1);
}
#endif
