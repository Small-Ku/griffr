# Why compio Instead of tokio

This project is a Windows-only game installer focused almost entirely on large-file I/O: multi-gigabyte downloads over HTTP, disk writes, MD5 checks, zip extraction, and hardlinking.

---

## 1. IOCP Integration

Windows asynchronous I/O is built on **I/O Completion Ports (IOCP)**. A proper IOCP implementation submits I/O requests to the kernel and yields until the kernel completes them, avoiding readiness polling or unnecessary userspace copies.

*   **compio:** Exposes IOCP directly. All download, extraction, and write tasks in the task pool are native `OVERLAPPED` kernel operations.
*   **tokio:** Relies on `mio`, wrapping IOCP behind an epoll-style readiness layer. Additionally, `tokio::fs` delegates file operations to blocking thread pools (`spawn_blocking`) instead of using native IOCP.

---

## 2. Zero-Copy Buffer Model

IOCP requires that buffers remain pinned and owned by the kernel for the duration of the request.
*   `compio` enforces this via the `IoBuf` trait: passing a buffer transfers ownership to the operation until the kernel returns it.
*   `cyper` (compio's HTTP client) yields network chunks as `Bytes` objects which implement `IoBuf`. These are streamed directly to disk writes without intermediate userspace memory copies.

---

## 3. HTTP/3 Support

`cyper` natively supports HTTP/3 via `compio-quic` and handles automatic upgrading via `Alt-Svc` headers. This offers stable throughput improvements on lossy networks compared to experimental implementations.

---

## 4. Execution Flow Model

Because I/O is explicitly split from CPU tasks (MD5, extraction):
*   All I/O runs on a single-thread `compio` completion event loop.
*   CPU tasks run on compio's blocking thread pool.
*   This avoids work-stealing overhead and fits the task pool design naturally.
*   `cyper::Client` is `!Send` due to raw pointer internals, which is fine since futures never migrate between threads.

---

## 5. Summary

| Metric | compio + cyper | tokio + reqwest |
|---|---|---|
| Windows I/O (Network/Disk) | Native IOCP | mio wrapper / thread pool |
| File Write Buffer Copy | Zero-copy (`IoBuf`) | Userspace copy |
| HTTP/3 support | Native (`compio-quic`) | Experimental |
| Executor model | Single-thread completion | Multi-thread work-stealing |
| Shared client state | Single-thread scoped | `Send` / `Arc` thread-safe |
