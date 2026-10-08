// SPDX-License-Identifier: (MIT OR Apache-2.0)
//
// Deny, for a non-root uid, a write that Unix permissions granted only
// through the "other" bits of a file or directory another uid owns: the
// kernel-side equivalent of `sandbox setup`'s walk that takes world write
// off every such path, applied to every process on the host for as long
// as the program is attached (docs/world-write.md).
//
// One body, attached one of two ways by the loader: as a BPF LSM program
// on the inode_permission hook where `bpf` is in the kernel's LSM list
// (RHEL 10), or as an fmod_ret program on security_inode_permission()
// itself where it is not (Ubuntu 26.04). Both run after the kernel's own
// permission check, so what they see is a write Unix permissions allowed.
//
// The license string below is what the kernel reads: the helpers it
// calls are GPL-only, and "Dual MIT/GPL" is one of the strings the kernel
// takes as GPL-compatible.

#include "vmlinux_min.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

char LICENSE[] SEC("license") = "Dual MIT/GPL";

// include/linux/fs.h
#define MAY_WRITE 0x00000002
// include/uapi/linux/stat.h
#define S_IFMT 00170000
#define S_IFREG 0100000
#define S_IFDIR 0040000
#define S_ISVTX 0001000
#define S_IWOTH 00002
#define EACCES 13

// Bounds the verifier needs on loops. A process in more groups than this
// is treated as in none of them (denied, never allowed by mistake); the
// kernel itself allows at most 340 extents in a uid map.
#define MAX_GROUPS 256
#define MAX_EXTENTS 340

// Whether the task's credentials put it in the inode's group.
static __always_inline bool in_group(const struct cred *cred, __u32 gid)
{
	if (BPF_CORE_READ(cred, fsgid.val) == gid)
		return true;
	struct group_info *groups = BPF_CORE_READ(cred, group_info);
	int count = BPF_CORE_READ(groups, ngroups);
	if (count > MAX_GROUPS)
		return false;
	// Relocate the array's offset once, then index into it.
	kgid_t *members = (void *)groups + bpf_core_field_offset(groups->gid);
	for (int i = 0; i < MAX_GROUPS; i++) {
		if (i >= count)
			break;
		kgid_t member;
		if (bpf_probe_read_kernel(&member, sizeof(member), members + i))
			return false;
		if (member.val == gid)
			return true;
	}
	return false;
}

// Whether kernel uid `uid` is mapped in the user namespace: the uids a
// namespace a user made can hold are that user's own and its
// subordinate ones, so inside it they are "the same user" (rootless
// containers chown their layers to subordinate uids).
static __always_inline bool uid_mapped(struct user_namespace *ns, __u32 uid)
{
	__u32 count = BPF_CORE_READ(ns, uid_map.nr_extents);
	// The inline extents, or the pointer to the allocated ones, both at
	// the same place after nr_extents (vmlinux_min.h says why by hand).
	void *inline_extents = (void *)ns + bpf_core_field_offset(ns->uid_map) + UID_GID_MAP_EXTENTS_OFFSET;
	const struct uid_gid_extent *extents = inline_extents;
	if (count > UID_GID_MAP_MAX_BASE_EXTENTS &&
	    bpf_probe_read_kernel(&extents, sizeof(extents), inline_extents))
		return false;
	for (int i = 0; i < MAX_EXTENTS; i++) {
		if (i >= count)
			break;
		struct uid_gid_extent extent;
		if (bpf_probe_read_kernel(&extent, sizeof(extent), extents + i))
			return false;
		if (uid - extent.lower_first < extent.count)
			return true;
	}
	return false;
}

// The policy. 0 lets the kernel's decision stand; -EACCES denies.
static __always_inline int world_write(struct inode *inode, int mask)
{
	if (!(mask & MAY_WRITE))
		return 0;
	__u32 mode = BPF_CORE_READ(inode, i_mode);
	// What the walk leaves alone: anything without the other-write
	// bit, sticky files and directories (/tmp), and anything that is
	// not a regular file or a directory (devices, sockets, fifos).
	if (!(mode & S_IWOTH) || (mode & S_ISVTX))
		return 0;
	__u32 format = mode & S_IFMT;
	if (format != S_IFREG && format != S_IFDIR)
		return 0;
	struct task_struct *task = bpf_get_current_task_btf();
	const struct cred *cred = BPF_CORE_READ(task, cred);
	__u32 uid = BPF_CORE_READ(cred, fsuid.val);
	if (uid == 0)
		return 0;
	__u32 owner = BPF_CORE_READ(inode, i_uid.val);
	if (uid == owner)
		return 0;
	// Group write stays, as the walk keeps it.
	if (in_group(cred, BPF_CORE_READ(inode, i_gid.val)))
		return 0;
	struct user_namespace *ns = BPF_CORE_READ(cred, user_ns);
	if (BPF_CORE_READ(ns, level) > 0 && uid_mapped(ns, owner))
		return 0;
	return -EACCES;
}

// Where bpf is an active LSM: the hook itself. `ret` is what an earlier
// program on the hook returned.
SEC("lsm/inode_permission")
int BPF_PROG(lsm_inode_permission, struct inode *inode, int mask, int ret)
{
	if (ret)
		return ret;
	return world_write(inode, mask);
}

// Where it is not: run before security_inode_permission(), whose return
// value a non-zero result replaces. The verifier allows this on any
// function named security_*, LSM list or not.
SEC("fmod_ret/security_inode_permission")
int BPF_PROG(fmod_ret_inode_permission, struct inode *inode, int mask, int ret)
{
	if (ret)
		return ret;
	return world_write(inode, mask);
}
