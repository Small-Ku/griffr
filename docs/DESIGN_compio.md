# Why compio Instead of tokio

## Context

This project is a third-party game installer for Windows. Its workload is
almost entirely large-file I/O: downloading multi-gigabyte assets over HTTP,
writing them to disk, verifying MD5 checksums, extracting zip archives, and
creating hard links to deduplicate files shared across server installations.
Windows is the primary — and for the foreseeable future, only — target
platform.

---

## The Core Reason: IOCP Is the Right Primitive for This Workload

Windows async I/O is built on I/O Completion Ports (IOCP). A correctly written
IOCP application submits an operation (e.g. `ReadFile` with an `OVERLAPPED`
struct), yields, and is woken only when the kernel signals completion. No
polling, no readiness notifications, no userspace copies.

**compio exposes IOCP directly.** Every `Download`, `Hardlink`, and file write
in the task pool is a native `OVERLAPPED` operation. The runtime calls
`GetQueuedCompletionStatus` and wakes the corresponding future — nothing more.

**tokio does not.** On Windows, tokio uses `mio`, which wraps IOCP behind an
epoll-style readiness API. This adds a translation layer that exists solely to
paper over the difference between Linux and Windows I/O models. For a
Windows-only application there is no reason to pay that cost. Additionally,
`tokio::fs` (file I/O) does not use IOCP at all — it offloads every file
operation to a `spawn_blocking` thread pool. In a workload where disk writes
are on the critical path of every downloaded file, this matters.

---

## Buffer Ownership: Zero-Copy on the Hot Path

IOCP completion model requires the buffer to be owned and pinned for the
duration of the kernel operation. compio enforces this through the `IoBuf`
trait: passing a buffer to `write_all` transfers ownership to the operation;
the future resolves only after the kernel signals completion, at which point
ownership returns to the caller.

`cyper` (compio's HTTP client) yields `Bytes` per chunk. `Bytes` implements
`IoBuf`, so the chunk received from the network can be passed directly to the
disk write — no intermediate copy, no `Vec` allocation. Over a 70 GB download
this zero-copy path is non-trivial.

tokio's readiness model cannot offer this: the kernel signals that the socket
is readable, then userspace performs the read into a buffer it owns. The data
passes through userspace regardless.

---

## HTTP/3 Support

`cyper` supports HTTP/3 natively via `compio-quic`, including automatic
upgrade on `Alt-Svc` response headers. Game CDNs increasingly support QUIC;
on high-latency or moderately lossy connections HTTP/3's lack of
head-of-line blocking can meaningfully improve throughput for large assets.

`reqwest`'s HTTP/3 support is experimental and not recommended for production
use. For a project where download reliability is a primary user-facing concern,
having a stable HTTP/3 path is a concrete advantage.

---

## Single-Thread Runtime Matches the Task Pool Model

compio runs a single-thread executor driven by a completion queue. The task
pool design in this project maps cleanly onto this model:

- All I/O tasks (Download, Hardlink, file writes) run on the compio executor.
- All CPU tasks (MD5, Extract) run on a rayon thread pool, bridged via channel.
- There is no work-stealing, no cross-thread task migration, no need for
  `Arc<Client>` across threads.

tokio's multi-thread work-stealing scheduler is the right tool when tasks are
heterogeneous and need to be distributed across cores. Here, I/O tasks are
explicitly separated from CPU tasks by design; the added complexity of
work-stealing provides no benefit and introduces the `Send` requirement on all
futures, which conflicts with `cyper::Client` (see below).

---

## `cyper::Client` Is `!Send` — and That Is Fine Here

`cyper::Client` does not implement `Send` due to `*mut ()` internals in
`cyper-core`'s stream type. This is not a problem for this project because:

1. The compio executor is single-threaded; futures never migrate between threads.
2. The task pool holds one `Client` per executor, not shared across threads.
3. CPU-bound work (MD5, Extract) is already explicitly moved to rayon and does
   not touch the `Client`.

If the architecture required `Arc<Client>` across multiple OS threads, `cyper`
would be the wrong choice and `reqwest` the right one. That is not this
architecture.

---

## Summary

| Concern | compio + cyper | tokio + reqwest |
|---|---|---|
| Windows IOCP (network) | Native | Wrapped via wepoll/mio |
| Windows IOCP (disk) | Native | `spawn_blocking` thread pool |
| Buffer copies on write path | Zero-copy (`IoBuf`) | Userspace copy after readiness |
| HTTP/3 | Stable (`compio-quic`) | Experimental |
| Single-thread executor fit | Natural | Over-engineered for this use case |
| `Arc<Client>` across threads | Not needed; `!Send` is fine | Supported |
| Ecosystem maturity | Smaller, active | Large, stable |

The ecosystem maturity gap is real. compio and cyper are younger projects with
fewer examples and a smaller community. This is an accepted trade-off: the
workload is Windows-native large-file I/O, which is precisely the case compio
was designed for, and the HTTP feature set required (TLS, HTTP/2, HTTP/3,
streaming response body) is fully covered by cyper.
