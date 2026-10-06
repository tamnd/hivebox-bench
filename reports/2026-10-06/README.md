# 2026-10-06: snapshot and memory on one machine

The first runs of the snapshot suite and the memory suite. The snapshot suite times pause and resume and disk snapshots of cells that wrote 10, 100 and 1000 MiB, and restores each snapshot into a new cell and checks its data. The memory suite runs 32 agent cells for 10 minutes and reads how much memory the comb's cgroup holds, with idle trimming off and on.

## Machine

server3: AMD EPYC Processor (with IBPB), 8 cores, 23 GiB of memory and no swap, kernel 6.8.0-106-generic. It is a VM shared with other people's compile jobs, and the load average was between 50 and 90 during the runs. It has no KVM, so only container cells are in these numbers. The disk is a shared one where a file and directory fsync costs about 29 ms.

## Setup

One comb at a time with real cgroups and container cells, all from the python image, on a store that already had the image. For the snapshot runs the comb had `[lifecycle] create_deadline = "600s"`, because with the default of 30 s a restore of 1000 MiB on 0.0.27 failed with `DRONE_UNREACHABLE`.

The snapshot run on hivebox 0.0.28 was:

```
hivebox-bench run snapshot --image python --raw snapshot-0.0.28.zst
```

with the defaults: 20 idle `true` runs, 50 pause and resume rounds with a `true` after each resume, and 3 snapshots each of cells that wrote 10, 100 and 1000 MiB of random data. While each snapshot is taken, a second task keeps running `true` in the same cell, and the slowest of those is how long a user of the cell waited. Each snapshot is then restored into a new cell, and the md5 of the data there has to match what was written.

The memory runs used the 0.0.27 comb with `[density] pressure_idle = "0s"`, once with `trim_idle = "0s"` and once with `trim_idle = "30s"`, twice each, one after the other:

```
hivebox-bench run memory --image python --cgroup /sys/fs/cgroup/hb-memb.slice --raw mem-1-0s.zst
```

with the defaults: 32 agent cells of 512 MiB, each running a step and then waiting 60 s, for 600 s after a 60 s warm up. A step imports a handful of standard library modules, reads every `.py` file of the standard library, and reads back a 32 MiB scratch file it wrote on its first step. The agents start spread over one wait so their steps do not all land at once. The suite reads `memory.current` and `memory.stat` of the comb's cgroup root every second.

## Results

### Snapshots on 0.0.28

| call | count | p50 ms | p90 ms | p99 ms | max ms |
|---|---|---|---|---|---|
| `true`, idle | 20 | 4.2 | 17.1 | 31.2 | 31.2 |
| pause | 50 | 21.0 | 174.3 | 398.2 | 398.2 |
| resume | 50 | 7.8 | 86.0 | 167.7 | 167.7 |
| `true` after resume | 50 | 4.7 | 15.6 | 39.5 | 39.5 |
| snapshot, 10 MiB | 3 | 1255.3 | 1262.6 | 1262.6 | 1262.6 |
| slowest `true` during it, 10 MiB | 3 | 93.9 | 96.6 | 96.6 | 96.6 |
| restore, 10 MiB | 3 | 391.3 | 467.2 | 467.2 | 467.2 |
| first `true` after restore, 10 MiB | 3 | 3.8 | 6.0 | 6.0 | 6.0 |
| snapshot, 100 MiB | 3 | 8545.1 | 11219.5 | 11219.5 | 11219.5 |
| slowest `true` during it, 100 MiB | 3 | 590.4 | 701.3 | 701.3 | 701.3 |
| restore, 100 MiB | 3 | 2727.1 | 3654.5 | 3654.5 | 3654.5 |
| first `true` after restore, 100 MiB | 3 | 14.7 | 14.8 | 14.8 | 14.8 |
| snapshot, 1000 MiB | 3 | 77434.8 | 137773.8 | 137773.8 | 137773.8 |
| slowest `true` during it, 1000 MiB | 3 | 7725.5 | 20981.8 | 20981.8 | 20981.8 |
| restore, 1000 MiB | 3 | 24491.0 | 29670.5 | 29670.5 | 29670.5 |
| first `true` after restore, 1000 MiB | 3 | 34.2 | 43.4 | 43.4 | 43.4 |

| MiB | snapshots | `true` during each | restores checked | mismatched |
|---|---|---|---|---|
| 10 | 3 | 170, 143, 124 | 3 | 0 |
| 100 | 3 | 1000, 832, 1096 | 3 | 0 |
| 1000 | 3 | 6575, 7124, 12977 | 3 | 0 |

