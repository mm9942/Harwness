#include "harw_dod_wire_v1.h"

/* Exit identity comes exclusively from helpers in the exiting task context.
 * No tracepoint-field layout is needed or assumed for this v1 event. */
SEC("tracepoint/sched/sched_process_exit")
int dod_sched_process_exit(void *ctx)
{
    (void)ctx;
    return hwd_emit_exit();
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
