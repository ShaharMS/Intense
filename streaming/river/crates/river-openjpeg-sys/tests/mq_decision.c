/* Differential check for River's combined MQ decoder exchange. Link with the
 * vendored libopenjp2.a. Both paths use the same unmodified byte refill. */
#include "opj_includes.h"
#include <stdio.h>

static OPJ_UINT32 reference(opj_mqc_t *m)
{
    const opj_mqc_state_t *state = *m->curctx;
    OPJ_UINT32 bit = state->mps;
    m->a -= state->qeval;
    if ((m->c >> 16) < state->qeval) {
        if (m->a < state->qeval) {
            *m->curctx = state->nmps;
        } else {
            bit ^= 1;
            *m->curctx = state->nlps;
        }
        m->a = state->qeval;
    } else {
        m->c -= state->qeval << 16;
        if (m->a >= 0x8000) return bit;
        if (m->a < state->qeval) {
            bit ^= 1;
            *m->curctx = state->nlps;
        } else {
            *m->curctx = state->nmps;
        }
    }
    opj_mqc_renormd(m);
    return bit;
}

int main(void)
{
    OPJ_BYTE bytes[] = {0xff, 0x7f, 0xff, 0xff, 0xff, 0xff};
    opj_mqc_t old = {0}, revised = {0};
    OPJ_UINT32 index, mps, a, lower;
    old.curctx = old.ctxs;
    revised.curctx = revised.ctxs;
    for (index = 0; index < 47; ++index) {
        for (mps = 0; mps < 2; ++mps) {
            opj_mqc_setstate(&old, 0, mps, (OPJ_INT32)index);
            const opj_mqc_state_t *initial = *old.curctx;
            for (a = 0x8000; a < 0x10000; ++a) {
                for (lower = 0; lower < 2; ++lower) {
                    OPJ_UINT32 got;
                    *old.curctx = *revised.curctx = initial;
                    old.a = revised.a = a;
                    old.c = revised.c = ((initial->qeval - lower) << 16) | (a ^ 0xa55a);
                    old.ct = revised.ct = a % 9;
                    old.bp = revised.bp = bytes + a % 3;
                    old.end_of_byte_stream_counter = revised.end_of_byte_stream_counter = 0;
                    OPJ_UINT32 expected = reference(&old);
                    opj_mqc_t *candidate = &revised;
                    opj_mqc_decode(got, candidate);
                    if (got != expected || old.a != revised.a || old.c != revised.c ||
                            old.ct != revised.ct || old.bp != revised.bp ||
                            *old.curctx != *revised.curctx ||
                            old.end_of_byte_stream_counter != revised.end_of_byte_stream_counter) {
                        fprintf(stderr, "MQ mismatch at state %u, MPS %u, A %u, lower %u\n",
                                index, mps, a, lower);
                        return 1;
                    }
                }
            }
        }
    }
    return 0;
}
