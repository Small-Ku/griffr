# Issue #1: bounded WorkUnit re-arbitration pilot

## Scope and authority

This slice implements executable research candidates in `#[cfg(test)]` code. It does not enable a production WorkUnit or deadline API. The maintainer owns acceptance and rollout.

The starting and rechecked canonical `main` is `995b063f5c9320359b4dd9268f0fd4e89fc11992`. Issue [#1](https://github.com/Small-Ku/griffr/issues/1) was read on 2026-10-02, including its latest body (updated 2026-10-02, no comments). Local `AGENTS.md`, `WORDING.md`, task-pool, compio, resource, optimization, patch, and testing docs govern this work. Other repositories were not modified.

Baseline findings:

- `TaskRun::Continue(Task)` already preserves the graph node, dependencies, and waiters. The coordinator releases admission before `graph.finish` and routes the replacement task again.
- `ResourceRequest`, volume/path/storage admission, and Dispatcher routing remain authoritative. Capacity is not a graph edge.
- Extraction already has 256 MiB source / 512-entry shard limits, but one entry is not split. These byte/count limits do not bound wall time.
- The bounded two-batch HTTP writer is internal backpressure. It does not release the task's coordinator permits after each batch.
- Commit `3f8539d` added SIMD MD5 batching. A replacement that loses this batching needs direct comparison with it.
- No checked-in direct WorkUnit/deadline ablation was found. Existing task-duration and queue-wait metrics are not useful-resource utilization or end-to-end latency evidence. The unmodified baseline's 134 task-pool tests passed.

The bounded plan was: implement test-only candidates over the existing graph/admission; compare five policies on mixed, throughput, tiny, high-resume-cost, solo, and deadline-pressure workloads; measure real Dispatcher read/hash resume cost and canonical verification; retain raw results; recommend rollout only if evidence supports it. No new executor, dependency, build profile, toolchain pin, CLI option, or serving-specific concept was added.

## Candidate semantics

The deterministic model attaches cursor, remaining atoms, queue-ready time, optional soft deadline, next quantum, and cost EWMA to an existing graph node. The placeholder `Task::Verify` payload is never run in the model. `TaskGraph::mark_running/finish` and production `ResourceState::can_acquire/acquire/release` handle every transition. The model's selection loop is a research policy, not the production `SchedulerQueue` or coordinator.

| Candidate | Quantum / arbitration |
| --- | --- |
| Coarse | All remaining safe atoms; union of their resource demands held throughout |
| FixedSmall | At most 2 atoms |
| FixedLarge | At most 32 atoms |
| Progressive | Starts at 2, doubles up to 32; returns to 2 under contention |
| SoftDeadline | Progressive sizing; urgent when estimated next cost reaches remaining slack |

A candidate must pass admission before ranking. Aging after 20 model ms outranks urgency; after three continuations, runnable bulk gets an opportunity. These research constants do not replace production age buckets, per-class continuation counters, network weighting, writer reservation delay, bounded lookahead/frontier, or routing caches. EWMA uses 3/4 old cost and 1/4 observed cost; its starting estimate overstates the constant actual atom cost by 2x. Deadline feasibility across remaining work or dependency chains is not predicted.

The model uses one network, CPU, blocking, extract, volume-read, and volume-write slot, mixed read/write pressure 2, unique output mutation paths, and a fixed free-space snapshot 16 with writer reservations 8. Separate tests reject saturated metadata, path conflicts, and overlapping storage reservations. Assertion failures abort the evidence run; `capacity_violations=0` is reported only after these checks pass. This is evidence for the tested configurations, not a proof for arbitrary requests or platform free-space behavior.

One atom costs 1 model ms. Scheduler/dispatch cost is an assumed 20 us per dispatch. Resume cost is assumed 40 us, raised to 1 ms in `costly-resume`. These values are sensitivity parameters, not calibrated machine measurements. `scheduler_host_ns` measures host time in model selection / graph finishing; it does not feed back into model time.

Workloads:

- `mixed`: eight background nodes, each download 8 / read 8 / extract 40 / write 8 atoms; sixteen two-atom foreground nodes arrive at 5 ms then every 3 ms, with a 16 ms soft SLO. A two-atom child depends on background node 0.
- `costly-resume`: the same work with expensive resume.
- `throughput`: sixteen 64-atom CPU nodes ready at time zero.
- `tiny`: thirty-two one-atom CPU nodes.
- `solo`: one 64-atom CPU node; permits progressive growth.
- `deadline-pressure`: eight 64-atom CPU background nodes plus sixteen two-atom foreground nodes arriving every 1 ms from 5 ms, with 16 ms SLOs.

The useful activity channels are `[network, CPU, blocking work, read, write, extract]`; held channels follow the admission vector. A mixed CPU-dispatched bundle can do blocking work without holding a separate logical blocking token. Useful time follows phase activity, while held time follows tokens. Neither is OS CPU/disk utilization. Idle gaps are the largest union gap in useful activity, including start/end of the command. A channel never used has a whole-command gap.

Latency p50/p95 use nearest rank over node arrival-to-finish time, including dependency wait. Foreground and background p95 are separate. Throughput counts useful atoms/second. `max_ready_wait_us` is the largest continuous wait for an eligible quantum, not first-start or whole-job delay. All finite test jobs finish; this does not prove starvation freedom under unbounded arrivals. `critical_unlock_us` measures parent finish to dependent child finish, including its service time. Model non-arbitration includes useful service plus assumed dispatch/resume cost while the bundle is held; no wall-clock guarantee follows.

## Safe yield and resume boundaries

| Work | Safe boundary / required state | Pilot status |
| --- | --- | --- |
| Read/hash | After a completed read and hash update; retain descriptor, cursor, incremental hash, expected identity | Real single-file pilot verifies byte count and final digest, including empty, 55/64/65-byte and partial-block cases |
| MD5 batch | After all lane reads/updates in a round; retain per-file states and preserve SIMD batching and resource accounting | Existing canonical batch measured; resumable batch not implemented |
| Download | After in-flight writes drain and prefix/digest state is consistent; preserve response ownership or validated range resume | Existing writer/resume tests only; no new re-arbitration |
| Extraction | After a verified entry commits or stages privately; retain source/range-reader lifetime and deferred controls | Existing shard/entry graph boundaries only; codec-internal resume not proven |
| Patch | Between committed entry tasks with base-consumer dependencies intact | Existing graph boundary only; HDiff call remains coarse |
| Write/commit | Completed positional writes to a private temp, consistent offset/hash/allocation; final atomic replacement is indivisible | No new writer resume; never release an install-path exclusion in the middle of a visible mutation |

The read pilot resumes in memory within one command. It is not a persisted crash checkpoint, has no concurrent-writer identity defense, and does not establish cancellation/retry behavior for a new production task. Download response migration, ZIP/HDiff state, range-cache lifetimes, cumulative allocated storage, and path safety must be resolved before extending the pilot to production.

## External evidence

Read on 2026-10-02. These sources inform hypotheses, not repository authority:

- [BreezeRT](https://breezeblue.ai/blog/breezert-ultra-low-latency-tts): progressive chunks trade first-output latency against repeated codec/scheduler cost. Its predictor allows batching only when playback slack permits it. Its audio/GPU results cannot establish Griffr whole-task latency or disk throughput gains.
- [OpenHarmony FFRT graph guide](https://github.com/openharmony/resourceschedule_ffrt/blob/master/docs/ffrt-concurrency-graph-cpp.md): task/data dependencies become runtime-visible readiness constraints. Griffr already has task dependency readiness; FFRT's versioned data-dependency model and coroutine backend are outside this slice.
- [Linux EEVDF](https://docs.kernel.org/scheduler/sched-eevdf.html): deadline ranking is coupled with fair-share eligibility/lag. A virtual deadline differs from a user soft SLO. The lesson is to test fairness alongside urgency, not to copy kernel policy.
- [compio Dispatcher source](https://github.com/compio-rs/compio/blob/master/compio-dispatcher/src/lib.rs): blocking dispatch uses the configured shared pool and can return the original closure when full. Checked against locked local `compio-dispatcher 0.11.1`; Griffr already restores rejected tasks. Cooperative byte-loop yields do not create coordinator re-admission unless the task returns a continuation.

## Reproduce

Use the configured `mise` cargo wrapper and `mr-boxington` managed target. No standalone benchmark build authority is required:

```sh
mise exec -- cargo test -p griffr-runtime --lib work_unit_research
mise exec -- cargo test -p griffr-runtime --profile ci --lib work_unit -- \
  --ignored --nocapture --test-threads=1 > work-unit-evidence.log
```

Extract the JSON objects from the captured test stdout (libtest may prefix the first object with the test name):

```python
import json
from pathlib import Path
rows = []
for line in Path("work-unit-evidence.log").read_text().splitlines():
    if "{" in line:
        rows.append(json.loads(line[line.index("{"):]))
model = [r for r in rows if "workload" in r]
probe = [r for r in rows if "kind" in r]
```

The real probe uses a 16 MiB deterministic file, 256 KiB fixed-small and 4 MiB fixed-large quanta, progressive doubling 256 KiB to 4 MiB, one warm-up per candidate, seven measured repeats, and rotated candidate order. The exact-byte bound needs one extra EOF dispatch. It preserves one real graph node and re-admits CPU/read resources around each `Dispatcher::dispatch_blocking` call. An open descriptor and incremental MD5 state move into/out of the closure; buffer allocation is charged to resume setup. SoftDeadline has no distinct arbitration when only one node is ready.

`wall_ns` includes channel wait and scheduling. `scheduler_ns` measures admission, graph transitions, submission and finish handling, excluding waiting for useful work. `resume_ns` measures per-quantum buffer setup including the first run, not fsync or durable checkpoint cost. `max_non_arbitration_ns` spans admission to permit release; `max_worker_run_ns` isolates the worker body. Canonical probes use the real `TaskPoolRunner::run_batch` for one and four 16 MiB MD5 files; the four-file case retains production SIMD batching. Canonical per-dispatch p95 is not end-to-end command p95 or a maximum held interval.

Seven warm-cache VM repeats cannot estimate production p95 reliably or prove a significant win. No competing workload runs in the real read pilot. The model compares arbitration; the real probe checks resume correctness and its costs. Keep those claims separate.

## Results from the final run

Raw evidence: [30 model rows](issue-1-model.jsonl), [49 real timing rows](issue-1-dispatcher.jsonl). Captured after workspace builds/checks finished, on 2026-10-02. Host: Linux 6.18.44, x86_64 AMD EPYC 9V74 VM, three visible CPUs and cgroup quota two CPUs. Cargo/rustc 1.99.0, mise 2026.10.0, mr-boxington 1.21.0, repository `ci` profile (opt-level 1). The environment already provided these tools; no toolchain was installed or repinned. Repository CI's Rust 1.97.1 and Windows were not exercised.

All model time below is **synthetic**, in ms. Foreground p95 differs from whole-node p95. Zero means no foreground work or no dependent child, as defined above.

| Workload | Policy | Command | Node p50 | Node p95 | Foreground p95 | Max non-arbitration | Resume cost | Requeues | Max ready wait |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| mixed | Coarse | 544.48 | 499.38 | 512.16 | 509.18 | 64.02 | 0.00 | 0 | 507.16 |
| mixed | FixedSmall | 496.10 | 2.80 | 494.04 | 3.96 | 2.06 | 9.92 | 248 | 28.56 |
| mixed | FixedLarge | 544.96 | 499.86 | 512.64 | 509.66 | 32.06 | 0.32 | 8 | 507.64 |
| mixed | Progressive | 496.10 | 2.80 | 494.04 | 3.96 | 2.06 | 9.92 | 248 | 28.56 |
| mixed | SoftDeadline | 496.10 | 2.80 | 494.04 | 3.96 | 2.06 | 9.92 | 248 | 28.56 |
| throughput | Coarse | 1024.32 | 512.16 | 1024.32 | 0.00 | 64.02 | 0.00 | 0 | 960.30 |
| throughput | FixedSmall | 1054.08 | 1031.42 | 1054.08 | 0.00 | 2.06 | 19.84 | 496 | 75.62 |
| throughput | FixedLarge | 1025.28 | 512.64 | 1025.28 | 0.00 | 32.06 | 0.64 | 16 | 929.14 |
| throughput | Progressive | 1054.08 | 1031.42 | 1054.08 | 0.00 | 2.06 | 19.84 | 496 | 75.62 |
| throughput | SoftDeadline | 1054.08 | 1031.42 | 1054.08 | 0.00 | 2.06 | 19.84 | 496 | 75.62 |
| tiny | Coarse | 32.64 | 16.32 | 31.62 | 0.00 | 1.02 | 0.00 | 0 | 31.62 |
| tiny | FixedSmall | 32.64 | 16.32 | 31.62 | 0.00 | 1.02 | 0.00 | 0 | 31.62 |
| tiny | FixedLarge | 32.64 | 16.32 | 31.62 | 0.00 | 1.02 | 0.00 | 0 | 31.62 |
| tiny | Progressive | 32.64 | 16.32 | 31.62 | 0.00 | 1.02 | 0.00 | 0 | 31.62 |
| tiny | SoftDeadline | 32.64 | 16.32 | 31.62 | 0.00 | 1.02 | 0.00 | 0 | 31.62 |
| costly-resume | Coarse | 544.48 | 499.38 | 512.16 | 509.18 | 64.02 | 0.00 | 0 | 507.16 |
| costly-resume | FixedSmall | 727.88 | 2.02 | 724.86 | 4.34 | 3.02 | 248.00 | 248 | 32.26 |
| costly-resume | FixedLarge | 552.64 | 507.54 | 520.32 | 517.34 | 33.02 | 8.00 | 8 | 515.32 |
| costly-resume | Progressive | 727.88 | 2.02 | 724.86 | 4.34 | 3.02 | 248.00 | 248 | 32.26 |
| costly-resume | SoftDeadline | 727.88 | 2.02 | 724.86 | 4.34 | 3.02 | 248.00 | 248 | 32.26 |
| solo | Coarse | 64.02 | 64.02 | 64.02 | 0.00 | 64.02 | 0.00 | 0 | 0.00 |
| solo | FixedSmall | 65.88 | 65.88 | 65.88 | 0.00 | 2.06 | 1.24 | 31 | 0.00 |
| solo | FixedLarge | 64.08 | 64.08 | 64.08 | 0.00 | 32.06 | 0.04 | 1 | 0.00 |
| solo | Progressive | 64.32 | 64.32 | 64.32 | 0.00 | 32.06 | 0.20 | 5 | 0.00 |
| solo | SoftDeadline | 64.32 | 64.32 | 64.32 | 0.00 | 32.06 | 0.20 | 5 | 0.00 |
| deadline-pressure | Coarse | 544.48 | 512.24 | 523.46 | 524.48 | 64.02 | 0.00 | 0 | 522.46 |
| deadline-pressure | FixedSmall | 559.36 | 88.26 | 557.30 | 115.00 | 2.06 | 9.92 | 248 | 112.98 |
| deadline-pressure | FixedLarge | 544.96 | 512.72 | 523.94 | 524.96 | 32.06 | 0.32 | 8 | 522.94 |
| deadline-pressure | Progressive | 559.36 | 88.26 | 557.30 | 115.00 | 2.06 | 9.92 | 248 | 112.98 |
| deadline-pressure | SoftDeadline | 559.36 | 32.72 | 557.30 | 36.80 | 2.06 | 9.92 | 248 | 71.18 |

Mixed-work useful CPU/read/write/extract activity and largest useful idle gaps:

| Policy | CPU / read / write / extract useful % | CPU / read / write / extract held % | CPU / read / write / extract max idle gap ms | Parent-finish to child-finish ms |
| --- | --- | --- | --- | ---: |
| Coarse | 64.6 / 70.5 / 82.6 / 58.8 | 100.0 / 94.1 / 94.4 / 94.1 | 24.02 / 40.32 / 30.30 / 40.32 | 450.16 |
| FixedSmall | 71.0 / 77.4 / 90.7 / 64.5 | 73.0 / 79.7 / 93.4 / 66.4 | 32.96 / 32.96 / 8.30 / 34.76 | 2.02 |
| FixedLarge | 64.6 / 70.5 / 82.6 / 58.7 | 100.0 / 94.1 / 94.4 / 94.1 | 24.02 / 40.32 / 30.30 / 40.32 | 418.56 |
| Progressive | 71.0 / 77.4 / 90.7 / 64.5 | 73.0 / 79.7 / 93.4 / 66.4 | 32.96 / 32.96 / 8.30 / 34.76 | 2.02 |
| SoftDeadline | 71.0 / 77.4 / 90.7 / 64.5 | 73.0 / 79.7 / 93.4 / 66.4 | 32.96 / 32.96 / 8.30 / 34.76 | 2.02 |

The raw rows also report network/blocking activity, all-channel idle gaps, dispatch count, scheduler cost, background p95, deadline misses, and selection host time. Every model run has zero checked capacity violations and every graph node finishes without allocating continuation nodes.

- Mixed-work fixed-small reduces foreground p95 from 509.18 to 3.96 ms and whole-node p95 from 512.16 to 494.04 ms. Useful write activity rises from 82.65% to 90.71%; maximum held run falls from 64.02 to 2.06 ms. This benefit depends on phase resource narrowing after each quantum.
- Fixed-large halves maximum held time but retains mixed bundles across phase boundaries. It gives no mixed-work latency/throughput win here.
- Progressive equals fixed-small whenever the model stays contended. In solo work it grows, but command time 64.32 ms is still above fixed-large 64.08 and coarse 64.02 ms. This implementation does not demonstrate a reason to prefer adaptive complexity over an appropriate fixed bound.
- Throughput-only fixed-small/progressive take 1,054.08 instead of 1,024.32 ms, and node p50 nearly doubles. Shorter re-arbitration gaps alone do not establish better end-to-end latency.
- Expensive resume increases mixed command time from 544.48 to 727.88 ms with small/progressive quanta, despite good foreground p95. The 248 requeues cost 248 ms before any durable checkpoint/fsync cost.
- In deadline-pressure work, soft-deadline foreground p95 falls from progressive's 115.00 to 36.80 ms and maximum ready wait from 112.98 to 71.18 ms. Command time and whole-node p95 stay unchanged. It misses 15/16 SLOs versus 16/16; offered foreground service already exceeds one CPU's arrival-rate capacity. Urgency cannot make this load feasible.
- Tiny work produces no continuation and all five policies match. This prevents policy machinery from claiming a granularity benefit where none exists.

Real warm-cache single-file results follow. p95 uses nearest rank over seven command samples, so it is the sample maximum. Max non-arbitration is the largest admitted interval across all repeats. Scheduler and resume columns are median per-command totals, not predictions or full production overhead.

| Pilot policy | Command p50 ms | Command p95 ms | Max non-arbitration ms | Scheduler ms | Resume setup ms | Dispatches / requeues | Median MiB/s |
| --- | ---: | ---: | ---: | ---: | ---: | --- | ---: |
| Coarse | 23.930 | 26.876 | 26.871 | 0.015 | 0.004 | 1 / 0 | 668.6 |
| FixedSmall | 30.170 | 35.228 | 2.463 | 0.653 | 0.248 | 65 / 64 | 530.3 |
| FixedLarge | 25.405 | 28.623 | 9.999 | 0.064 | 0.026 | 5 / 4 | 629.8 |
| Progressive | 24.600 | 25.640 | 6.888 | 0.108 | 0.037 | 8 / 7 | 650.4 |
| SoftDeadline | 24.717 | 25.077 | 6.688 | 0.091 | 0.036 | 8 / 7 | 647.3 |

Canonical production verification, with its existing coordinator and SIMD batch path:

| Files / total bytes | Command p50 ms | Command p95 ms | Median MiB/s |
| --- | ---: | ---: | ---: |
| 1 / 16 MiB | 24.662 | 26.362 | 648.8 |
| 4 / 64 MiB | 1016.654 | 1048.919 | 63.0 |

The small-quantum single-file pilot adds visible wall time (30.170 vs coarse 23.930 ms median) while shortening held intervals. Progressive's 24.600 ms median has no clear advantage over coarse and does not exercise arbitration with competing nodes. SoftDeadline's similar timing is an uncontended control, not evidence of its policy benefit.

The four-file canonical batch is much slower per byte on this VM/profile than the single-file path. This is recorded rather than hidden or used to justify replacing the batch. Its cause, optimized release behavior, Windows behavior, and an equivalent resumable multi-file SIMD candidate are unverified. Comparing a single pilot file directly against four batch files would confound workload size and batching. This baseline needs profiling before a production verify change.

## Verification and limits

All Cargo commands used `mise exec -- cargo` with the existing mr-boxington wrapper and managed target. The non-login task shell required PATH to include `/workspace/.local/bin`, `/workspace/.local/share/mbx/bin`, and `/workspace/.cargo/bin`, with the provisioned `CARGO_HOME`, `RUSTUP_HOME`, XDG cache/data paths restored to `/workspace`. Existing global mise wrapper config was read and trusted; no repository build configuration changed. `mbx doctor` finished with zero failures and zero warnings.

| Actual command | Result |
| --- | --- |
| `mise exec -- cargo test -p griffr-runtime --lib task_pool -- --test-threads=2` on baseline | 134 passed |
| `mise exec -- cargo test -p griffr-runtime --lib work_unit_research -- --test-threads=1` during iteration | First four checks passed; the final dependency and volume/path/storage checks also passed in workspace tests |
| `mise exec -- cargo fmt --all -- --check` | Passed |
| `mise exec -- cargo check --workspace --all-targets` | Passed |
| `mise exec -- cargo clippy --workspace --all-targets -- -D warnings` | Passed |
| `mise exec -- cargo test --workspace` | 425 passed, 0 failed, 12 ignored across unit, integration and doc tests; includes all six new non-ignored research checks |
| `mise exec -- cargo test -p griffr-runtime --profile ci --lib work_unit -- --ignored --nocapture --test-threads=1` | Both evidence tests passed; final isolated rerun supplied the raw rows above |
| `python scripts/check_repo.py .` | Passed |
| `python -m unittest discover -s scripts/tests -v` | 60 passed |
| `python -m compileall -q scripts` | Passed |
| `git diff --check` | Passed |

Ruff was unavailable. The first real evidence run overlapped compilation; it is not used in these tables. The final evidence run followed successful workspace check, clippy, and tests with no other task build running. Rust 1.97.1, Windows/IOCP, release profile, cold/slow disks, HTTP/CDN, archive codec/HDiff resume, real install/update/repair lifecycle, durable checkpoints, cancellation/panic/retry, multi-volume load, unbounded arrivals, and production SLO feasibility remain unverified. Existing fixture-based CLI E2E and download/archive/patch tests passed as part of the workspace suite; no live production payload lane was invoked.

## Recommendation for maintainer review

**NO-GO for enabling this adaptive/soft-deadline policy in production in this slice.** Retain the reproducible research harness and safe read/hash pilot as review evidence. This does not close Issue #1 or decide final acceptance.

The evidence supports investigating bounded safe boundaries for mixed work, but not selecting one universal scheduler winner. Fixed-small improves the constructed latency case and loses throughput when resume is costly. The tested progressive rule collapses to fixed-small under pressure and does not beat fixed-large when alone. Soft deadlines help a deliberately overloaded model but miss most SLOs, with no real competing-workload measurement.

A next bounded slice, if accepted, should profile the canonical Windows/release MD5 batch and compare a resumable batch that preserves SIMD against both coarse and fixed bounds. Set workload-specific stop/go thresholds before running it: a maximum admitted interval and foreground p95 target, an allowed throughput/overhead regression, background wait/fairness limits, and zero capacity/path/storage violations. Do not expand into download/ZIP/HDiff resumability until their state and resource lifetimes have safe release boundaries. No scheduler defaults should change based only on this model.
