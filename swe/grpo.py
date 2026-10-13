"""GRPO over hivebox cells: verl's agent loop and hivebox's verl adapter, with a policy small enough to train on a CPU.

Each step samples a group of trajectories for every prompt from the policy, runs each one in a cell
of its own through hivebox.verl.loop.HiveAgentLoop, takes the reward Verify.Run gives it, turns the
rewards into group relative advantages with verl's own GRPO function and takes one gradient step.
The policy is a softmax over a few scripted ways to treat psf/requests 2317 rather than a language
model, so the loop runs without a GPU, and what it measures is the sandbox side of a GRPO step: how
long the rollouts take, whether every reward is right, what is masked and what is left behind.
"""

import argparse
import asyncio
import contextlib
import io
import json
import os
import statistics as st
import time
import types
import uuid

import numpy as np
import torch
from verl.experimental.agent_loop.agent_loop import AgentLoopMetrics, AgentLoopOutput
from verl.experimental.agent_loop.tool_agent_loop import ToolAgentLoop
from verl.experimental.agent_loop.tool_parser import FunctionCall
from verl.trainer.ppo.core_algos import compute_grpo_outcome_advantage

import hivebox
from hivebox.verl import BashTool, EditorTool, SubmitTool
from hivebox.verl.loop import HiveAgentLoop

# A test for the bug the task is about, which the image does not have.
HIDDEN = '''import requests
from requests.adapters import BaseAdapter
from requests.models import Response


class Echo(BaseAdapter):
    def send(self, request, **kwargs):
        r = Response()
        r.status_code = 200
        r.request = request
        r._content = b""
        return r

    def close(self):
        pass


def test_binary_method_is_native():
    s = requests.Session()
    s.mount("http://", Echo())
    r = s.request(b"GET", "http://x.invalid/")
    assert r.request.method == "GET"
'''
# The hidden test and 32 of the task's own tests.
TESTS = ("source /opt/miniconda3/bin/activate testbed && python -m pytest -q -p no:cacheprovider hidden_test.py test_requests.py"
         " -k 'binary_method or UtilsTestCase or CaseInsensitive or TestMorselToCookie'")
OLD, GOLD, WRONG = "method = builtin_str(method)", "method = to_native_string(method)", "method = str(method)"
LOOK = [("bash", {"command": "grep -n 'builtin_str(method)' requests/sessions.py"}),
        ("str_replace_editor", {"command": "view", "path": "requests/sessions.py", "view_range": [420, 440]})]


def edit(new):
    return ("str_replace_editor", {"command": "str_replace", "path": "requests/sessions.py", "old_str": OLD, "new_str": new})


# What the policy picks from: the tool calls of each, and the reward each should get.
ARMS = {
    "gold": (LOOK + [edit(GOLD), ("bash", {"command": "git -c safe.directory='*' diff --stat"}), ("submit", {})], 1.0),
    "gold-sed": (LOOK + [("bash", {"command": f"sed -i 's/{OLD}/{GOLD}/' requests/sessions.py"}), ("submit", {})], 1.0),
    "nothing": (LOOK + [("submit", {})], 0.0),
    "wrong": (LOOK + [edit(WRONG), ("submit", {})], 0.0),
    "gold+tamper": (LOOK + [edit(GOLD), ("bash", {"command": "printf '\\ndef test_nothing():\\n    pass\\n' >> test_requests.py"}),
                            ("submit", {})], 0.0),
}
NAMES = list(ARMS)
LIVE = ("pending", "preparing", "starting", "running", "pausing", "paused", "stopping")


def config(args, run):
    return {"type": "native", "endpoint": args.endpoint, "project": "grpo", "task": {
        "image": args.image, "workdir": "/testbed", "cell": {"mem_mib": 512, "vcpu_milli": 1000},
        "labels": {"run": run}, "limits": {"command_timeout_s": 60, "max_wall_s": 600, "capacity_wait_s": args.capacity_wait},
        "verify": {"argv": ["bash", "-c", TESTS], "protected_paths": ["test_*.py", "**/conftest.py"],
                   "files": {"hidden_test.py": HIDDEN}, "cell": {"mem_mib": 1024, "vcpu_milli": 2000}}}}


def tools(cfg):
    # One set for the run, as verl makes them once for each agent loop worker. verl prints each
    # tool's schema as it makes it, which is left out.
    with contextlib.redirect_stdout(io.StringIO()):
        return {t.name: t for t in (BashTool(cfg), EditorTool(cfg), SubmitTool(cfg))}


def make_loop(tools):
    # Only what verl's tool dispatch reads, so it runs without a model server or a trainer.
    loop = HiveAgentLoop.__new__(HiveAgentLoop)
    loop.tools = tools
    loop.max_tool_response_length, loop.tool_response_truncate_side = 100_000, "middle"
    return loop


async def scripted_run(self, sampling_params, **kwargs):
    # Stands in for the model's turns: the arm's tool calls through verl's dispatch, timed.
    a = types.SimpleNamespace(request_id=uuid.uuid4().hex, extra_fields={})
    for name, args in ARMS[kwargs["arm"]][0]:
        t = time.monotonic()
        await self._call_tool(FunctionCall(name=name, arguments=json.dumps(args)), {}, a)
        kwargs["timings"].append([name, round(time.monotonic() - t, 3)])
    return AgentLoopOutput(prompt_ids=[1], response_ids=[2], response_mask=[1], metrics=AgentLoopMetrics(), extra_fields={})


