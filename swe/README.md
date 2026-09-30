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

Start one process per shard. Each one writes a JSON line per task to `--out` and skips the tasks already in it, so a run that stops is started again with the same command. To try a task again, delete its line.

## What each line says

- `gold` and `empty`: `create`, `apply` and `eval` in seconds, and `resolved` as the upstream grader decides it.
- `image`: how many layers the task image has, their compressed size in bytes, and `fetch`, the seconds it took to get them from the registry and unpack them.
- `wall`: the seconds the whole task took.
- `error`: set when the image could not be made, in which case nothing ran.

Docker Hub allows 100 anonymous pulls an hour, which is plenty, since a task takes several minutes.
