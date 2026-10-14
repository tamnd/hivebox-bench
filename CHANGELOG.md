# Changelog

Every change that a person reading a report would care about goes here, newest first. A change to how a number is computed is always one of those.

## Unreleased

- The first M0 node report, `reports/2026-10-14`: on one shared VM a hivebox create has a p50 of 104.6 ms at 1/s against 1131.4 ms for Docker, a no-op exec 5.7 ms against 303.1 ms, 1,000 cells run commands at once with none failing, and 2,500 too once the create deadline is 120 s, and the gold patch resolves 487 of the 500 SWE-bench Verified tasks, 12 of the 13 others needing the internet the cells did not have.
- `--docker SOCKET` runs create-storm and exec against the Docker daemon on that socket instead, as the baseline, with containers of `--cell-mib` and no network.
- `--cell-mib` sets the memory of each cell in create-storm and exec too, not only in density.
- `swe/verified.py` takes `--skip` with the output of an earlier run, to carry on where it stopped, and waits for room when the node turns a cell away for it, for up to an hour.
- `swe/verified.py` names the upstream image of each task itself, waits out Docker Hub's rate limit, and fetches an image again when a layer loses its connection. Before, a layer that failed halfway left the import waiting for the rest of its input for good.
- `swe/grpo.py`, a GRPO loop over hivebox cells through verl's agent loop and hivebox's verl adapter, with a policy small enough to train on a CPU. The first report, `reports/2026-10-13`, has 768 rollouts with every reward right, and 80 of 256 samples masked at 16 in flight until hivebox#175 made the adapter wait for room.
- The cpu-qos suite: a probe cell timing fixed Python steps alone, next to hog cells in the same class, and next to hogs in the best effort class. The first report, `reports/2026-10-05`, has the latency probe within 1.6% of alone at p99 with best effort hogs on every core, against +186.1% with no classes.
- The first report, `reports/2026-10-04`: image-import, cold-image, replay and node-loss on one shared VM.
- The M1 suites: replay and node-loss through a gate, with the keeper's write rate read from its applied index, and cold-image and image-import through the `hive-nectar` binary. hive-sdk moves to v0.0.17, and hive-proto at the same tag is new.
- The suite list, with the target each suite is judged against and the hivebox milestone it needs.
- Nearest rank percentiles and the summary every report prints.
- CI that checks the harness builds and works, and never times anything.