ToolAgentLoop.run = scripted_run


async def sample(cfg, tools, sem, step, prompt, k, arm):
    async with sem:
        timings = []
        t = time.monotonic()
        task = {"task_id": f"s{step}-p{prompt}-{k}", "labels": {**cfg["task"]["labels"], "step": str(step)}}
        out = await make_loop(tools).run({}, arm=arm, timings=timings, extra_info={"hive": task})
        f = out.extra_fields
        return {"step": step, "prompt": prompt, "arm": arm, "reward": out.reward_score, "want": ARMS[arm][1],
                "masked": f["hive_infra_error"], "tampered": f["hive_tampered"], "error": f["hive_error"],
                "verify_s": f["hive_verify_s"], "wall_s": round(time.monotonic() - t, 3), "calls": timings}


def rss_mib(pid):
    try:
        with open(f"/proc/{pid}/status") as f:
            return next(int(line.split()[1]) // 1024 for line in f if line.startswith("VmRSS:"))
    except (OSError, StopIteration):
        return None


async def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--endpoint", default=os.environ.get("HIVE_ENDPOINT"))
    p.add_argument("--image", default="swe-requests", help="the comb's name for the psf__requests-2317 image")
    p.add_argument("--steps", type=int, default=16)
    p.add_argument("--prompts", type=int, default=2, help="prompts a step, each its own group")
    p.add_argument("--group", type=int, default=8, help="samples a prompt")
    p.add_argument("--inflight", type=int, default=8, help="trajectories running at once")
    p.add_argument("--capacity-wait", type=float, default=300,
                   help="seconds to keep asking for a cell or a verify the node has no room for, 0 to mask the sample at once")
    p.add_argument("--lr", type=float, default=0.3)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--comb-pid", type=int, default=int(os.environ.get("COMB_PID", "0")))
    p.add_argument("--out", required=True)
    args = p.parse_args()

    run = uuid.uuid4().hex[:8]
    cfg = config(args, run)
    kit = tools(cfg)
    hive = hivebox.AsyncHive(args.endpoint, project="grpo")
    logits = torch.zeros(len(NAMES), requires_grad=True)
    opt = torch.optim.Adam([logits], lr=args.lr)
    gen = torch.Generator().manual_seed(args.seed)
    sem = asyncio.Semaphore(args.inflight)
    start = time.monotonic()
    with open(args.out, "w") as out:
        for step in range(args.steps):
            probs = torch.softmax(logits.detach(), 0)
            picks = torch.multinomial(probs, args.prompts * args.group, replacement=True, generator=gen).tolist()
            t = time.monotonic()
            samples = await asyncio.gather(*(sample(cfg, kit, sem, step, i // args.group, i % args.group, NAMES[a])
                                             for i, a in enumerate(picks)))
            rollout = time.monotonic() - t

            # Masked samples are left out of their group, as a trainer drops them.
            t = time.monotonic()
            kept = [s for s in samples if not s["masked"]]
            if kept:
                rewards = torch.tensor([[s["reward"]] for s in kept])
                adv, _ = compute_grpo_outcome_advantage(rewards, torch.ones_like(rewards),
                                                        np.array([s["prompt"] for s in kept]))
                adv = adv[:, 0]
            signal = bool(kept) and bool(adv.abs().sum() > 0)
            if signal:
                idx = torch.tensor([NAMES.index(s["arm"]) for s in kept])
                loss = -(adv * torch.log_softmax(logits, 0)[idx]).mean()
                opt.zero_grad()
                loss.backward()
                opt.step()
            update = time.monotonic() - t

            left = len(await hive.cells.list(labels={"run": run, "step": str(step)}, states=LIVE))
            live = len(await hive.cells.list(states=LIVE))
            rec = {
                "step": step, "rollout_s": round(rollout, 2), "update_ms": round(update * 1000, 2),
                "reward_mean": round(st.mean(s["reward"] for s in samples), 4),
                "wrong_rewards": sum(s["reward"] != s["want"] for s in samples if not s["masked"]),
                "masked": len(samples) - len(kept), "updated": signal,
                "groups_all_same": sum(len({s["reward"] for s in kept if s["prompt"] == g}) == 1 for g in range(args.prompts)),
                "picked": {n: picks.count(i) for i, n in enumerate(NAMES)},
                "probs_after": {n: round(v, 4) for n, v in zip(NAMES, torch.softmax(logits.detach(), 0).tolist())},
                "left_after_step": left, "live_cells": live, "comb_rss_mib": rss_mib(args.comb_pid) if args.comb_pid else None,
                "verify_s_median": st.median(s["verify_s"] for s in samples),
                "sample_s_median": st.median(s["wall_s"] for s in samples),
            }
            out.write(json.dumps({"kind": "step", **rec}) + "\n")
            for s in samples:
                out.write(json.dumps({"kind": "sample", **s}) + "\n")
            out.flush()
            print(f"step {step}: {rec['rollout_s']} s, reward {rec['reward_mean']}, wrong {rec['wrong_rewards']}, masked {rec['masked']}, "
                  f"left {left}/{live}, rss {rec['comb_rss_mib']}, p(gold+sed) {rec['probs_after']['gold'] + rec['probs_after']['gold-sed']:.3f}",
                  flush=True)
    print(f"{args.steps} steps of {args.prompts * args.group} in {time.monotonic() - start:.1f} s")
    await hive.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