Pause and resume are quick and the cell answers right after: the p50 of `true` after a resume is 4.7 ms against 4.2 ms idle. Every one of the 9 restores matched what was written.

A snapshot takes about 1.3 s at 10 MiB, 8.5 s at 100 MiB and 66 to 138 s at 1000 MiB, so it grows about in line with the data. The cell is frozen only while its changes are read out, and the slowest `true` during a snapshot shows that: under 0.1 s at 10 MiB, about 0.6 s at 100 MiB, and 6.9 to 21 s at 1000 MiB. The rest of a snapshot happens while the cell runs on. Of the 312 s the comb spent on all 9 snapshots, 37 s were the frozen read and 275 s the build of the new layer.

A restored cell answers its first `true` in 4 to 43 ms. Most of a restore is fetching the new layer into the layer cache.

### Snapshots on 0.0.27 against 0.0.28

0.0.28 has hivebox#115, which stores the chunks of a new layer together instead of with a file and a directory fsync each. On 0.0.27 a 1000 MiB layer of about 14000 chunks took about 28000 fsyncs. The 0.0.27 numbers come from three runs earlier the same day, with the same setup at a similar load, and with 2 or 3 snapshots per size:

| | 0.0.27 | 0.0.28 |
|---|---|---|
| snapshot, 10 MiB | 1.35 to 2.1 s | 1.25 to 1.26 s |
| snapshot, 100 MiB | 7.8 to 11.4 s | 7.6 to 11.2 s |
| snapshot, 1000 MiB | 159.4, 160.5, 188.5, 230.8, 274.1 s | 66.2, 77.4, 137.8 s |
| restore, 1000 MiB | 23.6, 26.6, 42.1, 51.7, 52.5 s | 22.7, 24.5, 29.7 s |

The 1000 MiB snapshot is about twice as fast. At 100 MiB the difference is inside the noise of this machine.

### Memory on 0.0.27

| trim_idle | round | readings | peak MiB | mean MiB | mean file MiB | mean anon MiB | steps | failed | step p50 ms | step p99 ms |
|---|---|---|---|---|---|---|---|---|---|---|
| 0s | 1 | 601 | 1325 | 1287 | 1162 | 16 | 316 | 0 | 657.1 | 1645.7 |
| 30s | 1 | 601 | 1261 | 993 | 895 | 19 | 315 | 0 | 1033.8 | 4409.1 |
| 0s | 2 | 601 | 1339 | 1283 | 1153 | 16 | 315 | 0 | 649.3 | 2665.0 |
| 30s | 2 | 601 | 1147 | 969 | 878 | 18 | 312 | 0 | 963.0 | 2770.5 |

With trimming on, the mean memory of the 32 agents went from 1287 and 1283 MiB to 993 and 969 MiB, 23% and 24% less. Nearly all of it is page cache. The python processes only live for a step, so anon memory stays under 20 MiB. The peak dropped less, 5% and 14%, because a step that lands right after a trim reads everything back in.

That reading back has a cost. The p50 step went from 657 and 649 ms to 1034 and 963 ms, about 50% slower, because each agent waits 60 s, gets trimmed after 30 s, and then reads its 32 MiB scratch file and the standard library from disk again. On this machine's disk that is slow. An agent that reads the same files on every step pays for trimming on every step, and one that touches different files each time would not.

## What this does not show

- Forking a running cell, and snapshots of microVM cells. Both need KVM, which server3 does not have.
- The big part of a 1000 MiB snapshot on 0.0.28. That is mkfs.erofs at about 40 s, which on its own makes about 72000 writes and 68000 reads of about 15 KiB each for a 1000 MiB tar.
- pmem-DAX, FPR and DAMON, the parts of the memory target that DSec reports 40.2% and 21.2% for. They need a microVM backend and a kernel with DAMON, and server3 has neither. These runs only measure idle trimming of container cells.
- A quieter machine. The load from other tenants moved by tens of percent during the runs, and it is why 3 snapshots of the same size can differ by 2x.

## Raw results

`snapshot-0.0.28.zst` has every timed call of the 0.0.28 snapshot run, and `mem-ROUND-TRIM.zst` has every agent step of each memory run, as zstd JSON lines in the format `src/raw.rs` describes. The memory readings over time are only in the tables above. The 0.0.27 snapshot runs were not kept as raw results.
