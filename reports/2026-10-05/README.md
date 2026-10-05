# 2026-10-05: cpu-qos on one machine

The first run of the cpu-qos suite, which measures how much slower a latency class cell gets while best effort cells keep every core busy. The target is 20% or less, and DSec reports 17.3% with SCHED_IDLE and core scheduling together.

## Machine

server3: AMD EPYC Processor (with IBPB), 8 cores, 23 GiB of memory and no swap, kernel 6.8.0-106-generic. It is a VM shared with other people's compile jobs, and the load average was between 45 and 54 during the run. It has no SMT, so the kernel refuses core scheduling cookies and only the `cpu.idle` half of CPU QoS is in these numbers. The comb logged `cells run without core scheduling: No such device (os error 19)` at start.

## Setup

One comb of hivebox 0.0.20 with real cgroups and container cells, all from the swe-requests image. The run was:

```
hivebox-bench run cpu-qos --image swe-requests --raw qos.zst
```

with the defaults: a probe cell of 1 core that runs 200 steps of 200,000 Python loop turns with 20 ms of sleep after each, and 4 hog cells of 2 cores each that run 2 spinning processes each for 30 s, so 8 spinning processes on 8 cores. The probe times every step inside the cell. Each of 5 rounds runs three modes one after the other:

- alone: the probe in the latency class with no hogs.
- no QoS: the probe and the hogs all in the standard class.
- QoS: the probe in the latency class and the hogs in the best effort class, which has `cpu.idle` set.

## Results

| mode | steps | p50 ms | p90 ms | p99 ms | mean ms | p50 vs alone | p99 vs alone | hog work vs no QoS |
|---|---|---|---|---|---|---|---|---|
| alone | 1000 | 22.6 | 55.6 | 73.5 | 29.1 |  |  |  |
| no QoS | 1000 | 57.6 | 112.7 | 210.3 | 65.6 | +155.0% | +186.1% | 1.00 |
| QoS | 1000 | 22.4 | 55.6 | 72.3 | 29.2 | -0.6% | -1.6% | 0.61 |

Per round, p50 and p99 in ms:

| round | alone | no QoS | QoS |
|---|---|---|---|
| 1 | 21.8, 71.0 | 56.8, 158.5 | 22.6, 64.6 |
| 2 | 23.2, 73.9 | 50.6, 166.5 | 22.3, 61.0 |
| 3 | 23.1, 70.2 | 49.7, 169.8 | 21.2, 63.4 |
| 4 | 21.9, 67.5 | 59.3, 202.7 | 23.3, 74.5 |
| 5 | 23.1, 77.7 | 72.1, 341.2 | 23.2, 84.4 |

With the hogs in the best effort class, the probe ran as fast as it did alone: p50 within 0.6% and p99 within 1.6%, both inside the spread between rounds of the alone mode. With everything in one class the probe's p50 went up 155% and its p99 186%. The hogs still did 61% of the work they did with no classes. Best effort cells give way to every task that is not idle, the other tenants' included, so that share is what the probe and the rest of the machine left over. The probe never ran past the hogs.

The spread inside the alone mode, p50 22.6 ms against p90 55.6 ms, is the other tenants of the VM, which this run cannot take out. The latency class does not shield the probe from them, because they are outside the comb's cgroup tree.

## What this does not show

- Core scheduling, which needs SMT. None of the machines we have has it, so the second half of DSec's setup is not measured.
- A heavier probe. The probe sleeps 20 ms after each step, so it uses about half a core. A latency cell that wants its whole core could look different.

## Raw results

`qos.zst` has every probe step of every mode as zstd JSON lines, in the format `src/raw.rs` describes.
