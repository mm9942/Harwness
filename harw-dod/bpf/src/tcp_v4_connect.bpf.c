#include "harw_dod_wire_v1.h"

struct hwd_sockaddr_in {
    __u16 family;
    __u8 port_be[2];
    __u8 address[4];
    __u8 zero[8];
} __attribute__((packed));

/* Fentry receives the target function arguments in the u64 context array.
 * tcp_v4_connect's second argument is the kernel-copied sockaddr supplied by
 * the calling task.  The loader uses target BTF to attach precisely to that
 * function; if it is absent or incompatible, this sensor does not start. */
SEC("fentry/tcp_v4_connect")
int dod_tcp_v4_connect(unsigned long long *ctx)
{
    const void *uaddr = (const void *)(unsigned long)ctx[1];
    struct hwd_sockaddr_in addr = {};

    if (bpf_probe_read_kernel(&addr, sizeof(addr), uaddr) != 0)
        return 0;
    if (addr.family != 2)
        return 0;
    return hwd_emit_tcp(4, addr.port_be, addr.address);
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
