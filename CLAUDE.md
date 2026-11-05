# CLAUDE.md — libsql-python

Python SDK for [libSQL](https://turso.tech/libsql) (Turso). A PyO3/maturin native extension exposing a `sqlite3`-compatible DB-API. Published to PyPI as **`libsql`** (Cargo crate name: `pylibsql`, Rust lib/module name: `libsql`).

## Layout

- `src/lib.rs` — the entire extension (~780 lines): `connect()`, `Connection`, `Cursor`, helpers, `#[pymodule] libsql`.
- `build.rs` — only calls `pyo3_build_config::use_pyo3_cfgs()` (needed for the `Py_3_12` cfg).
- `tests/test_suite.py` — pytest suite; most tests are parametrized over `["libsql", "sqlite"]` to assert behavioral parity with stdlib `sqlite3`.
- `docs/api.md` — hand-written API reference (partly stale, e.g. says `executescript` is "Unimplemented" though it exists).
- `examples/` — per-feature demos (local, memory, remote, sync, batch, transaction, vector, encryption, sqlalchemy), each with a README. `example.py`, `perf-libsql.py`, `perf-sqlite3.py` live in the root.
- `RUST.md` — Rust/PyO3 best practices. **These are repo rules: follow them** (std `OnceLock`/`LazyLock` over `lazy_static`, `Mutex`/`RwLock` over `unsafe` Send/Sync, `Python::attach`/`detach`, `Bound`/`Py<PyAny>`, `IntoPyObject`, `if cfg!()` over `#[cfg]` where both branches can compile, explicit crate features, up-to-date maturin).
- `.github/workflows/CI.yml` — single workflow: builds wheels for linux (x86_64, i686, aarch64, armv7), macOS (x86_64, arm64) and Windows (x86_64, i686, arm64) × CPython 3.10–3.15 (Windows ARM64 3.11+, no earlier CPython builds exist) incl. free-threaded (`3.14t`, `3.15t`) + sdist; installs the built wheels and runs `tests/test_suite.py` on a native subset; tags `v*` publish to PyPI (token `PYPI_API_TOKEN` if set, otherwise trusted publishing, environment `pypi`). Runs on PRs, `main`, tags and `workflow_dispatch` (dispatch builds but never publishes). Modeled on `New-Elysium/thumbleweed`'s wheel workflow; keep action majors (`checkout@v6`, `setup-python@v6`, `upload-artifact@v7`, `download-artifact@v8`) and `MATURIN_VERSION` current.

## Commands

```bash
python3 -m venv .env && source .env/bin/activate   # maturin develop requires a virtualenv
pip install -U maturin pytest pyperf   # maturin >= 1.9.4 (PyO3 0.30 has no `extension-module` feature)
maturin develop            # build + install extension into the venv (rebuild after every src/ change)
pytest                     # run tests/test_suite.py
pytest tests/test_suite.py -k test_execute   # single test
python3 perf-libsql.py     # benchmarks (compare with perf-sqlite3.py)
```

`shell.nix` provides a FHS env with python312, maturin, pytest, pyperf. Tests never import from source; they test the installed build, so run `maturin develop` first or you'll test stale code.

Remote tests (provider `"libsql-remote"`) need an `sqld` server on `http://localhost:8080` and fail with "libsql-remote server is not running" otherwise.

## Key dependencies (Cargo.toml)

- `pyo3 0.30.1`, Rust edition **2024** (MSRV 1.85), `crate-type = ["cdylib"]`. Keep `pyo3` and `pyo3-build-config` on the same version. No `extension-module` feature: maturin sets `PYO3_BUILD_EXTENSION_MODULE`.
- `libsql 0.10.0` with the `encryption` feature (the default features — core, replication, remote, sync, tls — are all used); `tokio` with explicit features `rt-multi-thread`, `time`, `macros` (all used directly; don't rely on libsql's feature unification); `tracing-subscriber` (initialized at import via `try_init`).
- Version must be kept in sync in **both** `Cargo.toml` and `pyproject.toml` (currently 0.1.11). Dependency bumps and releases are separate commits in history ("Update libsql dependency", "0.1.x").
- `requires-python >=3.10` (the project minimum; CI builds 3.10–3.15, no 3.13t since PyO3 0.30 dropped it).
- `cargo audit` reports advisories in libsql 0.10.0's pinned transitive deps (`rustls 0.22`/`rustls-webpki 0.102`, `h2 0.3`, `bincode 1`, `rustls-pemfile`): they come from `hyper 0.14`/`tonic 0.11` inside libsql and can't be bumped from here — they need an upstream libsql release. Re-run `cargo audit` after each libsql bump.

## Architecture / how things work

- **Sync API over async core.** A global single-worker tokio runtime (`rt()`, `OnceLock<Runtime>`, `worker_threads(1)`) drives libsql's async API; blocking calls go through `block_on_detached` (releases the interpreter via `py.detach`, so other Python threads run) or `block_on_interruptible` (same, plus re-attaching every 300 ms to check signals). Anything passed into those must be `Send`: convert Python objects (parameters, rows) to plain Rust values *before* detaching and back *after*.
- **`block_on_interruptible`** is used for `connect` (synced/local builder) and `sync`; Ctrl-C raises `KeyboardInterrupt` there. Execute/fetch use `block_on_detached` and are not interruptible.
- **`connect()`** has two `#[cfg]` variants: `Py_3_12` adds the `autocommit` kwarg (default `LEGACY_TRANSACTION_CONTROL = -1`, accepts -1/0/1). Both delegate to `_connect_core`. **If you change the signature, update both variants.**
  - Remote path (`libsql://`, `http://`, `https://`) → `Database::open_remote_internal`.
  - `sync_url` set → embedded replica via `Builder::new_synced_database` (`sync_interval`, `offline` → `remote_writes(!offline)`); `encryption_key` is rejected here.
  - Otherwise local file/`:memory:` via `Builder::new_local`, optional `EncryptionConfig` (default cipher).
  - Underscore-prefixed args (`_check_same_thread`, `_uri`) are accepted for sqlite3 compat and ignored. `timeout` → `busy_timeout`.
- **`ConnectionGuard`** wraps `libsql::Connection` and enters the tokio runtime in `Drop` (hrana needs a runtime to spawn its close call). `Connection` and `Cursor` share it via `Arc`; closing a connection sets its `Mutex<Option<Arc<..>>>` to `None`.
- **`Connection`/`Cursor`** are `#[pyclass]` types with `Mutex`/atomic interior state (free-threading safe, no `unsafe impl`); use the `lock()` helper (ignores poisoning). Don't hold a `MutexGuard` across `py.detach` — lock inside the detached closure (guards aren't `Send`). A closed connection/cursor raises `ProgrammingError("Connection already closed")` (no panics).
- **Transactions** emulate `sqlite3` legacy behavior in `execute()`: if not autocommit (`TxnSettings::is_autocommit`) and the statement is INSERT/UPDATE/DELETE (`stmt_is_dml`, prefix match on trimmed uppercased SQL) and the connection is in autocommit state, an implicit `BEGIN <isolation_level>` is issued (`BEGIN IMMEDIATE` takes the write lock up front, which helps with libSQL's single writer). `isolation_level` and `autocommit` live in an `Arc<TxnSettings>` shared by the connection and its cursors, so changing them on the connection applies to existing cursors. Both are writable like in `sqlite3`: `isolation_level` accepts None/''/DEFERRED/IMMEDIATE/EXCLUSIVE (case-insensitive, stored uppercased), and setting it to None commits first. SQLAlchemy's AUTOCOMMIT execution option depends on this. `commit()`/`rollback()` run `COMMIT`/`ROLLBACK` only if a txn is open (a TODO notes switching to libsql's transaction API). `__exit__` commits on clean exit, rolls back on exception, never swallows exceptions.
- **`execute()` flow:** convert params (attached) → detached `run_statement`: optional implicit `BEGIN`, `prepare`, then `query()` (statement has columns → store `Rows`) or `execute()` → reset `done`, accumulate `rowcount` (+= `changes()`) → store statement/rows under a tokio context (their drop may need it).
- **Parameter conversion order:** None → `i64` → `f64` → `str` → `bytes`. Only positional (list/tuple) params; no named/dict params, no `bool`/`datetime`/adapter support.
- **Fetching:** all `fetch*` go through `Cursor::fetch_values`, which reads plain `Value`s detached and honors a `done` flag, because libsql's `Rows::next()` restarts iteration after exhaustion; keep that guard when touching fetch logic. `done` is reset on every `execute`. `description` returns 7-tuples with only the name filled in.
- **Free-threading:** the module keeps PyO3's default (`gil_used = false`). Don't flip it to `true`: on 3.14t that re-enables the GIL for the whole process at import (with a RuntimeWarning) just because libsql was imported. libSQL's one-writer limit is about database concurrency, not interpreter concurrency; the classes are already `Sync` via Mutex/atomics.
- Module exports: `connect`, `Connection`, `Cursor`, `Error`, `LEGACY_TRANSACTION_CONTROL`, `paramstyle="qmark"`, hard-coded `sqlite_version_info=(3,42,0)`.

## Known quirks / gotchas (verify before relying on them)

- Errors follow the DB-API/`sqlite3` hierarchy (`Error` → `DatabaseError` → `OperationalError`/`IntegrityError`/`ProgrammingError`/`DataError`/`InternalError`/`NotSupportedError`; `InterfaceError`), mapped in `to_py_err` from SQLite result codes and libsql error variants. Remote/Hrana/replication failures are `OperationalError` (so SQLAlchemy can detect dead Hrana streams). `Error` deliberately subclasses `ValueError` so old `except ValueError` code keeps working. Argument validation errors are still plain `ValueError`.
- `Connection.executescript` discards errors (`let _ = ...`); `Cursor.executescript` propagates them.
- `executemany` requires a list of parameters (raises `ValueError` otherwise) and runs statements one by one.
- `docs/api.md` and README lag behind the code; update docs when adding user-visible behavior.

## Conventions

- Keep the DB-API surface mirroring stdlib `sqlite3`; when adding behavior, add a test parametrized over `["libsql", "sqlite"]` so parity is checked.
- Run `cargo fmt` and `cargo clippy` before committing (no repo config; defaults are used).
- Don't commit `*.db`, `client_wal_index`, `target/`, `.env`, venvs (see `.gitignore`).
- Don't push to or release from `main` by hand: releases are tags `v*` handled by CI with `PYPI_API_TOKEN`.
