/* SPDX-License-Identifier: (MIT OR Apache-2.0) */
/*
 * The kernel structures world_write.bpf.c reads, declared with only the
 * members it touches. `preserve_access_index` makes every access to them
 * a CO-RE relocation: the loader rewrites each member's offset from the
 * running kernel's BTF (/sys/kernel/btf/vmlinux), so one object loads on
 * RHEL 10's 6.12 and Ubuntu 26.04's 7.0 alike, and refuses to load on a
 * kernel where a member is gone rather than reading the wrong bytes.
 *
 * Member names and nesting must match the kernel's; types need only be
 * compatible (any integer for an integer). A generated vmlinux.h would
 * do the same in three megabytes nobody reads.
 */
#ifndef AGENTIC_JOB_VMLINUX_MIN_H
#define AGENTIC_JOB_VMLINUX_MIN_H

/* The integer types libbpf's helper declarations are written in. */
typedef unsigned char __u8;
typedef unsigned short __u16;
typedef unsigned int __u32;
typedef unsigned long long __u64;
typedef signed char __s8;
typedef short __s16;
typedef int __s32;
typedef long long __s64;
typedef __u16 __be16;
typedef __u32 __be32;
typedef __u64 __be64;
typedef __u16 __le16;
typedef __u32 __le32;
typedef __u64 __le64;
typedef __u32 __wsum;
typedef __u16 __sum16;
typedef __u16 umode_t;
typedef __u32 uid_t;
typedef __u32 gid_t;
typedef _Bool bool;
#define true 1
#define false 0

/* include/uapi/linux/bpf.h: the one map type the program uses. */
enum bpf_map_type {
	BPF_MAP_TYPE_RINGBUF = 27,
};

typedef struct {
	uid_t val;
} kuid_t;

typedef struct {
	gid_t val;
} kgid_t;

struct posix_acl;

struct inode {
	umode_t i_mode;
	kuid_t i_uid;
	kgid_t i_gid;
	unsigned long i_ino;
	struct posix_acl *i_acl;
} __attribute__((preserve_access_index));

struct group_info {
	int ngroups;
	kgid_t gid[];
} __attribute__((preserve_access_index));

struct uid_gid_extent {
	__u32 first;
	__u32 lower_first;
	__u32 count;
};

/*
 * Up to five extents live inline, more are allocated behind a pointer,
 * and the two share an anonymous union after nr_extents. The union is
 * not declared: aya's loader cannot relocate an access through an
 * anonymous member (docs/world-write.md), so world_write.bpf.c finds
 * it at UID_GID_MAP_EXTENTS_OFFSET, where it has been since Linux 4.15.
 */
#define UID_GID_MAP_MAX_BASE_EXTENTS 5
#define UID_GID_MAP_EXTENTS_OFFSET 8

struct uid_gid_map {
	__u32 nr_extents;
} __attribute__((preserve_access_index));

struct user_namespace {
	struct uid_gid_map uid_map;
	int level;
} __attribute__((preserve_access_index));

struct cred {
	kuid_t fsuid;
	kgid_t fsgid;
	struct user_namespace *user_ns;
	struct group_info *group_info;
} __attribute__((preserve_access_index));

struct task_struct {
	const struct cred *cred;
} __attribute__((preserve_access_index));

#endif
