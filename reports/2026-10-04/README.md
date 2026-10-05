# 2026-10-04: first cluster runs on one machine

The first numbers from the cluster suites: image-import, cold-image, replay and node-loss. They come from one shared VM, not the 16 node cluster the M1 exit criterion asks for, so read them as a check that the suites measure what they say and as a baseline to beat, not as the M1 report.

## Machine

server3: AMD EPYC Processor (with IBPB), 8 cores, 23 GiB of memory and no swap, kernel 6.8.0-106-generic, a 400 GB QEMU virtual disk. It is a VM shared with other people's compile jobs, and the load average was between 30 and 70 during every run here, so every latency below includes waiting for a core. One node for image-import and cold-image, and three combs on the same VM for replay and node-loss.

## image-import

`hivebox-bench run image-import` pulls each image, imports it into the content addressed layer store and reports what the store grew by. 14 images from SWE-Gym, R2E-Gym and SWE-smith, imported one after another into an empty store with hivebox 0.0.16. Load 49.78 at the start.

| image | pulled MiB | new layers MiB | store grew MiB | pulled so far | layers so far | stored so far | import s |
|---|---|---|---|---|---|---|---|
| swegym-sympy-24213 | 1081.9 | 1081.9 | 2377.3 | 1081.9 | 1081.9 | 2377.3 | 483.8 |
| swegym-sympy-24909 | 1086.6 | 456.5 | 1037.9 | 2168.5 | 1538.4 | 3415.2 | 378.1 |
| swegym-sympy-24102 | 1081.9 | 196.5 | 232.3 | 3250.4 | 1734.9 | 3647.5 | 97.0 |
| swegym-sympy-24152 | 1081.3 | 196.0 | 231.8 | 4331.7 | 1930.9 | 3879.3 | 92.5 |
| swegym-pylint-5859 | 984.7 | 354.6 | 979.9 | 5316.4 | 2285.5 | 4859.2 | 371.8 |
| swegym-sphinx-8627 | 1038.3 | 408.2 | 1027.7 | 6354.7 | 2693.6 | 5887.0 | 173.2 |
| r2e-aiohttp-ffb66cb0 | 418.3 | 418.3 | 1041.1 | 6773.0 | 3112.0 | 6928.0 | 133.5 |
| r2e-aiohttp-fecb85a9 | 474.7 | 445.6 | 933.2 | 7247.7 | 3557.6 | 7861.3 | 182.3 |
| r2e-aiohttp-fe6325e7 | 418.1 | 389.1 | 731.2 | 7665.8 | 3946.7 | 8592.5 | 169.5 |
| r2e-aiohttp-fde031fe | 474.0 | 445.0 | 926.7 | 8139.8 | 4391.7 | 9519.2 | 232.9 |
| swesmith-joke2k_1776_faker | 1309.2 | 1309.2 | 3103.4 | 9449.0 | 5700.8 | 12622.6 | 395.7 |
| swesmith-tornadoweb_1776_tornado | 1334.3 | 436.9 | 1226.8 | 10783.3 | 6137.7 | 13849.5 | 267.9 |
| swesmith-arrow-py_1776_arrow | 1330.5 | 433.1 | 1208.1 | 12113.8 | 6570.8 | 15057.6 | 283.6 |
| swesmith-davidhalter_1776_parso | 1262.2 | 364.8 | 1068.5 | 13376.0 | 6935.6 | 16126.1 | 250.8 |

The 14 images are 13376.0 MiB pulled one by one and 6935.6 MiB as distinct compressed layers. The store holds 16126.1 MiB, which is more than the pulls, because it keeps each layer as an uncompressed EROFS image so a cell can mount it without unpacking. That is a loss against a node that keeps the compressed layers, by 9190.5 MiB here, and it is the number to bring down. Images from the same project share most of their bytes: the second to fourth sympy images added 196 to 457 MiB of new layers each against 1082 MiB pulled.

## cold-image

`hivebox-bench run cold-image` starts a cell from an image with nothing of it on the node and runs one command, five times per mode, each time with an empty cache and work directory so nothing of the image is on the node. The image is swegym-sympy-24213, 13 layers and 2370.1 MiB of data, and the command is `python -c "import sympy; print(sympy.__version__)"` in its testbed env. Load 40.03 at the start.

