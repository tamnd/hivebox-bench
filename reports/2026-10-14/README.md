# 2026-10-14: the M0 node report, create-storm, exec and SWE-bench Verified, with a Docker baseline

The first node report for hivebox M0: create-storm and exec against one comb with container cells, the same two suites against Docker on the same machine, and every one of the 500 tasks in SWE-bench Verified run with its gold patch and with no patch. The M0 exit criterion asks for one bare metal node, and this is not one. It is a shared VM with no KVM, so the numbers say what hivebox does on this machine, and the microVM target is not measured at all. Each M0 target is under "M0 targets" with whether it was met here.

## Machine

server3: AMD EPYC Processor (with IBPB), 8 cores, 23 GiB of memory and no swap, kernel 6.8.0-106-generic, a 400 GB virtual disk. It is a VM shared with other people's compile jobs, and the load average is in each table's notes. It has no `/dev/kvm`, and the comb logs `cells run without core scheduling: No such device`.

## Setup

One comb in its own cgroup with real cgroups and container cells. Its config:

```
[node]
data_dir = "/root/hb-tmp/node/data"
socket = "/root/hb-tmp/node/comb.sock"
mem_mib = 48000

[pools]
cgroup_root = "/sys/fs/cgroup/hb-node.slice/cells"
netns_dir = "/root/hb-tmp/node/netns"

[backends.container]
drone = "/root/hb-tmp/node/bin/hive-drone"
state_dir = "/root/hb-tmp/node/oci"

[images]
layers_dir = "/root/hb-tmp/node/layers"

[network]
guard = false
```

With the default overcommit of 1.5 the node admits 72,000 MiB of cells. `guard = false` leaves the network guard off, so cells get a loopback interface and nothing else, and no cell reached the internet. `hive-comb` and `hive-oci` are release builds of hivebox 0.0.45, and `hive-drone` is a static musl build of the same tree. Partway through the SWE-bench run the comb was replaced with a build of 0.0.46 plus hivebox#179, which calls itself 0.0.46, after a pytest run sat frozen for five hours: the pressure brake paused cells that were in the middle of a long command. Every number after that point is from the fixed comb, and the SWE-bench section says which tasks ran on which.

The image for create-storm and exec is `python:3.12-slim` from Docker Hub, pulled and unpacked by `swe/verified.py` into the comb's image store. For the Docker baseline the same unpacked root filesystem was loaded into Docker 29.6.2 with `docker import`, which took 30.4 s and made an image of 43.7 MB, so both sides start the same files. Cells and containers are 64 MiB, and containers get `NetworkMode: none` to match the cells. The Docker side of `hivebox-bench` talks to the daemon's socket directly over HTTP, so no `docker` CLI process is in the timings: a container create is `POST /containers/create` then `POST /containers/{id}/start`, and an exec is the three calls to make, start and read back an exec.

## Results

### create-storm

Creates at a fixed rate for 30 s a step, open loop, with each create timed from the call to the cell being ready to run a command. hivebox first, on a comb that had been up for 120 s and filled its pools, at a load of 24.4 at the start and 102 at the end:

| rate/s | started | failed | achieved/s | p50 ms | p90 ms | p99 ms | max ms | iqr ms | stop ms |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 30 | 0 | 1.0 | 104.6 | 163.9 | 269.4 | 269.4 | 50.0 | 235.6 |
| 2 | 60 | 0 | 2.0 | 103.6 | 174.7 | 277.9 | 277.9 | 27.2 | 497.0 |
| 5 | 150 | 0 | 5.0 | 113.8 | 222.2 | 504.8 | 518.0 | 61.6 | 1072.6 |
| 10 | 300 | 0 | 9.4 | 685.0 | 2209.8 | 2565.5 | 2684.0 | 1751.2 | 2299.7 |
| 25 | 750 | 171 | 7.8 | 21909.1 | 46408.3 | 48324.5 | 48683.3 | 29561.3 | 5797.4 |

Docker, at a load of 43.1 at the start:

