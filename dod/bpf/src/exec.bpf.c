#include "harw_dod_wire_v1.h"

/* `__data_loc_filename` is obtained from the target kernel's generated
 * tracepoint BTF type.  We intentionally do not carry a guessed byte offset
 * for this tracepoint across kernel builds. */
SEC("tracepoint/sched/sched_process_exec")
int dod_sched_process_exec(struct trace_event_raw_sched_process_exec *ctx)
{
    const void *filename = (const void *)ctx + (ctx->__data_loc_filename & 0xffff);
    return hwd_emit_exec(filename);
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
