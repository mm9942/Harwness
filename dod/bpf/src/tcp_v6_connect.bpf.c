#include "harw_dod_wire_v1.h"

struct hwd_sockaddr_in6 {
    __u16 family;
    __u8 port_be[2];
    __u32 flowinfo;
    __u8 address[16];
    __u32 scope_id;
} __attribute__((packed));

SEC("fentry/tcp_v6_connect")
int dod_tcp_v6_connect(unsigned long long *ctx)
{
    const void *uaddr = (const void *)(unsigned long)ctx[1];
    struct hwd_sockaddr_in6 addr = {};

    if (bpf_probe_read_kernel(&addr, sizeof(addr), uaddr) != 0)
        return 0;
    if (addr.family != 10)
        return 0;
    return hwd_emit_tcp(6, addr.port_be, addr.address);
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
