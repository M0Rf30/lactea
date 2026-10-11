/* Host hooks required by projectm-eval plus small introspection helpers for the tests. */
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "projectm-eval/CompilerTypes.h"
#include "projectm-eval/MemoryBuffer.h"

/* Single-threaded use per context, no locking needed (one context per test thread). */
void projectm_eval_memory_host_lock_mutex() {}
void projectm_eval_memory_host_unlock_mutex() {}

size_t prjm_oracle_var_count(struct projectm_eval_context* ctx)
{
    size_t n = 0;
    for (prjm_eval_variable_entry_t* v = ctx->variables.first; v; v = v->next)
    {
        ++n;
    }
    return n;
}

/* Variables are listed newest first (linked list), index 0 is the most recently created. */
const char* prjm_oracle_var_at(struct projectm_eval_context* ctx, size_t index, double* value)
{
    prjm_eval_variable_entry_t* v = ctx->variables.first;
    while (v && index--)
    {
        v = v->next;
    }
    if (!v)
    {
        return NULL;
    }
    *value = v->variable->value;
    return v->variable->name;
}

/* Reads a local megabuf cell (allocating its block like the interpreter does). */
double prjm_oracle_megabuf_get(struct projectm_eval_context* ctx, int32_t index)
{
    PRJM_EVAL_F* p = prjm_eval_memory_allocate(ctx->memory, index);
    return p ? *p : 0.0;
}

double prjm_oracle_gmegabuf_get(struct projectm_eval_context* ctx, int32_t index)
{
    PRJM_EVAL_F* p = prjm_eval_memory_allocate(ctx->global_memory, index);
    return p ? *p : 0.0;
}
