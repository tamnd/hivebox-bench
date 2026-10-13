# 2026-10-13: GRPO through hivebox's verl adapter, and waiting for room

The first runs of `swe/grpo.py`, a GRPO loop that does its rollouts in hivebox cells through verl's own agent loop and hivebox's verl adapter, and gets each reward from `Verify.Run`. Run B showed that with 16 rollouts in flight the node turned away almost a third of the verifies for room, and the adapter masked those samples, most of them right answers. hivebox#175 makes the adapter, the slime grader and `hive-pollen` ask again instead, and runs C and the two `hive-pollen` runs measure that change.

No language model was trained here. server3 has no GPU, so the policy is a softmax over five scripted ways to treat one task, trained on the CPU with verl's GRPO advantage. What the runs measure is the sandbox side of a GRPO step: how long a group of rollouts takes, whether every reward is right, how many samples are masked and whether anything is left behind. Only one SWE task image is used, psf/requests 2317 from SWE-bench, and there is no Docker baseline, because the disk was 99% full and pulling images on this machine is not allowed.

## Machine

server3: AMD EPYC Processor (with IBPB), 8 cores, 23 GiB of memory and no swap, kernel 6.8.0-106-generic. It is a VM shared with other people's compile jobs, and the load average was between 63 and 86 during runs A and B and between 43 and 54 during the rest. It has no KVM, so only container cells are in these numbers.

## Setup

One comb in its own cgroup with real cgroups and container cells, `mem_mib = 12288` for the node, which with the default overcommit of 1.5 gives an admission ceiling of 18432 MiB. `hive-comb` is a release build of the tree released as hivebox 0.0.44, made just before its version bump, so it calls itself 0.0.43. `hive-pollen` is a release build of the same tree with hivebox#175. The image is the SWE-bench image for psf__requests-2317, already in the store.

The task is the SWE-bench one: `Session.request` turns a bytes method like `b"GET"` into the string `"b'GET'"`. Every sample runs in a cell of 512 MiB and one core, in `/testbed`. The verifier is a cell of 1 GiB and two cores that gets the sample's diff, adds a hidden test that sends `b"GET"` through a session to an adapter that answers at once and checks the method is `"GET"`, and runs it with 32 of the task's own tests:

```
pytest -q -p no:cacheprovider hidden_test.py test_requests.py -k 'binary_method or UtilsTestCase or CaseInsensitive or TestMorselToCookie'
```

`test_*.py` and `**/conftest.py` are protected, so a sample that changes a test gets no reward even when the tests pass.

The five arms the policy picks from each look at the code first, with a `grep` through the adapter's bash tool and a view through its editor tool, and then:

| arm | then | reward it should get |
|---|---|---|
| gold | the upstream fix with the editor tool, a `git diff --stat`, submit | 1 |
| gold-sed | the upstream fix with `sed`, submit | 1 |
| nothing | submit | 0 |
| wrong | `str(method)` in place of `builtin_str(method)`, which still gives `"b'GET'"`, submit | 0 |
| gold+tamper | the upstream fix, then a test appended to `test_requests.py`, submit | 0 |

Each step picks 2 prompts times a group of 8 arms from the policy and runs the 16 samples, each through `HiveAgentLoop` with verl's tool dispatch (`ToolAgentLoop._call_tool`), with the arm's tool calls in place of a model's. The advantages come from `verl.trainer.ppo.core_algos.compute_grpo_outcome_advantage`, the loss is the advantage times the log probability of the arm picked, and Adam at a learning rate of 0.3 takes one step. Masked samples are left out of their group, as a trainer drops them, and a step where every group has the same reward makes no update. After each step the run lists the cells still alive with its labels, and reads the comb's RSS. verl 0.9.1 and torch 2.14.1 on the CPU, Python 3.12.3. All three runs use seed 1, so they pick the same arms as long as their rewards are the same.

```
python swe/grpo.py --steps 16 --prompts 2 --group 8 --inflight 8 --out grpo-a.jsonl
python swe/grpo.py --steps 16 --prompts 2 --group 8 --inflight 16 --out grpo-b.jsonl
python swe/grpo.py --steps 16 --prompts 2 --group 8 --inflight 16 --capacity-wait 300 --out grpo-c.jsonl
```

Runs A and B used the Python SDK of hivebox 0.0.44, which masks a sample the node turns away for room. Run C used the SDK of hivebox#175, which keeps asking for up to 300 s.

