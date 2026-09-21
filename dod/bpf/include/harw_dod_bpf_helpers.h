/* Minimal, versioned helper declarations for the standalone DoD BPF build.
 *
 * We intentionally vendor this tiny surface instead of relying on whatever
 * libbpf headers happen to be installed on a deployment host.  The generated
 * vmlinux.h remains target-kernel input and is produced by the Makefile from
 * /sys/kernel/btf/vmlinux before compilation.
 */
#ifndef HARW_DOD_BPF_HELPERS_H
#define HARW_DOD_BPF_HELPERS_H

#define SEC(NAME) __attribute__((section(NAME), used))
#define __always_inline inline __attribute__((always_inline))
#define __uint(name, val) int (*name)[val]
#define __type(name, val) typeof(val) *name

#define BPF_MAP_TYPE_HASH 1
#define BPF_MAP_TYPE_ARRAY 2
#define BPF_MAP_TYPE_PERCPU_ARRAY 6
#define BPF_MAP_TYPE_RINGBUF 27

#define BPF_ANY 0

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static __u64 (*bpf_ktime_get_ns)(void) = (void *)5;
static long (*bpf_probe_read_kernel)(void *dst, __u32 size, const void *unsafe_ptr) = (void *)113;
static long (*bpf_probe_read_kernel_str)(void *dst, __u32 size, const void *unsafe_ptr) = (void *)115;
static __u64 (*bpf_get_current_pid_tgid)(void) = (void *)14;
static __u64 (*bpf_get_current_uid_gid)(void) = (void *)15;
static long (*bpf_get_current_comm)(void *buf, __u32 size_of_buf) = (void *)16;
static __u64 (*bpf_get_current_cgroup_id)(void) = (void *)80;
static void *(*bpf_ringbuf_reserve)(void *ringbuf, __u64 size, __u64 flags) = (void *)131;
static void (*bpf_ringbuf_submit)(void *data, __u64 flags) = (void *)132;
static struct task_struct *(*bpf_get_current_task_btf)(void) = (void *)158;

/* eBPF does not support __builtin_memcpy for variable-length copies.  Use a
 * bounded byte loop that the verifier can unroll/roll as needed. */
static __always_inline void hwd_memcpy(__u8 *dst, const __u8 *src, __u32 n)
{
    __u32 i;
    for (i = 0; i < n; i++)
        dst[i] = src[i];
}

#endif
