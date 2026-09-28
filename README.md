# hivebox-bench

The benchmark harness for [hivebox](https://github.com/tamnd/hivebox).

Node suites for create rate, exec latency, snapshot, fork and density on bare metal. Cluster suites that replay a workload fit to the distributions DeepSeek published for DSec, including the headline run of 5,000 creates a second for thirty minutes with 400K cells alive. Real data runs over SWE-bench Verified, SWE-Gym, R2E-Gym and SWE-smith, including a GRPO training job. Baselines against Kubernetes with containerd and Kata, self-hosted E2B, Daytona and plain Docker.

It is a separate repository so that a result can be reproduced by someone who does not trust us, against a released hivebox and not a particular commit of it. hivebox makes performance claims, and those claims are only as good as the measurements behind them. A number without its method is marketing.

The design is [`spec/13_observability_testing_bench.md`](https://github.com/tamnd/hivebox/blob/main/spec/13_observability_testing_bench.md) section 3 in the hivebox repository, and the targets are in [`spec/02_requirements_slos.md`](https://github.com/tamnd/hivebox/blob/main/spec/02_requirements_slos.md).

## Status

Nothing can be measured yet, because the thing being measured does not exist. What is here is the suite list with the target each suite is judged against, the summary arithmetic every report shares, and CI. `hivebox-bench list` prints the suites.

```
cargo run -- list
```

## How numbers are reported

- Every latency is a distribution: count, p50, p90, p99, max and the interquartile range. Never a mean alone and never a minimum.
- Percentiles use nearest rank, so every one of them is a sample that was actually observed.
- Warm and cold are labelled on every row, and a cold number means the cache was actually dropped.
- The machine is described on every report: CPU, cores, memory, kernel, storage and node count.
- The losses go next to the wins. A table with the regressions left out is not a benchmark table.
- A number from a vendor's blog is marked as such and never mixed into a table of our own runs.
- Nothing measured on a shared CI runner is a result. CI here checks that the harness works, not what it says.

## License

Apache-2.0. See [LICENSE-APACHE](LICENSE-APACHE).
