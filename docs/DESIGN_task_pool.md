# Task Pool Design

## Overview

The installer's three operations — install, update, and validate — are all
different orderings of three atomic operations: `Download`, `MD5Verify`, and
`Extract`. A shared task pool with per-command initial task lists unifies all
three, and allows feedback loops (e.g. re-queuing a download on verify failure)
without separate state machines.

---

## Task Definition

```rust
enum Task {
    Download {
        url: String,
        dest: PathBuf,
        expected_md5: [u8; 16],
        retry_count: u32,
    },
    Verify {
        path: PathBuf,
        expected_md5: [u8; 16],
        on_fail: Box<Task>,   // re-queued automatically on mismatch
    },
    Extract {
        archive: PathBuf,
        dest: PathBuf,
    },
    Hardlink {
        src: PathBuf,
        dest: PathBuf,
    },
}
```

`on_fail` carries the retry task inline — no external state machine needed.
When `Verify` fails it pushes `*on_fail` back into the queue directly.

---

## Operation Decomposition

```
Install:   Download → Verify → (Extract if zip)
Update:    Verify → [Download → Verify] → (Extract if zip)
Validate:  Verify → [Download → Verify if failed]
```

All three share the same executor; only the initial task list differs.

---

## Pool Structure

Three independent slot groups, each rate-limited separately:

```
┌─────────────────────────────────────────────────┐
│                  Task Queue                     │
│   routed by task kind into dedicated queues      │
├─────────────────┬───────────────┬───────────────┤
│   I/O slots     │   CPU slots   │ Extract slots │
│ (Download,      │  (MD5Verify)  │ (zip + disk)  │
│  Hardlink,      │  = CPU cores  │  independent  │
│  file write)    │               │  throttle     │
└─────────────────┴───────────────┴───────────────┘
```

**Why separate slot groups:**
- Download tasks are network-bound; high concurrency does not pressure CPU.
- MD5Verify is purely CPU-bound; concurrency should be capped at core count.
- Extract is both CPU and disk heavy; it needs its own throttle to avoid
  starving downloads.

---

## Runtime Model

```
single shared compio Dispatcher (threaded)
    ├── worker loops: IO / CPU / Extract queues
    └── task bodies: Download, Hardlink, file writes, MD5Verify, Extract
        (I/O uses compio async ops; CPU/extract use dedicated worker slots)

async-channel / flume
    └── bridges both sides; task completion posts next task
```

The dispatcher thread count is derived from configured slot groups, with extra
I/O lanes reserved so nested dispatches do not starve when worker loops are busy.

---

## Retry Limit

Verify failure → re-queue Download → Verify again → failure is an unbounded
loop without a guard. Each `Task::Download` carries `retry_count`; increment
on re-queue and fail the session when it exceeds a threshold (e.g. 3):

```rust
if retry_count >= 3 {
    progress_tx.send(ProgressEvent::Failed { path, reason: "MD5 mismatch after retries" });
    return;
}
queue.push(Task::Download { retry_count: retry_count + 1, .. });
```

---

## Extract Atomicity

A crash mid-extraction leaves partially written files. The next Verify pass
will produce incorrect results against them.

Always extract to a temp directory, then rename into place:

```rust
let tmp = dest.with_extension("tmp");
extract_zip(&archive, &tmp).await?;
// MoveFileExW with MOVEFILE_REPLACE_EXISTING — atomic on same volume
tokio::fs::rename(&tmp, &dest).await?;
```

`rename` on the same volume is atomic on Windows; partial results are never
visible to the Verify stage.

---

## Progress Reporting

Each task, on completion or failure, sends a typed event to a shared `mpsc`
channel. The UI layer consumes this channel independently:

```rust
enum ProgressEvent {
    Downloaded { path: PathBuf, bytes: u64 },
    Verified   { path: PathBuf, ok: bool },
    Extracted  { path: PathBuf },
    Failed     { path: PathBuf, reason: String },
}
```

The pool never calls into the UI directly. This keeps the pool logic testable
in isolation.