| rate/s | started | failed | achieved/s | p50 ms | p90 ms | p99 ms | max ms | iqr ms | stop ms |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 30 | 0 | 1.0 | 1131.4 | 1744.1 | 1848.4 | 1848.4 | 388.4 | 6221.3 |
| 2 | 60 | 0 | 1.9 | 1815.8 | 3181.8 | 5017.2 | 5017.2 | 524.0 | 16628.3 |
| 5 | 150 | 0 | 2.8 | 13814.9 | 26205.0 | 29447.3 | 29523.9 | 16270.4 | 33142.5 |
| 10 | 300 | 0 | 2.2 | 77705.5 | 107008.9 | 114636.1 | 115490.6 | 58042.1 | 82947.3 |

A run stops after the first step with a failure or a p99 over 100 s, so Docker stopped after 10/s on its p99 and hivebox after 25/s on its failures. Up to 5/s a hivebox create has a p50 of 104 to 114 ms, about a tenth of Docker's at 1/s. The node keeps up to about 9.4 creates a second on this VM, and Docker to about 2.8. At 25/s hivebox falls behind, and the 171 that failed were all turned away with CAPACITY_UNAVAILABLE, "the node was too busy making other cells to start this one in time", which is the node shedding load as it should and not an error. Stopping the cells of a step is the stop column, and Docker's is 26 to 36 times as long.

Memory for the cells themselves, from a sampler every 5 s: Docker's `containerd-shim-runc-v2` processes went from 11 (other people's containers, 83 MiB) to 311 at the 10/s step, 3,164 MiB of summed RSS, so about 10 MiB for each container before the container runs anything. hivebox has no process for each cell outside it, and the comb's RSS with 1,000 cells up was 66 to 76 MiB.

### exec

`true` run in a cell over and over for 10 s a step, closed loop, spread over all the cells, with a given number in flight. 100 cells on each side, hivebox at a load of 56.4 and Docker at 76.2:

| in flight | hivebox per second | p50 ms | p99 ms | max ms | Docker per second | p50 ms | p99 ms | max ms |
|---|---|---|---|---|---|---|---|---|
| 1 | 87 | 5.7 | 69.8 | 194.6 | 3 | 303.1 | 664.7 | 664.7 |
| 8 | 362 | 13.4 | 90.9 | 200.3 | 10 | 765.6 | 1121.9 | 1175.8 |
| 32 | 553 | 41.4 | 270.8 | 525.7 | 12 | 2402.2 | 4530.6 | 5002.7 |
| 128 | 617 | 125.9 | 1000.1 | 2202.8 | 13 | 7951.5 | 15290.5 | 15513.1 |

Nothing failed on either side. hivebox ran 29 to 47 times as many execs a second with a p50 53 to 63 times lower. The 100 cells were made in 6.3 s and the 100 containers in 34.6 s, and stopping them took 0.95 s and 23.3 s. An exec through hivebox goes to the drone already running in the cell, which starts `true` itself, where each Docker exec goes through dockerd, containerd, the shim and `runc exec`.

hivebox with 1,000 cells, on 0.0.45 at a load of 90.7:

| in flight | ran | failed | per second | p50 ms | p90 ms | p99 ms | max ms | iqr ms |
|---|---|---|---|---|---|---|---|---|
| 1 | 785 | 0 | 78 | 6.3 | 39.2 | 81.0 | 140.3 | 5.8 |
| 8 | 2662 | 0 | 265 | 16.9 | 69.3 | 160.0 | 335.1 | 34.4 |
| 32 | 4245 | 0 | 421 | 64.1 | 164.2 | 310.1 | 938.4 | 76.5 |
| 128 | 5279 | 0 | 517 | 128.1 | 565.1 | 1679.5 | 3382.2 | 232.9 |

The 1,000 cells were made in 126.9 s and stopped in 7.4 s. With all of them up the cells' cgroup held 835 MiB in all, under 1 MiB a cell.

hivebox with 2,500 cells, on the 0.0.46 comb with the config above changed in two places: `mem_mib = 110000`, so the node admits 165,000 MiB with overcommit, and `create_deadline = "120s"` under `[lifecycle]`. The load was 48.6 at the start.

| in flight | ran | failed | per second | p50 ms | p90 ms | p99 ms | max ms | iqr ms |
|---|---|---|---|---|---|---|---|---|
| 1 | 808 | 0 | 81 | 6.5 | 39.6 | 64.7 | 102.4 | 5.5 |
| 8 | 2768 | 0 | 275 | 17.6 | 67.5 | 106.9 | 186.0 | 34.8 |
| 32 | 4604 | 0 | 458 | 56.3 | 146.9 | 298.8 | 541.7 | 67.4 |
| 128 | 3920 | 0 | 384 | 204.1 | 737.0 | 2500.2 | 4614.4 | 312.5 |

