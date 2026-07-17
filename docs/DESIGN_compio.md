# Why compio Instead of tokio

`griffr` is a Windows-first game installer whose hot path is large-file network and storage I/O: split-package downloads, resumable writes, MD5 verification, extraction, patching, and hardlink-based reuse.

---

## 1. Native Completion I/O

Windows asynchronous I/O is completion based. `compio` maps this model directly and lets the task pool submit network and filesystem futures to a small `Dispatcher` dedicated to completion processing.

The task pool does not use the Dispatcher as its general-purpose worker pool. CPU hashing and blocking filesystem orchestration run on explicit fixed worker groups, while compio handles the asynchronous operations they submit.

---

## 2. Owned Buffer Model

Completion I/O requires a buffer to remain valid until the kernel completes the operation. compio's `IoBuf` model transfers buffer ownership into an operation and returns it with the completion result.

The download path receives `cyper` body chunks as `Bytes` and passes those chunks directly to positional compio writes. MD5 is updated from the same chunk before ownership is transferred to the write operation, avoiding an additional staging buffer on the hot path.

---

## 3. Runtime Shape

The current runtime is intentionally hybrid:

```text
TaskPool scheduler
├── fixed Network workers
├── fixed CPU workers
├── fixed Blocking workers
└── shared resource permits

compio Dispatcher (2..=4 threads)
└── asynchronous network/filesystem completions
```

Workers do not migrate tasks through a work-stealing runtime. They dequeue only tasks in their execution class after the coordinator atomically grants network, volume, extraction, and install-root permits.

Permanent queue loops are ordinary named threads rather than jobs occupying compio's blocking pool.

---

## 4. HTTP Client Lifetime

The pinned `cyper` client supports cloning and shared use. `TaskPoolRunner` creates one long-lived client and clones it into dispatched transfer futures. This lets downloads reuse client-level connection and protocol state instead of constructing a fresh client for every file.

The client is coupled to the lifetime of the task-pool runner, not to individual task objects or UI frontends.

---

## 5. HTTP Protocol Support

`cyper` is built on compio and hyper and supports the protocol features used by the installer, including streaming bodies and optional HTTP/3 support through the selected crate features.

Protocol choice remains below the task scheduler. The scheduler controls admission and storage pressure; the HTTP client controls connection reuse and transport negotiation.

---

## 6. Why Not a Tokio-Centric Runtime

A Tokio implementation would be viable, but it would not remove the need for the resource-aware coordinator. The dominant correctness and performance constraints are physical-volume contention, install-root mutation, retry dependencies, and archive DAG ordering—not generic asynchronous task migration.

The current choice keeps the Windows completion-I/O path explicit while assigning CPU and blocking work to purpose-built worker groups. The trade-off is a smaller ecosystem and the need to maintain a narrow bridge between synchronous worker orchestration and compio futures.

---

## 7. Summary

| Concern | Current design |
|---|---|
| Network and async file operations | compio completion futures |
| HTTP | one long-lived `cyper::Client` per task-pool runner |
| CPU hashing | fixed CPU workers |
| Filesystem orchestration | fixed blocking workers |
| Concurrency control | shared network, physical-volume, extraction, and mutation permits |
| Archive parallelism | scheduler-visible shard tasks |
| Backpressure | admission waits for all required permits |
