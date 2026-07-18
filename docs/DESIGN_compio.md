# Why compio Instead of tokio

`griffr` is a Windows-first game installer whose hot path is large-file network and storage I/O: split-package downloads, resumable writes, MD5 verification, extraction, patching, and hardlink-based reuse.

---

## 1. Native Completion I/O

Windows asynchronous I/O is completion based. `compio` maps this model directly and lets Griffr submit futures to a threaded `Dispatcher` processing completions concurrently.

Instead of custom worker pools, the Dispatcher acts as the single execution gateway:
*   `dispatch()`: asynchronous HTTP, compio file I/O, and metadata operations.
*   `dispatch_blocking()`: synchronous hashing, ZIP extraction, and HDiff patching.

---

## 2. Owned Buffer Model

Completion I/O requires a buffer to remain valid until the kernel completes the operation. compio's `IoBuf` model transfers buffer ownership into an operation and returns it with the completion result.

The download path receives `cyper` body chunks as `Bytes`, updates MD5 from the same chunk, then transfers the chunk directly to positional compio writes. There is no intermediate staging `Vec` on the download/write hot path.

---

## 3. Dispatcher Threads vs. Admission Limits

The `Dispatcher` execution threads do not dictate admission concurrency:
*   `dispatcher_threads` controls the underlying compio runtimes processing completions.
*   `network_slots`, `cpu_slots`, `blocking_slots`, and volume policies control coordinator-level admission.

This separates OS thread allocation from logical task limits.

---

## 4. Shared Blocking Pool

`Dispatcher::dispatch_blocking()` runs blocking tasks on compio's shared `AsyncifyPool`. To protect the pool:
*   `cpu_slots` and `blocking_slots` throttle admitted tasks.
*   `blocking_pool_limit` configures the shared pool capacity (retaining a default reserve of 4).
*   Transient dispatch rejections restore the task to the coordinator queue rather than stalling other work.

---

## 5. Runtime-Local HTTP Clients

Each Dispatcher thread lazily owns a thread-local `cyper::Client`. Transfer futures reuse this client's connection pool, avoiding cross-thread migration overhead.

Protocol and transport negotiation are controlled by the client, while network admission remains with the coordinator.

---

## 6. Why Not a Tokio-Centric Runtime

A Tokio implementation would still need the resource-aware coordinator. Griffr's bottleneck constraints are volume contention, root mutation, and archive DAG ordering rather than generic work stealing.

Using compio keeps the Windows completion-I/O path explicit. The `Dispatcher` handles concurrent async work and synchronous execution, eliminating custom scheduler/worker loops.

---

## 7. Summary

| Concern | Current design |
|---|---|
| Async network and file operations | `Dispatcher::dispatch()` |
| CPU hashing and synchronous libraries | `Dispatcher::dispatch_blocking()` |
| Completion ownership | one coordinator completion channel |
| HTTP client lifetime | one lazy client per Dispatcher runtime thread |
| Concurrency control | coordinator admission permits, not worker counts |
| Blocking-pool protection | CPU/blocking admission plus internal headroom |
| Archive parallelism | scheduler-visible shard tasks |
| Backpressure | admission waits for every required resource permit |
