"""SWE-bench Verified on a hivebox node: the gold patch has to pass and the empty patch has to fail.

Each task runs in a container cell made from the task's own image. The image's layers come
straight from the registry and are applied one by one with `hive-oci import --layer` into the
comb's image directory, which writes each file once. Going through docker instead writes the image
three times, once to pull it, once to export it and once to import it, and on a busy disk that
took five to nine minutes a task where the eval took one. The image is removed again after the
task unless --keep-images is given, so a run needs room for one image at a time and not for all
500. The eval script and the grading are the ones the `swebench` package ships with the
SWE-bench/SWE-bench_Verified dataset, so a pass here means the same thing it means in the upstream
Docker harness.

One JSON line per task goes to --out, and a task already in that file is skipped, so a run that
stops can be started again with the same arguments.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request
import zlib

import pyarrow.parquet as pq
from swebench.harness.grading import get_eval_report
from swebench.harness.utils import make_test_spec

import hivebox

# The same order the upstream harness tries them in.
APPLY = (
    "git apply --verbose /tmp/patch.diff || "
    "git apply --verbose --reject /tmp/patch.diff || "
    "patch --batch --fuzz=5 -p1 -i /tmp/patch.diff"
)

REGISTRY = "https://registry-1.docker.io/v2"
INDEXES = ("application/vnd.oci.image.index.v1+json", "application/vnd.docker.distribution.manifest.list.v2+json")
MANIFESTS = ("application/vnd.oci.image.manifest.v1+json", "application/vnd.docker.distribution.manifest.v2+json")


def image_name(instance_id: str) -> str:
    return "swe-" + re.sub(r"[^a-z0-9-]", "-", instance_id.lower())


class Registry:
    """Just enough of the registry API to read one image from Docker Hub without docker."""

    def __init__(self, ref: str):
        repo, _, tag = ref.partition(":")
        self.repo = repo if "/" in repo else "library/" + repo
        self.tag = tag or "latest"
        self._token()

    def _token(self) -> None:
        url = f"https://auth.docker.io/token?service=registry.docker.io&scope=repository:{self.repo}:pull"
        with urllib.request.urlopen(url, timeout=60) as r:
            self.token = json.load(r)["token"]

    def open(self, path: str, accept: str = ""):
        headers = {"Authorization": "Bearer " + self.token}
        if accept:
            headers["Accept"] = accept
        req = urllib.request.Request(f"{REGISTRY}/{self.repo}/{path}", headers=headers)
        return urllib.request.urlopen(req, timeout=300)

    def manifest(self) -> dict:
        """The linux/amd64 image manifest."""
        with self.open(f"manifests/{self.tag}", ",".join(INDEXES + MANIFESTS)) as r:
            m = json.load(r)
        if m.get("mediaType") in INDEXES or "manifests" in m:
            want = [d for d in m["manifests"] if d.get("platform", {}).get("architecture") == "amd64"
                    and d.get("platform", {}).get("os") == "linux"]
            with self.open(f"manifests/{want[0]['digest']}", ",".join(MANIFESTS)) as r:
                m = json.load(r)
        return m

    def blob(self, digest: str):
        self._token()
        return self.open(f"blobs/{digest}")


def apply_layer(reg: Registry, layer: dict, dest: str, hive_oci: str) -> int:
    """Streams one layer from the registry into `hive-oci import --layer`, checking its digest on the
    way. Returns its compressed size."""
    kind = layer["mediaType"]
    if not kind.endswith(("tar.gzip", "tar+gzip", ".tar")):
        raise RuntimeError(f"layer {layer['digest']} is {kind}, which this runner does not unpack")
    inflate = zlib.decompressobj(wbits=47) if "gzip" in kind else None
    sha, size = hashlib.sha256(), 0
    imp = subprocess.Popen([hive_oci, "import", dest, "--layer"], stdin=subprocess.PIPE,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        with reg.blob(layer["digest"]) as r:
            while chunk := r.read(1 << 20):
                sha.update(chunk)
                size += len(chunk)
                imp.stdin.write(inflate.decompress(chunk) if inflate else chunk)
        if inflate:
            imp.stdin.write(inflate.flush())
        imp.stdin.close()
    except BrokenPipeError:
        pass
    finally:
        err = imp.stderr.read().decode(errors="replace")
        code = imp.wait()
    if code != 0:
        raise RuntimeError(f"hive-oci import of layer {layer['digest']}: {err.strip()}")
    if "sha256:" + sha.hexdigest() != layer["digest"]:
        raise RuntimeError(f"layer {layer['digest']} came down as sha256:{sha.hexdigest()}")
    return size


def prepare(task: dict, images: str, hive_oci: str) -> tuple[str, dict, dict]:
    """Makes the task's image available to the comb. Returns its name there, the environment
    the image sets and what fetching it took."""
    name = image_name(task["instance_id"])
    dest = os.path.join(images, name)
    t = time.monotonic()
    reg = Registry(task["image"])
    m = reg.manifest()
    with reg.blob(m["config"]["digest"]) as r:
        config = json.load(r)
    env = dict(e.split("=", 1) for e in config.get("config", {}).get("Env") or [])
    info = {"layers": len(m["layers"]), "bytes": 0}
    if not os.path.isdir(dest):
        try:
            for layer in m["layers"]:
                info["bytes"] += apply_layer(reg, layer, dest, hive_oci)
        except BaseException:
            shutil.rmtree(dest, ignore_errors=True)
            raise
    info["fetch"] = time.monotonic() - t
    return name, env, info


async def attempt(hive, task: dict, name: str, env: dict, patch: str, run: str, timeout: float, logs: str) -> dict:
    """Runs the eval script once with `patch` applied, and grades the log."""
    spec = hivebox.Spec(image=name, backend="container", env=env, labels={"bench": run, "task": task["instance_id"][:63]})
    out: dict = {}
    t = time.monotonic()
    cell = await hive.cells.create(spec)
    out["create"] = time.monotonic() - t
    try:
        log = []
        if patch:
            await cell.files.write("/tmp/patch.diff", patch)
            t = time.monotonic()
            r = await cell.run(APPLY, cwd="/testbed", timeout=600)
            out["apply"] = time.monotonic() - t
            log.append((r.stdout + r.stderr).decode(errors="replace"))
            if r.exit_code != 0:
                out.update(applied=False, resolved=False)
                return out
        out["applied"] = True
        await cell.files.write("/eval.sh", make_test_spec(task).eval_script)
        t = time.monotonic()
        r = await cell.run(["/bin/bash", "-c", "/bin/bash /eval.sh 2>&1"], timeout=timeout, max_output_bytes=64 << 20)
        out["eval"] = time.monotonic() - t
        out["timed_out"], out["truncated"] = r.timed_out, r.truncated
        log.append(r.stdout.decode(errors="replace"))
        kind = "gold" if patch else "empty"
        path = os.path.join(logs, f"{task['instance_id']}.{kind}.log")
        with open(path, "w") as f:
            f.write("\n".join(log))
        pred = {"instance_id": task["instance_id"], "model_name_or_path": "gold", "model_patch": patch}
        report = get_eval_report(make_test_spec(task), pred, path, include_tests_status=True)
        rep = report[task["instance_id"]]
        out["resolved"] = rep["resolved"]
        out["infra_failure"] = rep.get("infra_failure", False)
        if not rep["resolved"] and patch:
            st = rep.get("tests_status", {})
            out["failed_tests"] = {k: v.get("failure", [])[:10] for k, v in st.items() if v.get("failure")}
        return out
    except hivebox.HiveError as e:
        out.update(error=str(e), infra=e.is_infra_error, resolved=False)
        return out
    finally:
        try:
            await cell.stop()
        except hivebox.HiveError:
            pass


async def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--dataset", required=True, help="the SWE-bench/SWE-bench_Verified parquet file")
    p.add_argument("--images", required=True, help="the comb's image directory, data_dir/images")
    p.add_argument("--hive-oci", default="hive-oci")
    p.add_argument("--out", required=True)
    p.add_argument("--logs", required=True, help="where each attempt's log is kept")
    p.add_argument("--only", default="", help="comma separated instance ids")
    p.add_argument("--shard", default="0/1", help="i/n: the ith of n contiguous blocks of tasks")
    p.add_argument("--timeout", type=float, default=1800)
    p.add_argument("--keep-images", action="store_true")
    p.add_argument("--min-free-gib", type=float, default=12, help="stop before a task when the disk has less free")
    a = p.parse_args()

    tasks = pq.read_table(a.dataset).to_pylist()
    tasks.sort(key=lambda t: t["instance_id"])
    if a.only:
        wanted = set(a.only.split(","))
        tasks = [t for t in tasks if t["instance_id"] in wanted]
    i, n = map(int, a.shard.split("/"))
    size = -(-len(tasks) // n)
    tasks = tasks[i * size : (i + 1) * size]
    os.makedirs(a.logs, exist_ok=True)
    done = set()
    if os.path.exists(a.out):
        with open(a.out) as f:
            done = {json.loads(line)["instance_id"] for line in f if line.strip()}
    run = f"swe-{os.getpid()}"
    async with hivebox.AsyncHive(project="bench") as hive:
        for k, task in enumerate(tasks):
            iid = task["instance_id"]
            if iid in done:
                continue
            free = shutil.disk_usage(a.images).free / (1 << 30)
            if free < a.min_free_gib:
                print(f"stopping before {iid}: {free:.1f} GiB free", flush=True)
                break
            row: dict = {"instance_id": iid, "repo": task["repo"]}
            started = time.monotonic()
            try:
                name, env, row["image"] = prepare(task, a.images, a.hive_oci)
            except (OSError, RuntimeError, ValueError, KeyError) as e:
                row["error"] = f"image: {e}"[:1000]
                row["gold"] = row["empty"] = None
            else:
                row["gold"] = await attempt(hive, task, name, env, task["patch"], run, a.timeout, a.logs)
                row["empty"] = await attempt(hive, task, name, env, "", run, a.timeout, a.logs)
                if not a.keep_images:
                    shutil.rmtree(os.path.join(a.images, name), ignore_errors=True)
            row["wall"] = time.monotonic() - started
            with open(a.out, "a") as f:
                f.write(json.dumps(row) + "\n")
            gold = row["gold"] and row["gold"].get("resolved")
            empty = row["empty"] and row["empty"].get("resolved")
            print(f"[{k + 1}/{len(tasks)}] {iid} gold={gold} empty={empty} {row['wall']:.0f}s", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
