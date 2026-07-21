# Project Wording

Use this guide for source names, public APIs, CLI text, logs, comments, tests, and project documents.
The aim is to follow ASD-STE100 principles where they fit software source code. This is not a claim of formal ASD-STE100 certification.

## 1. Main Rules

1. Use one term for one meaning.
2. Use short, direct verbs.
3. Use active voice when the actor is useful.
4. Keep one main action in each sentence.
5. Put conditions before the action when this prevents ambiguity.
6. Name files and functions by the data, action, or result that they contain.
7. Keep protocol fields, operating-system API names, library API names, and third-party format names unchanged.

## 2. Preferred Terms

| Do not use | Use a direct term |
|---|---|
| materialization | write, create, copy, save, build |
| characterization | format check, description, measurement |
| bootstrap | set up, initialize, load, start |
| preflight | check, scan, prepare |
| pipeline | steps, flow, queue, data path |
| topology | layout, graph, links, dependency order |
| finalization | finish, save, close, commit |
| utilize | use |
| commence | start, begin |
| in order to | to |
| prior to | before |
| operation, operational | task, step, work, action |
| execute, execution, executable | run, start, runnable |
| perform | check, save, write, do, run |
| obtain | get, read, receive |
| terminate | stop, end, cancel |

Choose the direct term for the real action or object. Do not replace one abstract noun with another abstract noun.

## 3. Names

Use a verb and an object for functions:

- `read_archive_index`
- `check_patch_archives`
- `save_archive_volumes`
- `setup_persistent_vfs`
- `stop_game`

Use a concrete noun for types and files:

- `ArchiveIndex`
- `PatchPlan`
- `space_use.rs`
- `archive_index.rs`
- `save_volumes.rs`

Avoid broad file names such as `models.rs`, `operations.rs`, and `workflow.rs` when a specific name is available.

## 4. Comments and Messages

State what the code does. Do not repeat the function name without more useful information.

Prefer:

> Save each verified range as an archive volume.

Avoid:

> Perform finalization of the materialized archive ranges.

For errors, state the failed action and the affected object:

> Failed to save archive volume 12.

## 5. Exceptions

Keep an external name when changing it would make the source inaccurate or incompatible. Examples include:

- JSON keys such as `get_latest_game_rsp`
- Windows functions such as `TerminateProcess`
- Rust library methods such as `Digest::finalize`
- file-format terms such as ZIP central directory and MD5

When an external name is not clear, explain it once with direct project wording.

## 6. Automatic Check

`scripts/rust_check.py` reports restricted terms as `WRD001` and broad file names as `WRD002`.
The check covers Rust, Python, PowerShell, Markdown, TOML, YAML, and text files.
This guide is the only glossary exception because it must show the terms that it restricts.
