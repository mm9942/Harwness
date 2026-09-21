/* DoD BPF ring-buffer ABI v1.
 *
 * This file is the C half of `harw-dod-bpf/src/abi.rs`.  It has no implicit
 * layout: every fixed-width field is packed and every offset is asserted at
 * compile time.  BPF objects are built only for bpfel, so every multibyte
 * field is explicitly specified as little endian (except TCP payload port,
 * which remains network byte order by contract).
 */
#ifndef HARW_DOD_WIRE_V1_H
#define HARW_DOD_WIRE_V1_H

#include "vmlinux.h"
#include "harw_dod_bpf_helpers.h"

#if __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error "DoD wire ABI v1 is built only for bpfel"
#endif

#define HWD_WIRE_MAGIC_0 'H'
#define HWD_WIRE_MAGIC_1 'D'
#define HWD_WIRE_MAGIC_2 'O'
#define HWD_WIRE_MAGIC_3 'D'
#define HWD_WIRE_VERSION_V1 1
#define HWD_WIRE_HEADER_LEN_V1 56

#define HWD_EVENT_EXEC 1
#define HWD_EVENT_PROCESS_EXIT 2
#define HWD_EVENT_TCP_CONNECT 3

#define HWD_EXEC_PATH_TRUNCATED 0x01
#define HWD_EXEC_PATH_UNAVAILABLE 0x02
#define HWD_EXEC_PATH_MAX 256
#define HWD_MAX_SCOPE_CGROUP_IDS 16384

struct hwd_wire_header_v1 {
    __u8 magic[4];
    __u16 version_le;
    __u8 event_type;
    __u8 flags;
    __u32 record_len_le;
    __u64 ktime_ns_le;
    __u64 sequence_le;
    __u32 tgid_le;
    __u32 pid_le;
    __u32 ppid_le;
    __u32 uid_le;
    __u64 cgroup_id_le;
    __u16 payload_len_le;
    __u16 reserved_le;
} __attribute__((packed));

_Static_assert(sizeof(struct hwd_wire_header_v1) == HWD_WIRE_HEADER_LEN_V1, "wire header size");
_Static_assert(__builtin_offsetof(struct hwd_wire_header_v1, version_le) == 4, "wire version offset");
_Static_assert(__builtin_offsetof(struct hwd_wire_header_v1, event_type) == 6, "wire type offset");
_Static_assert(__builtin_offsetof(struct hwd_wire_header_v1, record_len_le) == 8, "wire length offset");
_Static_assert(__builtin_offsetof(struct hwd_wire_header_v1, ktime_ns_le) == 12, "wire ktime offset");
_Static_assert(
    __builtin_offsetof(struct hwd_wire_header_v1, sequence_le) == 20, "wire sequence offset");
_Static_assert(__builtin_offsetof(struct hwd_wire_header_v1, tgid_le) == 28, "wire tgid offset");
_Static_assert(__builtin_offsetof(struct hwd_wire_header_v1, cgroup_id_le) == 44, "wire cgroup offset");
_Static_assert(__builtin_offsetof(struct hwd_wire_header_v1, payload_len_le) == 52, "wire payload length offset");

struct hwd_exec_payload_v1 {
    __u8 comm[16];
    __u16 path_len_le;
    __u8 path[];
} __attribute__((packed));

_Static_assert(__builtin_offsetof(struct hwd_exec_payload_v1, path) == 18, "exec payload prefix size");

struct hwd_tcp_payload_v1 {
    __u8 family;
    __u8 port_be[2];
    __u8 address[16];
} __attribute__((packed));

_Static_assert(sizeof(struct hwd_tcp_payload_v1) == 19, "tcp payload size");

/* The map names below are part of the object contract and are checked by the
 * host loader before any attachment.  `SCOPE_CGROUP_IDS` contains the exact
 * current cgroup IDs allowed by a resolved profile; key zero is exclusively
 * the explicit host-profile marker. */
struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 1 << 24);
} EVENTS SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, HWD_MAX_SCOPE_CGROUP_IDS);
    __type(key, __u64);
    __type(value, __u8);
} SCOPE_CGROUP_IDS SEC(".maps");

/* Per-CPU counters indexed by event type.  A reservation failure increments
 * the matching slot after the sequence has advanced, making dropped records
 * observable rather than silently invisible. */
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 4);
    __type(key, __u32);
    __type(value, __u64);
} LOSS_COUNTS SEC(".maps");

/* A single global map value provides a monotonically increasing issuance
 * sequence.  Cross-CPU delivery can still be reordered, so consumers must
 * not use it as a wall clock; it exists to surface loss and ordering gaps. */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u64);
} SEQUENCE SEC(".maps");

static __always_inline int hwd_in_scope(void)
{
    __u64 cgroup_id = bpf_get_current_cgroup_id();
    __u64 host_marker = 0;

    if (bpf_map_lookup_elem(&SCOPE_CGROUP_IDS, &cgroup_id))
        return 1;
    return bpf_map_lookup_elem(&SCOPE_CGROUP_IDS, &host_marker) != 0;
}

static __always_inline void hwd_note_loss(__u32 event_type)
{
    __u64 *loss = bpf_map_lookup_elem(&LOSS_COUNTS, &event_type);
    if (loss)
        *loss += 1;
}

static __always_inline __u64 hwd_next_sequence(void)
{
    __u32 key = 0;
    __u64 *sequence = bpf_map_lookup_elem(&SEQUENCE, &key);
    if (!sequence)
        return 0;
    return __sync_fetch_and_add(sequence, 1) + 1;
}