- whole: every layer is fetched from the store before the image is mounted.
- lazy: layers are mounted over nbd and their blocks are fetched when read.
- lazy-traced: lazy, with a prefetch trace of the same command recorded beforehand.

| mode | runs | failed | mount p50 ms | mount p99 ms | command p50 ms | command p99 ms | total p50 ms | total p99 ms | total max ms |
|---|---|---|---|---|---|---|---|---|---|
| whole | 5 | 0 | 103310.0 | 128570.0 | 46890.0 | 60710.0 | 150200.0 | 182310.0 | 182310.0 |
| lazy | 5 | 0 | 2620.0 | 4580.0 | 50710.0 | 76250.0 | 53030.0 | 80840.0 | 80840.0 |
| lazy-traced | 5 | 0 | 3090.0 | 4170.0 | 50630.0 | 80310.0 | 54790.0 | 83050.0 | 83050.0 |

Lazy mounts cut the time to a running cell from 103.3 s to 2.6 s at p50 and the time to the command's end from 150.2 s to 53.0 s. The prefetch trace did not help here: lazy-traced was 1.8 s slower at p50 in total, inside the noise of a machine at load 40 to 53. With five runs a mode the p99 is the worst run. In whole mode every byte is on the node before the command starts and the command still took 46.9 s at p50, so most of the command time is importing sympy on a loaded VM, and lazy reads added about 4 s to it.

## replay

`hivebox-bench run replay` creates cells through the gate at a fixed rate for 60 s a step, each living 10 to 60 s with up to 30 s idle, and waits for the last cell of a step to stop before the next. A keeper, a scout, a gate and three combs ran on server3 with hivebox 0.0.17, the image was swe-requests on the container backend and every cell asked for the default 2048 MiB. Each comb admits 16 such cells, its memory less the 2 GiB reserve times the standard class overcommit of 1.5, so 48 alive is the most the three combs take. The raw samples are in `replay.zst`.

| rate/s | started | failed | infra | peak alive | p50 ms | p99 ms | max ms | keeper writes/s |
|---|---|---|---|---|---|---|---|---|
| 0 | 0 | 0 | 0 | 0 | - | - | - | 0.87 |
| 2 | 93 | 0 | 0 | 40 | 297.3 | 505.3 | 505.3 | 0.93 |
| 4 | 259 | 92 | 92 | 48 | 249.0 | 588.2 | 620.5 | 0.92 |
| 8 | 489 | 278 | 278 | 48 | 278.9 | 475.7 | 493.5 | 0.92 |
| 16 | 1005 | 781 | 781 | 48 | 201.7 | 811.0 | 811.9 | 0.92 |

At 2 creates a second every create made a cell, with a p99 of 505.3 ms. From 4 a second on the three combs were full and every failure was `CAPACITY_UNAVAILABLE: no node has room for the cell`, which is the scheduler saying no when there is no room, not a create that broke. The bench counts those as infra failures, since a cluster sized for the load would have had the room. The latency of the creates that went in stayed under 812 ms at every rate. The keeper took under one write a second at every rate, against 0.87 idle, so the create path does not write to the keeper per cell, and the write rate stays flat with load as spec 13 asks. This says nothing about 1,000 creates a second: that needs the 16 node cluster, and one 8 core VM holds 48 cells of this size.

## node-loss

`hivebox-bench run node-loss` holds 10 cells, then creates one a second for 20 s with idempotency keys and up to 4 retries, and kills a comb with `kill -9` 10 s into the burst. Same three combs as replay, node 2 killed, load 37.57 at the start. The raw samples are in `loss.zst`.

- Node 2 was killed 10001.6 ms into the burst. All 20 creates in the burst made a cell, none of them needed a retry, none was refused and none failed.
- Burst create latency with retries included: n=20, p50 175.6 ms, p90 280.2 ms, p99 311.2 ms, max 311.2 ms, interquartile range 81.4 ms.
- The gate answered `CELL_LOST` for a cell on the killed node 9009.1 ms after the kill, inside the 10 s lease.
- After the run, 26 cells were running on the two combs left and the 4 cells that were on node 2 were Lost. None of the cells elsewhere was touched.
- The bench stopped the 26 cells left in 315.7 ms. The gate, keeper and comb logs had no errors or warnings, and no cgroups were left behind.

Losing a node mid burst cost nothing a client could see here: the creates after the kill went to the two combs left on the first try. A lost cell is reported as lost within the lease and not as running.
