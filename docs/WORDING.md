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
8. Apply the same wording rules to segments in `snake_case`, `PascalCase`, and `camelCase` names.

## 2. Preferred Terms

| Do not use | Use a direct term |
|---|---|
| materialization | write, create, copy, save, build |
| characterization | format check, description, measurement |
| bootstrap | set up, load, start |
| preflight | check, scan, prepare |
| pipeline | steps, flow, queue, data path |
| topology | layout, graph, links, dependency order |
| finalization | finish, save, close, commit |
| complete, completion, completed, incomplete | finish, done, full, ready, missing, partial, unfinished |
| transaction | batch, patch, step, change, group |
| fixture | sample, test data, test setup |
| initial, initialize, initialization | first, start, base, root, set up, start value |
| utilize | use |
| commence | start, begin |
| in order to | to |
| prior to | before |
| operation, operational | task, step, work, action |
| execute, execution, executable | run, start, runnable, program file |
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

## 4. Short, Simple, and Direct Paragraphs

Keep documentation, comments, and messages short, simple, and concise. Break long explanations into short bullet points. Use direct active voice (state the subject and the direct action verb).

### Phrasing Examples

| Avoid (Passive / Wordy) | Prefer (Short / Direct Active Voice) |
|---|---|
| `CollectCommitJobs` is performed by the `CommitArchive` task. | The `CommitArchive` task collects commit jobs. |
| Successful checks are carried into the integrity pass. | The integrity pass receives successful checks. |
| This implementation is split into six code stages. | Six code stages make up this design. |
| Task runs are wrapped with `catch_unwind` in order to handle panics. | The runner wraps task runs with `catch_unwind` to handle panics. |
| Perform finalization of the materialized archive ranges. | Save each verified range as an archive volume. |

For errors, state the failed action and the affected object:

> Failed to save archive volume 12.

## 5. Exceptions

Keep an external name when changing it would make the source inaccurate or incompatible. Examples include:

- JSON keys such as `get_latest_game_rsp`
- Windows functions such as `TerminateProcess`
- Rust library methods such as `Digest::finalize`
- Windows terms such as I/O completion port (IOCP)
- protocol names such as `index_initial`, `pref_initial`, and the resource-group value `initial`
- file-format terms such as ZIP central directory and MD5

When an external name is not clear, explain it once with direct project wording.

## 6. Automatic Check

`scripts/rust_check.py` reports restricted terms as `WRD001` and broad file names as `WRD002`.
The check covers Rust, Python, PowerShell, Markdown, TOML, YAML, and text files. It checks file and directory names, plain text, and name segments such as `completed_tasks`, `TaskCompletion`, and `TaskInitializer`.

The Rust pass uses Tree-sitter after the checker builds the crate module graph and its local name index. It checks project definitions such as items, methods, fields, variants, parameters, local bindings, generic parameters, and explicit import aliases. It does not scan all Rust identifiers as plain text. Thus, a call to an external method such as `Digest::finalize` does not become a project wording fault.

The module graph resolves `mod`, `#[path]`, and literal `include!` declarations to source files. The local name resolver can also map direct project paths to the `Symbol` and source file that define them. It cannot fully resolve macro output, inferred method calls, trait dispatch, build-script output, or all `cfg` combinations. Cargo and rust-analyzer remain the final tools for those cases.

The checker still scans Rust comments, documentation, and string literals as text. It also uses text checks for non-Rust files because those locations do not have Rust symbol meaning.
The guide and checker source are glossary exceptions because they must contain the terms that they restrict. Fixed external names listed in section 5 are also allowed.
