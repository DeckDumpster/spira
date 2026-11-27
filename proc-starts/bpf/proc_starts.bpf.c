// Counts process starts inside the kernel; user space drains and deletes. Only the kernel
// fields read are declared, relocated against the running kernel's BTF (CO-RE).
typedef unsigned char __u8;
typedef unsigned int __u32;
typedef unsigned long long __u64;
typedef int __s32;

#define SEC(name) __attribute__((section(name), used))
#define __always_inline inline __attribute__((always_inline))
#define __uint(name, val) int (*name)[val]
#define __type(name, val) __typeof__(val) *name
#define CORE __attribute__((preserve_access_index))

#define MAP_HASH 1
#define MAP_ARRAY 2
#define MAP_PERCPU_ARRAY 6
#define MAP_LRU_HASH 9
#define NOEXIST 1

#define PENDING_FULL 0
#define COUNTS_FULL 1

#define PROG_FORK 0
#define PROG_EXEC 1
#define PROG_EXIT 2
#define PROG_MKDIR 3

#define PATH_MAX_BYTES 256

struct task_struct { int pid; int tgid; } CORE;
struct kernfs_node { __u64 id; } CORE;
struct cgroup { struct kernfs_node *kn; } CORE;

struct start_key { __u64 cgroup; char program[16]; char starter[16]; };
struct pending_val { __u64 cgroup; char starter[16]; };
struct cost_val { __u64 ns; __u64 runs; };
struct path_val { char path[PATH_MAX_BYTES]; };

struct { __uint(type, MAP_HASH); __uint(max_entries, 65536); __type(key, __u32); __type(value, struct pending_val); } pending SEC(".maps");
struct { __uint(type, MAP_HASH); __uint(max_entries, 16384); __type(key, struct start_key); __type(value, __u64); } starts SEC(".maps");
struct { __uint(type, MAP_HASH); __uint(max_entries, 16384); __type(key, __u64); __type(value, __u64); } threads SEC(".maps");
struct { __uint(type, MAP_HASH); __uint(max_entries, 16384); __type(key, __u64); __type(value, __u64); } execs SEC(".maps");
struct { __uint(type, MAP_LRU_HASH); __uint(max_entries, 4096); __type(key, __u64); __type(value, struct path_val); } names SEC(".maps");
struct { __uint(type, MAP_ARRAY); __uint(max_entries, 2); __type(key, __u32); __type(value, __u64); } dropped SEC(".maps");
struct { __uint(type, MAP_PERCPU_ARRAY); __uint(max_entries, 4); __type(key, __u32); __type(value, struct cost_val); } cost SEC(".maps");

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(void *map, const void *key, const void *value, __u64 flags) = (void *)2;
static long (*bpf_map_delete_elem)(void *map, const void *key) = (void *)3;
static __u64 (*bpf_ktime_get_ns)(void) = (void *)5;
static long (*bpf_get_current_comm)(void *buf, __u32 size) = (void *)16;
static __u64 (*bpf_get_current_cgroup_id)(void) = (void *)80;
static long (*bpf_probe_read_kernel_str)(void *dst, __u32 size, const void *unsafe_ptr) = (void *)115;

char _license[] SEC("license") = "GPL";

static __always_inline void add_one(__u64 *v) { __sync_fetch_and_add(v, 1); }

static __always_inline void drop(__u32 reason)
{
	__u64 *v = bpf_map_lookup_elem(&dropped, &reason);
	if (v)
		add_one(v);
}

static __always_inline void bump(void *map, const void *key)
{
	__u64 *v = bpf_map_lookup_elem(map, key);
	if (v) {
		add_one(v);
		return;
	}
	__u64 one = 1;
	if (bpf_map_update_elem(map, key, &one, NOEXIST) == 0)
		return;
	v = bpf_map_lookup_elem(map, key);
	if (v)
		add_one(v);
	else
		drop(COUNTS_FULL);
}

static __always_inline void charge(__u32 prog, __u64 started)
{
	struct cost_val *c = bpf_map_lookup_elem(&cost, &prog);
	if (!c)
		return;
	__sync_fetch_and_add(&c->ns, bpf_ktime_get_ns() - started);
	__sync_fetch_and_add(&c->runs, 1);
}

static __always_inline void count_start(__u32 pid, const char *program)
{
	struct pending_val *p = bpf_map_lookup_elem(&pending, &pid);
	if (!p)
		return;
	struct start_key k = {};
	k.cgroup = p->cgroup;
	__builtin_memcpy(k.starter, p->starter, 16);
	__builtin_memcpy(k.program, program, 16);
	bump(&starts, &k);
	bpf_map_delete_elem(&pending, &pid);
}

SEC("tp_btf/sched_process_fork")
int on_fork(__u64 *ctx)
{
	__u64 t0 = bpf_ktime_get_ns();
	struct task_struct *child = (struct task_struct *)ctx[1];
	__u64 cgroup = bpf_get_current_cgroup_id();
	if (child->pid != child->tgid) {
		bump(&threads, &cgroup);
	} else {
		__u32 pid = child->pid;
		struct pending_val p = {};
		p.cgroup = cgroup;
		bpf_get_current_comm(p.starter, 16);
		if (bpf_map_update_elem(&pending, &pid, &p, 0) != 0)
			drop(PENDING_FULL);
	}
	charge(PROG_FORK, t0);
	return 0;
}

SEC("tp_btf/sched_process_exec")
int on_exec(__u64 *ctx)
{
	__u64 t0 = bpf_ktime_get_ns();
	struct task_struct *p = (struct task_struct *)ctx[0];
	__u64 cgroup = bpf_get_current_cgroup_id();
	bump(&execs, &cgroup);
	char comm[16] = {};
	bpf_get_current_comm(comm, 16);
	count_start(p->pid, comm);
	charge(PROG_EXEC, t0);
	return 0;
}

SEC("tp_btf/sched_process_exit")
int on_exit(__u64 *ctx)
{
	__u64 t0 = bpf_ktime_get_ns();
	struct task_struct *p = (struct task_struct *)ctx[0];
	if (p->pid == p->tgid) {
		__u32 pid = p->pid;
		struct pending_val *pv = bpf_map_lookup_elem(&pending, &pid);
		if (pv) {
			char starter[16];
			__builtin_memcpy(starter, pv->starter, 16);
			count_start(pid, starter);
		}
	}
	charge(PROG_EXIT, t0);
	return 0;
}

SEC("tp_btf/cgroup_mkdir")
int on_mkdir(__u64 *ctx)
{
	__u64 t0 = bpf_ktime_get_ns();
	struct cgroup *cgrp = (struct cgroup *)ctx[0];
	const char *path = (const char *)ctx[1];
	__u64 id = cgrp->kn->id;
	struct path_val v = {};
	bpf_probe_read_kernel_str(v.path, PATH_MAX_BYTES, path);
	bpf_map_update_elem(&names, &id, &v, 0);
	charge(PROG_MKDIR, t0);
	return 0;
}
