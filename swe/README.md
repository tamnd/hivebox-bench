# SWE-bench Verified on a node

`verified.py` runs every task in SWE-bench Verified twice in a hivebox container cell, once with the gold patch and once with no patch. The gold patch has to resolve the task and the empty patch must not. Nothing is scored against a model here. What this checks is that a cell runs the upstream eval the same way the upstream Docker harness does, and how long each step takes on a node.

## What it needs

- A running `hive-comb` with the container backend, and `hive-oci` from the same release.
- About 20 GiB free on the comb's disk. Each task's image is fetched, used and removed before the next one.
- Python 3.10 or newer, with the packages in `requirements.txt`.
- The dataset as parquet, from the `SWE-bench/SWE-bench_Verified` repository on the Hugging Face hub.

## Running it

```
python -m venv venv && ./venv/bin/pip install -r requirements.txt
HIVE_ENDPOINT=unix:/var/lib/hivebox/comb.sock ./venv/bin/python verified.py \
  --dataset verified.parquet --images /var/lib/hivebox/images \
  --hive-oci /usr/local/bin/hive-oci --out run/shard0.jsonl --logs run/logs --shard 0/2
```

Start one process per shard. Each one writes a JSON line per task to `--out` and skips the tasks already in it, so a run that stops is started again with the same command. To try a task again, delete its line. To split a run that is under way into more shards, stop it and start the new shards with each old `--out` file given as `--skip`.

## What each line says

- `gold` and `empty`: `create`, `apply` and `eval` in seconds, and `resolved` as the upstream grader decides it. A node short of memory turns new cells away, and an attempt waits for room for up to an hour, in which case `waited_for_room` has the seconds it waited.
- `image`: how many layers the task image has, their compressed size in bytes, and `fetch`, the seconds it took to get them from the registry and unpack them.
- `wall`: the seconds the whole task took.
- `error`: set when the image could not be made, in which case nothing ran.

## Images

A task's image is the one the upstream harness builds and publishes, `swebench/sweb.eval.x86_64.<instance id>:latest` on Docker Hub, with `__` in the id written as `_1776_` and the whole name in lower case. A task row that has an `image` field uses that instead.

Docker Hub lets an address without an account read 100 manifests an hour, and three shards started together read more than that in the first hour. A refused request waits a minute and asks again, for up to two hours. A layer of a few gigabytes sometimes loses its connection halfway, so a fetch that fails on the network is thrown away and done again from the start, up to three times, before the task is written down with an `error`.

# GRPO over cells

`grpo.py` is a GRPO loop whose rollouts run in hivebox cells through verl's agent loop and the verl adapter in the hivebox Python SDK, with the reward from `Verify.Run`. The policy is a softmax over five scripted ways to treat psf/requests 2317, from the upstream fix to a fix that tampers with a test, so it trains on a CPU. What it measures is the sandbox side of a step: how long a group takes, whether every reward is right, what is masked and what is left behind.

It needs the psf__requests-2317 image on the comb, named `swe-requests` or given with `--image`, and verl 0.9 with torch and the hivebox SDK from 0.0.45 or newer:

```
HIVE_ENDPOINT=unix:/var/lib/hivebox/comb.sock COMB_PID=$(pidof hive-comb) python grpo.py \
  --steps 16 --prompts 2 --group 8 --inflight 16 --out grpo.jsonl
```

Each step writes a line with `kind` `step`, which has the rollout time, the rewards, how many samples were masked or got a reward their arm should not, the policy after the update, the cells still alive and the comb's RSS, and a line with `kind` `sample` for each sample, with its arm, reward, verify time and the time of each tool call. `--capacity-wait 0` masks a sample the node turns away for room at once, as the SDK did before 0.0.45.
