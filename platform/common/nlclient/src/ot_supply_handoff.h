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
static inline int ot_authorized(const struct ot_arm *a, const struct ot_request *r, uint32_t pid,
                                uint32_t now, uint32_t heating, uint32_t current) {
    return a->magic == OT_ARM_MAGIC && r->magic == OT_REQ_MAGIC && pid > 1 &&
           a->pid == pid && r->pid == pid && a->token && a->token == r->token &&
           ot_live(a->expires, now) && ot_live(r->expires, now) && r->expires <= a->expires &&
           heating == 1 && ot_whole(a->lower) && ot_whole(a->upper) && a->lower <= a->upper &&
           ot_whole(current) && current >= a->lower && current <= a->upper &&
           r->expected == current && ot_whole(r->target) && r->target >= a->lower &&
           r->target <= a->upper && r->target != current;
}
/* Both the arm and request must already have been claimed before calling.
 * A successful return is only an in-memory observation, NOT a boiler ACK. */
static inline struct ot_ack ot_apply(const struct ot_arm *a, const struct ot_request *r,
                                     uint32_t pid, uint32_t now, void *globals,
                                     uint32_t (*heat)(void *), uint32_t (*get)(void *),
                                     void (*set)(void *, uint32_t), int writes) {
    struct ot_ack ack = {OT_ACK_MAGIC, pid, r->token, OT_REJECTED, 0, 0};
    if (!globals) return ack;
    ack.before = ack.after = get(globals);
    if (!ot_authorized(a, r, pid, now, heat(globals), ack.before)) return ack;
    ack.status = OT_VALIDATED;
    if (writes) {
        set(globals, r->target);
        ack.after = get(globals);
        ack.status = ack.after == r->target ? OT_SETTER_RETURNED : OT_READBACK_MISMATCH;
    }
    return ack;
}
#endif