Up to 32 in flight an exec costs about the same with 2,500 cells as with 100 or 1,000. At 128 in flight the p99 is 2.5 s, against 1.0 s with 100 cells. With all 2,500 up the cells' cgroup held 2,020 MiB, the comb's RSS was 138 MiB, and the machine still had 8,939 MiB available. Stopping them took 27.2 s, and the load went over 500 right after.

The 2,500 cells took 537.6 s to make, and the first try with the default 30 s create deadline did not finish. It got to about 2,440 cells, and then a create failed with DRONE_UNREACHABLE, "the guest agent did not answer within the create deadline". The create rate falls as cells pile up. From the cgroup count every 30 s on the run that finished, cells came up at about 8 a second at first, 5.3 a second past 1,000 and 3.2 a second past 2,350. The comb's `hive_create_seconds` metrics say where the time goes. Mean time for each stage over each 40 s window:

| cells up | creates | start ms | pool ms | prepare ms | handshake ms | wal ms |
|---|---|---|---|---|---|---|
| 869 (all creates so far) | 862 | 7864 | 95 | 99 | 65 | 50 |
| 1085 | 215 | 13945 | 242 | 143 | 69 | 77 |
| 1286 | 202 | 14106 | 265 | 172 | 79 | 79 |
| 1473 | 186 | 16245 | 125 | 218 | 94 | 71 |
| 1633 | 163 | 20103 | 157 | 188 | 82 | 67 |
| 1775 | 140 | 19992 | 182 | 211 | 106 | 90 |

Every stage but `start` stays under 300 ms. `start` is the wait for one of the container driver's 8 workers plus the OCI create itself, so it grows with the queue, and the queue grows because each create takes longer the more cells are up. With 2,075 cells up the host had 4,683 mounts, 2,504 of them network namespace binds under `netns_dir` and 2,152 overlay root filesystems. Every OCI create makes a new mount namespace, which copies the whole host mount table under a lock the kernel holds for all of them, so the 8 workers queue on it. `unshare -m true` took 0.21 to 0.29 s at that point, and 0.00 to 0.01 s with no cells up and 724 mounts. That was a hivebox bug in the container driver (tamnd/hivebox#183), and it is why 2,500 cells needed the longer deadline here.