The `hive-pollen` runs give the same task to the worker, two tasks of 16 samples each, with the gold fix by `sed` for even samples and the wrong one for odd ones, and the same verifier, once with `limits.capacity_wait_s` 0 and once with 300:

```
hive-pollen --socket $HIVE_SOCKET --project pollen --max-inflight 16 --out pollen-0.jsonl pollen-tasks-0.jsonl
```

## Results

### The GRPO runs

| run | in flight | wait | rollouts | masked | wrong rewards | rollout time | step p50 | step min | step max |
|---|---|---|---|---|---|---|---|---|---|
| A | 8 | none | 256 | 0 | 0 | 250.6 s | 15.6 s | 13.8 s | 18.2 s |
| B | 16 | none | 256 | 80 | 0 | 184.4 s | 11.4 s | 9.1 s | 15.9 s |
| C | 16 | 300 s | 256 | 0 | 0 | 246.3 s | 15.2 s | 13.4 s | 20.0 s |

Every reward that came back was the one its arm should get, 0 out of 768 wrong. The protected path check flagged the gold+tamper arm every time it was picked, 14 times in A and C and 9 in B, and nothing else.

| run | first tool call p50 | p90 | verify p50 | p90 | max | sample p50 | p90 |
|---|---|---|---|---|---|---|---|
| A | 0.35 s | 0.53 s | 6.64 s | 8.05 s | 10.26 s | 7.24 s | 8.71 s |
| B | 0.65 s | 1.08 s | 9.61 s | 11.46 s | 15.15 s | 9.66 s | 12.12 s |
| C | 0.57 s | 0.86 s | 9.32 s | 13.84 s | 18.70 s | 10.21 s | 15.05 s |

The first tool call is when the adapter makes the sample's cell, so it is the create plus one shell command. The verify times leave out the masked samples. The verifier is most of every sample, and with 16 in flight its p50 goes up by about 40% on the 8 core host, which is why C is no faster than A: the node, not the trainer, is the limit here.

All 80 masked samples in B were verifies the node turned away with CAPACITY_UNAVAILABLE: 16 samples in flight each holding 512 MiB, and up to 16 verifiers of 1 GiB on top, pass 18432 MiB. 72 of the 80 were gold or gold-sed samples that should have scored 1. Run B still learned, since the samples it kept had the right rewards, but on fewer of them, and a trainer counting masked samples as failures would have pushed the policy away from the right answer.

The probability of gold or gold-sed after each step:

| step | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 15 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A | 0.532 | 0.662 | 0.770 | 0.852 | 0.852 | 0.909 | 0.945 | 0.967 | 0.980 | 0.987 | 0.987 |
| B | 0.514 | 0.640 | 0.747 | 0.832 | 0.888 | 0.928 | 0.953 | 0.970 | 0.981 | 0.988 | 0.988 |
| C | 0.532 | 0.662 | 0.770 | 0.852 | 0.852 | 0.909 | 0.945 | 0.967 | 0.980 | 0.987 | 0.987 |

C is A to the last digit, since with no masked samples and every reward right the same seed picks the same arms. The mean reward reached 1.0 at step 10 in A and C and stayed there, and in B the mean of the samples it kept did too. From step 10 on every group had the same reward, so there was nothing more to learn and no update was made. A made 9 updates with a p50 of 12.45 ms each, B 10 and C 9.

No cell was left alive after any step in any run. The comb's RSS went from 30 to 37 MiB over A, 39 to 46 MiB over B and 33 to 41 MiB over C.

### hive-pollen

| wait | samples | masked | rewards right | wall | queue p50 | verify p50 | verify max |
|---|---|---|---|---|---|---|---|
| 0 | 32 | 12 | 20 of 20 | 20.4 s | 5.7 s | 8.2 s | 10.6 s |
| 300 s | 32 | 0 | 32 of 32 | 37.7 s | 9.8 s | 10.5 s | 19.0 s |

Without the wait, 7 of the 12 masked samples were gold ones. With it the worker took longer, because the 12 samples it used to drop now run, but it gave every gold sample 1 and every wrong one 0. The verify times leave out the masked samples.

## Raw results

`grpo-a.zst`, `grpo-b.zst` and `grpo-c.zst` have a JSON line for each step and each sample. `pollen-0.zst` and `pollen-300.zst` are the `hive-pollen` output, a line for each sample.