static __always_inline __u32 hwd_parent_tgid(void)
{
    struct task_struct *task = bpf_get_current_task_btf();
    struct task_struct *parent = 0;
    __u32 ppid = 0;

    if (!task)
        return 0;
    /* `vmlinux.h` is generated from the target BTF.  These are BTF-backed
     * field accesses, not hand-written tracepoint offsets. */
    if (bpf_probe_read_kernel(&parent, sizeof(parent), &task->real_parent) != 0 || !parent)
        return 0;
    (void)bpf_probe_read_kernel(&ppid, sizeof(ppid), &parent->tgid);
    return ppid;
}

static __always_inline void hwd_fill_header(struct hwd_wire_header_v1 *header,
                                             __u8 event_type, __u8 flags,
                                             __u16 payload_len, __u64 sequence)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    __u64 uid_gid = bpf_get_current_uid_gid();

    header->magic[0] = HWD_WIRE_MAGIC_0;
    header->magic[1] = HWD_WIRE_MAGIC_1;
    header->magic[2] = HWD_WIRE_MAGIC_2;
    header->magic[3] = HWD_WIRE_MAGIC_3;
    header->version_le = HWD_WIRE_VERSION_V1;
    header->event_type = event_type;
    header->flags = flags;
    header->record_len_le = HWD_WIRE_HEADER_LEN_V1 + payload_len;
    header->ktime_ns_le = bpf_ktime_get_ns();
    header->sequence_le = sequence;
    header->tgid_le = pid_tgid >> 32;
    header->pid_le = (__u32)pid_tgid;
    header->ppid_le = hwd_parent_tgid();
    header->uid_le = (__u32)uid_gid;
    header->cgroup_id_le = bpf_get_current_cgroup_id();
    header->payload_len_le = payload_len;
    header->reserved_le = 0;
}

static __always_inline int hwd_emit_exit(void)
{
    struct hwd_wire_header_v1 *header;
    __u64 sequence;

    if (!hwd_in_scope())
        return 0;
    sequence = hwd_next_sequence();
    header = bpf_ringbuf_reserve(&EVENTS, HWD_WIRE_HEADER_LEN_V1, 0);
    if (!header) {
        hwd_note_loss(HWD_EVENT_PROCESS_EXIT);
        return 0;
    }
    hwd_fill_header(header, HWD_EVENT_PROCESS_EXIT, 0, 0, sequence);
    bpf_ringbuf_submit(header, 0);
    return 0;
}

static __always_inline int hwd_emit_exec(const void *filename)
{
    /* Two guard bytes distinguish a complete 256-byte path (257 bytes with
     * its NUL) from a path that exceeds the v1 capture bound. */
    __u8 path[HWD_EXEC_PATH_MAX + 2] = {};
    long copied;
    __u16 path_len = 0;
    __u8 flags = 0;
    __u16 payload_len;
    __u64 sequence;
    struct hwd_wire_header_v1 *header;
    struct hwd_exec_payload_v1 *payload;

    /* Scope is evaluated before reading the path or reserving ring-buffer
     * memory: non-selected processes do not yield a process-data record. */
    if (!hwd_in_scope())
        return 0;

    copied = bpf_probe_read_kernel_str(path, sizeof(path), filename);
    if (copied < 0) {
        flags |= HWD_EXEC_PATH_UNAVAILABLE;
    } else {
        if (copied > HWD_EXEC_PATH_MAX + 1) {
            path_len = HWD_EXEC_PATH_MAX;
            flags |= HWD_EXEC_PATH_TRUNCATED;
        } else {
            path_len = copied - 1; /* omit the helper's trailing C NUL */
        }
    }

    payload_len = sizeof(struct hwd_exec_payload_v1) + path_len;
    sequence = hwd_next_sequence();
    header = bpf_ringbuf_reserve(&EVENTS, HWD_WIRE_HEADER_LEN_V1 + payload_len, 0);
    if (!header) {
        hwd_note_loss(HWD_EVENT_EXEC);
        return 0;
    }
    hwd_fill_header(header, HWD_EVENT_EXEC, flags, payload_len, sequence);
    payload = (struct hwd_exec_payload_v1 *)(header + 1);
    (void)bpf_get_current_comm(payload->comm, sizeof(payload->comm));
    payload->path_len_le = path_len;
    if (path_len > 0)
        hwd_memcpy(payload->path, path, path_len);
    bpf_ringbuf_submit(header, 0);
    return 0;
}

static __always_inline int hwd_emit_tcp(__u8 family, const __u8 port_be[2], const __u8 *address)
{
    struct hwd_wire_header_v1 *header;
    struct hwd_tcp_payload_v1 *payload;
    __u64 sequence;

    if (!hwd_in_scope())
        return 0;
    sequence = hwd_next_sequence();
    header = bpf_ringbuf_reserve(&EVENTS, HWD_WIRE_HEADER_LEN_V1 + sizeof(struct hwd_tcp_payload_v1), 0);
    if (!header) {
        hwd_note_loss(HWD_EVENT_TCP_CONNECT);
        return 0;
    }
    hwd_fill_header(header, HWD_EVENT_TCP_CONNECT, 0, sizeof(struct hwd_tcp_payload_v1), sequence);
    payload = (struct hwd_tcp_payload_v1 *)(header + 1);
    payload->family = family;
    payload->port_be[0] = port_be[0];
    payload->port_be[1] = port_be[1];
    hwd_memcpy(payload->address, address, sizeof(payload->address));
    bpf_ringbuf_submit(header, 0);
    return 0;
}

#endif