hivebox 0.0.48 fixes it (tamnd/hivebox#184). Each container worker now works in a private mount namespace of its own, and the comb hands it the cell's overlay as a mount that is attached nowhere on the host, so a create copies about 30 mounts however many cells are up. The same exec run on that build, with the default 30 s create deadline and the load at 87.6 at the start, made the 2,500 cells in 170.2 s with no failures, against 537.6 s above. Cells came up at about 17 a second at first, 12.8 a second past 1,000 and 13 a second past 2,000, and the host mount table grew from 484 to 2,834, one network namespace bind a cell. Exec itself did not change, at 62, 218, 324 and 480 a second for 1, 8, 32 and 128 in flight, within the noise of this machine, and stopping the cells took 20.7 s. The raw results are in `exec-2500-fix.zst`.

### create-storm by backend

`--backend` picks the backend for every cell in a run. This machine has no KVM and no QEMU, so the microVM and full VM backends are not measured here, only containers and wasm.

Containers again on hivebox 0.0.48, the same storm as above at a load of 101.7 at the start:

| rate/s | started | failed | achieved/s | p50 ms | p90 ms | p99 ms | max ms | iqr ms | stop ms |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 30 | 0 | 1.0 | 146.4 | 188.0 | 372.3 | 372.3 | 62.4 | 248.3 |
| 2 | 60 | 0 | 2.0 | 90.2 | 126.1 | 516.5 | 516.5 | 65.1 | 723.5 |
| 5 | 150 | 0 | 5.0 | 117.6 | 298.5 | 493.8 | 666.8 | 77.3 | 1500.7 |
| 10 | 300 | 0 | 10.0 | 101.0 | 291.2 | 1018.3 | 1105.2 | 39.6 | 3228.5 |
| 25 | 750 | 0 | 16.0 | 6999.6 | 17486.3 | 18709.4 | 19253.6 | 9405.8 | 7250.5 |
| 50 | 1500 | 600 | 15.8 | 24580.5 | 35353.1 | 36654.1 | 36934.3 | 21133.5 | 17480.0 |

With the fix for tamnd/hivebox#183 the node keeps up at 10/s, with a p50 of 101.0 ms where it was 685.0 ms, and makes about 16 cells a second at 25/s and 50/s with none failed at 25/s, against 7.8 a second and 171 failed before. The 600 that failed at 50/s were turned away with CAPACITY_UNAVAILABLE for being too busy, as before.

Wasm cells run a WASI program in the comb itself, here a 50 byte module that returns at once, with the fncall backend turned on in the comb's config and 64 MiB cells, 10 s a step. On hivebox 0.0.48, at a load of 13.4 at the start:

| rate/s | started | failed | achieved/s | p50 ms | p90 ms | p99 ms | max ms | iqr ms | stop ms |
|---|---|---|---|---|---|---|---|---|---|
| 25 | 250 | 0 | 25.0 | 14.8 | 51.6 | 116.2 | 191.9 | 17.5 | 119.5 |
| 50 | 500 | 0 | 49.1 | 34.0 | 106.3 | 353.3 | 451.6 | 47.9 | 428.0 |
| 100 | 1000 | 0 | 100.0 | 34.5 | 94.2 | 153.4 | 192.3 | 43.4 | 1248.6 |
| 200 | 2000 | 0 | 197.9 | 82.8 | 260.0 | 347.2 | 423.6 | 145.2 | 2413.5 |
| 400 | 4000 | 1422 | 257.7 | 365.3 | 1073.0 | 1487.5 | 1914.0 | 597.5 | 2672.3 |

Most of that was two WAL syncs a create, which cost about 10.7 ms each on this disk for a 4 KiB write. A wasm cell is gone after a restart of the comb whatever its record says, so hivebox 0.0.49 stops waiting for those syncs (tamnd/hivebox#186). The same storm on that comb, at a load of 38.6 at the start:

| rate/s | started | failed | achieved/s | p50 ms | p90 ms | p99 ms | max ms | iqr ms | stop ms |
|---|---|---|---|---|---|---|---|---|---|
| 25 | 250 | 0 | 25.1 | 2.4 | 13.4 | 44.2 | 51.6 | 1.2 | 430.0 |
| 50 | 500 | 0 | 50.0 | 2.9 | 29.0 | 186.8 | 276.2 | 3.3 | 442.9 |
| 100 | 1000 | 0 | 99.6 | 5.1 | 42.7 | 208.0 | 262.4 | 9.0 | 1438.2 |
| 200 | 2000 | 0 | 197.2 | 30.1 | 323.1 | 557.6 | 656.5 | 123.2 | 2830.3 |
| 400 | 4000 | 1422 | 257.7 | 71.9 | 411.7 | 632.1 | 810.3 | 285.2 | 1608.1 |

Both storms ran twice, alternating, and the second pair, at a load of 31.7 and 34.8 at the start, gave a p50 at 25/s of 19.3 ms before and 2.5 ms after, and at 50/s of 48.1 ms and 3.8 ms. At 100/s and up the second pair was closer, at 39.5 and 34.1 ms at 100/s. The 1,422 that failed at 400/s in every run were the node's memory ceiling and not its speed, 165,000 MiB with `mem_mib = 110000`, or 2,578 cells of 64 MiB. The p99 at 25/s is still 44 ms, against the 10 ms the spec asks of a wasm create, and from the comb's create metrics what is left is mostly the handshake with the drone while a burst keeps the comb's runtime busy.

Exec on 100 wasm cells on hivebox 0.0.49, at a load of 41.7:

| in flight | cells | ran | failed | per second | p50 ms | p90 ms | p99 ms | max ms | iqr ms |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 100 | 5914 | 0 | 591 | 0.7 | 2.1 | 34.8 | 67.0 | 0.5 |
| 8 | 100 | 12052 | 0 | 1204 | 3.1 | 12.0 | 54.5 | 115.4 | 3.6 |
| 32 | 100 | 18650 | 0 | 1864 | 10.2 | 46.3 | 68.7 | 170.7 | 11.2 |
| 128 | 100 | 26462 | 0 | 2628 | 46.6 | 81.2 | 110.0 | 189.1 | 43.4 |

A wasm exec is 0.7 ms at the p50 with one in flight, and 2,628 a second at 128, about 4 times the 617 a second container cells did above.

### SWE-bench Verified

All 500 tasks, each run twice in a fresh container cell, once with the gold patch and once with no patch, through the upstream eval script and graded with `swebench` 5.0.2. The images are the SWE-bench ones from Docker Hub, fetched layer by layer into the comb's store by `verified.py`. The run went in 3 shards at once, and then in 6 for the tasks the first 3 had not done, each shard one task at a time.

| | tasks | gold resolved | empty resolved |
|---|---|---|---|
| all | 500 | 486 (97.2%) | 2 |
| with the two timeouts run again | 500 | 487 (97.4%) | 2 |

| repo | tasks | gold resolved |
|---|---|---|
| django/django | 231 | 231 |
| sympy/sympy | 75 | 75 |
| sphinx-doc/sphinx | 44 | 40 |
| matplotlib/matplotlib | 34 | 33 |
| scikit-learn/scikit-learn | 32 | 30, 31 run again |
| astropy/astropy | 22 | 22 |
| pydata/xarray | 22 | 22 |
| pytest-dev/pytest | 19 | 19 |
| pylint-dev/pylint | 10 | 9 |
| psf/requests | 8 | 2 |
| mwaskom/seaborn | 2 | 2 |
| pallets/flask | 1 | 1 |

12 of the 14 gold failures need the internet, which these cells did not have:

- psf/requests 1724, 1766, 1921 and 2317 call httpbin.org and fail with "Temporary failure in name resolution", and 2931 and 5414 have connect timeout tests that expect a route to the outside.
- sphinx 7985, 8269 and 8475 are linkcheck tests, and 10435 is `test_latex_images`, which fetches a remote image.
- matplotlib 20488 fails only `test_https_imread_smoketest`.
- pylint 4661 adds `appdirs` as a dependency, and the eval's `pip install -e .` cannot reach PyPI, so the tests never start.

The other two are scikit-learn 14710 and 25232, which hit the 1,800 s limit with the gold patch. Run again one at a time on the fixed comb at a load of 40 to 60, 25232 resolved with a gold eval of 1,482 s, and 14710 hit the limit again with both patches, the gold run still in `test_gradient_boosting.py` when it was stopped.

The two empty patches that resolve are both cases where the FAIL_TO_PASS tests pass on the unpatched tree. In django 7530 the one FAIL_TO_PASS test, `test_squashmigrations_initial_attribute`, passes without the fix, and the fix shows in `test_makemigrations_consistency_checks_respect_routers`, which errors with no patch and passes with gold. In django 10097 the 438 FAIL_TO_PASS tests pass without the fix in this image, and both runs end with failures in other tests, 51 with no patch and 45 with gold. Neither is something the cell did.

Times for the 500 tasks, in seconds:

| step | n | p50 | p90 | p99 | max |
|---|---|---|---|---|---|
| fetch the image | 500 | 129.3 | 254.3 | 465.9 | 771.3 |
| cell create | 1000 | 0.221 | 0.457 | 1.149 | 5.029 |
| apply the patch | 500 | 0.201 | 0.421 | 0.719 | 1.299 |
| gold eval | 500 | 34.2 | 115.9 | 813.1 | 1800.3 |
| the whole task | 500 | 230.2 | 539.5 | 1851.8 | 3959.6 |

The images are 1.157 GB at the p50 and 3.168 GB at the most, 659.3 GB in all, fetched in 77,369 s. A cell create here is a cell from a 1 to 3 GB image, and its p50 is 221 ms. 129 tasks ran on the 0.0.45 comb, with a create p50 of 195 ms and every gold patch resolved, and 371 on the fixed comb, with a create p50 of 235 ms and 357 resolved. Six creates were turned away for room while the pressure brake was on and waited 196 s in all before they got one. Over the run on the fixed comb, from 10 s samples: at most 5 cells and 1,254 MiB in the cells' cgroup, a comb RSS of at most 47 MiB, and a load of 36.9 on average and 133.1 at the most.

## M0 targets

| target | measured here | met |
|---|---|---|
| container create p50 of 300 ms or less | 104.6 ms at 1/s, 113.8 ms at 5/s, 685.0 ms at 10/s, and 221 ms over the 1,000 SWE-bench creates | yes up to 5 creates a second, and since hivebox 0.0.48 up to 10, at 101.0 ms |
| microVM restore p50 of 250 ms or less | no KVM on this machine | not measured |
| 1,000 cells at once | 1,000 cells up and running commands, none failed, and 2,500 with the default create deadline since hivebox 0.0.48 | yes |
| SWE-bench Verified gold passes on 99% or more | 487 of 500, 97.4% | no |

The SWE-bench target is not met as run. The 12 failures that need the internet would need a node with the network guard on and an egress allow list for PyPI, httpbin.org and the sites the sphinx and matplotlib tests fetch. Without those 12, 487 of 488 pass, 99.8%. And none of it is on bare metal, so this report does not say M0's exit criterion passes.

## How to run it again

The comb as above, and `HIVEBOX_ENDPOINT=unix:/root/hb-tmp/node/comb.sock`. `hivebox-bench` is a release build of this repository.

```
hivebox-bench run create-storm --image python --cell-mib 64 --rates 1,2,5,10,25,50 --seconds 30 --p99-ms 100000 --raw storm.zst
hivebox-bench run exec --image python --cells 100 --cell-mib 64 --raw exec-100.zst
hivebox-bench run exec --image python --cells 1000 --cell-mib 64 --raw exec.zst
hivebox-bench run exec --image python --cells 2500 --cell-mib 64 --raw exec-2500.zst
hivebox-bench run create-storm --image python --cell-mib 64 --rates 1,2,5,10,25,50 --seconds 30 --p99-ms 100000 --raw storm-container-0048.zst
hivebox-bench run create-storm --backend fncall --image noop --cell-mib 64 --rates 25,50,100,200,400 --seconds 10 --p99-ms 100000 --raw storm-wasm-0049.zst
hivebox-bench run exec --backend fncall --image noop --cell-mib 64 --cells 100 --raw exec-wasm-0049.zst
hivebox-bench run create-storm --docker /var/run/docker.sock --image hb-bench-python:0 --cell-mib 64 --rates 1,2,5,10,25 --seconds 30 --p99-ms 100000 --raw docker-storm.zst
hivebox-bench run exec --docker /var/run/docker.sock --image hb-bench-python:0 --cells 100 --cell-mib 64 --raw docker-exec.zst
```

The 2,500 cell run needs the comb with `mem_mib = 110000`, and before hivebox 0.0.48 also `create_deadline = "120s"`, as above. The wasm runs need `[backends.fncall]` with `enabled = true` in the comb's config, and the module in `data/wasm/noop.wasm`, with a copy as `true.wasm` for exec, since the first word of a command picks the program. `hb-bench-python:0` is the comb's unpacked python image, `tar -C data/images/python -c . | docker import - hb-bench-python:0`. SWE-bench Verified, one of 6 shards and then one task again, with `HIVE_SOCKET` set to the comb's socket and the venv from `swe/requirements.txt`:

```
python swe/verified.py --dataset verified.parquet --images data/images --hive-oci hive-oci --out swe-0.jsonl --logs logs --shard 0/6
python swe/verified.py --dataset verified.parquet --images data/images --hive-oci hive-oci --out swe-rerun.jsonl --logs logs --only scikit-learn__scikit-learn-14710
```

## Raw results

`storm.zst`, `docker-storm.zst`, `exec.zst`, `exec-100.zst`, `docker-exec.zst`, `exec-2500.zst`, `exec-2500-fix.zst`, `storm-container-0048.zst`, `storm-wasm-0048.zst`, `storm-wasm-0049.zst` and `exec-wasm-0049.zst` have a JSON line for each create or exec. `hivebox-bench report storm.zst` prints a file's table again from its lines, and every create-storm and exec table above comes back the same from these files, except that calls a second is worked out to the last answer and not to the end of the step, so it can differ in the first decimal. `storm-container-0048.zst` also gives 90.1 and 65.2 ms for the p50 and iqr at 2/s, against 90.2 and 65.1 ms as printed during the run. `swe.zst` has a JSON line for each of the 500 tasks from the main run, and `swe-rerun.zst` the two scikit-learn tasks run again.
