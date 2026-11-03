# Block-device, NFS and Lustre monitoring

`--collect io` now collects both application POSIX calls and sampled storage-layer
activity. Both appear on the same timeline, using separate expandable graphs.

```sh
make build PROFILE=release
target/release/rprof record --collect io --storage-interval-ms 1000 \
  -o storage.jsonl -- ./application
target/release/rprof view storage.jsonl --open
```

Use `--collect storage` for monitoring without a preload library, or
`--collect io --no-storage` for POSIX calls alone. Sampling defaults to one second
and accepts 100 ms–60 s. These measurements require a new capture; an earlier POSIX
capture cannot reconstruct device or filesystem-client activity.

## Timeline

POSIX I/O remains visible alongside the discovered storage backends:

- **Block-device I/O total** → filesystem/shared-device group → host device.
- **Lustre bulk RPC I/O total** → filesystem → host client.
- **NFS server payload I/O total** → filesystem/export → host client.

Each backend has its own total. Select throughput, IOPS or latency in the shared
metric control. Click a storage window to see its measured interval, contributing
resources, exact byte/operation deltas and optional diagnostic metrics. Aggregates
across resources are sampled estimates; incomplete contributors are labeled.
Storage and POSIX totals are not added together.

Rates are derived from consecutive counter snapshots, not from file-call spans.
The first snapshot establishes a baseline. Counter resets, remounts, topology
changes and missing reads produce gaps/rebaselines, not negative or enormous rates.
Zero operations yield no mean-latency observation, rather than a false zero latency.
Indexed viewing supplies multi-resolution data; standalone reports retain bounded
full-run storage samples independently of raw-event retention.

## Measurement layers

| Backend | Source | Meaning |
| --- | --- | --- |
| Local block devices | `/proc/diskstats`, sysfs device topology | Completed block requests and sectors at the host block-device layer; 512 bytes/sector regardless of device sector size |
| NFS | `/proc/self/mountstats` | NFS client **server payload bytes**, read/write-family RPC counts, execute time, queue/RTT totals, retransmissions, timeouts and optional errors |
| Lustre | OSC/MDC `stats` under `/proc/fs/lustre`, `/sys/fs/lustre` or `/sys/kernel/debug/lustre` | Client OSC bulk-transferred bytes, OST read/write RPC counts/times, reported RPC wait data, and MDC RPC/wait totals where supplied |

These are host/client measurements, including other workloads and possibly the
profiler's own capture writes. Local block counters are below the page cache, but
are not physical-media measurements; controller caches, virtualization and device
firmware can still affect actual media activity. NFS and Lustre client counters
are **not remote server disk counters**. Server-side monitoring would require
collecting on the servers or using their telemetry interfaces.

### Local device/filesystem mapping

The sampler selects whole leaf block devices and follows partitions and device
mapper/RAID backing relationships when mapping mounts. It avoids adding a partition,
its whole disk and a stacked mapper device into the same total. Loop/RAM/zram and
similar virtual block layers are excluded. A filesystem spanning multiple backing
devices can group those devices. If a physical device backs several filesystems,
it is explicitly labeled **shared device**; bytes are not arbitrarily divided
between its filesystems. Hidden container topology can leave devices **unmapped**.

Mean request latency is delta cumulative read/write milliseconds divided by delta
completed requests. In-flight requests, busy time and weighted I/O time are retained
as additional attributes. Busy time is not a claim that an NVMe device is saturated.

### NFS

The byte counters use `SERVERREADBYTES` and `SERVERWRITTENBYTES`, not the `NORMAL`
application byte counters. READ/READ_PLUS and WRITE/WRITE_SAME families are combined.
Latency uses RPC execute time; queue time and RTT remain separately visible.
Retransmissions are transmissions minus operations. Error fields absent in older
formats remain unavailable. Bind mounts sharing a superblock/export are deduplicated.

### Lustre

OSC `read_bytes`/`write_bytes` **SUM** fields count bulk bytes transferred. They are
different from llite's application-facing byte counters. The sampler aggregates
OSCs by filesystem at collection time, keeping volume bounded instead of emitting
six series per OST. The target count is retained. Individual target resets are
checked before summing, so one OST resetting cannot be hidden by another growing.

OST read/write RPC latency uses the reported opcode microsecond totals. MDC request
counts and wait totals use `req_waittime`; opcode and reint breakdown counters are
not summed together. Standard lprocfs output omits zero-count rows; known absent
rows in a valid standard stats file are treated as zero. Malformed rows/units and
failed reads are diagnosed. OSC bulk monitoring does not measure remote physical
disks or all possible data-on-MDT paths.

**Permissions matter:** on tst3, OSC/MDC directories exist in procfs, but their
`stats` files are in root-restricted debugfs. Ordinary-user collection reports
Lustre unavailable there. Provide read access through your system's normal admin
configuration or run an appropriately privileged sampler. rprof never invokes
sudo or changes kernel/debugfs permissions itself.

## Scope, overhead and deployment

One concurrent sampler is elected per host/session/mount-namespace using the same
local file-lock approach as system metrics. Retain all rank captures; the elected
rank need not be rank zero. Namespaces/private temporary directories can produce
duplicate host resources; the viewer selects one source rather than summing copies.
Cross-host totals require a common calibrated clock domain.

Reads are bounded to 2 MiB per source, 8,192 diskstats rows, 4,096 resources/targets
and bounded topology traversal. Storage emits up to six primary counter samples per
resource per interval, plus health metadata. Put capture output on a different
device or suitable memory-backed storage when practical, to reduce capture-write
feedback in device counters. Monitoring stops with the supervised workload; delayed
writeback after exit needs a longer observation wrapper, for example a brief sleep
after the application. These are sampled snapshots, not atomic system-wide reads.

## Darshan reference and validation

The design follows Darshan's useful distinction between application I/O behavior,
filesystem context and detailed traces. Darshan's Lustre module records layout and
striping information; it is not a replacement for client/device counters. Per-file
stripe/layout collection is a separate potential addition.

- [Darshan overview](https://wordpress.cels.anl.gov/darshan/)
- [Darshan Lustre instrumentation](https://github.com/darshan-hpc/darshan/blob/main/darshan-runtime/lib/darshan-lustre.c)
- [Linux disk statistics](https://docs.kernel.org/admin-guide/iostats.html)
- [Linux NFS counter definitions](https://github.com/torvalds/linux/blob/master/include/linux/nfs_iostat.h)
- [Lustre client counter definitions](https://github.com/lustre/lustre-release/blob/master/lustre/ptlrpc/lproc_ptlrpc.c)

Validation on tst3/HBv4 allocation 21 (`hbv404EX3U`) used an elevated sampler and an
unprivileged direct-I/O workload against local ext4, NFS and Lustre. Each filesystem
received 1 MiB of writes and reads. NFS and Lustre both reported exactly 1 MiB in each
direction; block counters included additional host/filesystem activity. The capture
contained 122/122 POSIX records, zero drops and zero storage source errors. Four leaf
block devices (including two backing the same XFS mapper filesystem), one NFS export
and one Lustre filesystem with 16 OSC targets were identified. The allocation was
released. An ordinary-user run separately verified that inaccessible Lustre stats
are reported without changing workload exit status.
