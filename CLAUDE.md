# CLAUDE.md — libsql-python

Python SDK for [libSQL](https://turso.tech/libsql) (Turso). A PyO3/maturin native extension exposing a `sqlite3`-compatible DB-API. Published to PyPI as **`libsql`** (Cargo crate name: `pylibsql`, Rust lib/module name: `libsql`).

## Layout

- `src/lib.rs` — the entire extension (~780 lines): `connect()`, `Connection`, `Cursor`, helpers, `#[pymodule] libsql`.
- `build.rs` — only calls `pyo3_build_config::use_pyo3_cfgs()` (needed for the `Py_3_12` cfg).
- `tests/test_suite.py` — pytest suite; most tests are parametrized over `["libsql", "sqlite"]` to assert behavioral parity with stdlib `sqlite3`.
- `docs/api.md` — hand-written API reference (partly stale, e.g. says `executescript` is "Unimplemented" though it exists).
- `examples/` — per-feature demos (local, memory, remote, sync, batch, transaction, vector, encryption, sqlalchemy), each with a README. `example.py`, `perf-libsql.py`, `perf-sqlite3.py` live in the root.
- `RUST.md` — a general Rust/PyO3 tips article by a contributor. Treat it as background reading, not project policy: it describes newer PyO3 (0.30) APIs than this repo pins (0.25.1).
- `.github/workflows/CI.yml` — builds wheels (linux/macOS/windows + sdist) with maturin-action; tags `v*` publish to PyPI. `pr-tests.yml` runs `maturin develop && pytest` on PRs to `main`.

## Commands

```bash
python3 -m venv .env && source .env/bin/activate   # maturin develop requires a virtualenv
pip install maturin pytest pyperf
maturin develop            # build + install extension into the venv (rebuild after every src/ change)
pytest                     # run tests/test_suite.py
pytest tests/test_suite.py -k test_execute   # single test
python3 perf-libsql.py     # benchmarks (compare with perf-sqlite3.py)
```

`shell.nix` provides a FHS env with python312, maturin, pytest, pyperf. Tests never import from source; they test the installed build, so run `maturin develop` first or you'll test stale code.

Remote tests (provider `"libsql-remote"`) need an `sqld` server on `http://localhost:8080` and fail with "libsql-remote server is not running" otherwise.

## Key dependencies (Cargo.toml)

- `pyo3 0.25.1`, Rust edition **2024** (needs a recent toolchain), `crate-type = ["cdylib"]`.
- `libsql 0.9.22` with the `encryption` feature; `tokio` (multi-thread rt); `tracing-subscriber` (initialized at import via `try_init`).
- Version must be kept in sync in **both** `Cargo.toml` and `pyproject.toml` (currently 0.1.11). Dependency bumps and releases are separate commits in history ("Update libsql dependency", "0.1.x").
- `pyproject.toml` declares `requires-python >=3.7` but CI builds with 3.10+ and PyO3 0.25 supports ≥3.7 only nominally; verify before claiming support for old versions.

## Architecture / how things work

- **Sync API over async core.** A global single-worker tokio runtime (`rt()`, `OnceLock<Runtime>`, `worker_threads(1)`) drives libsql's async API; every call does `rt().block_on(...)`.
- **`check_signals`** wraps long futures (`connect`, `sync`) and polls `py.check_signals()` every 300 ms so Ctrl-C works. Other `block_on` calls (execute/fetch) are not interruptible.
- **`connect()`** has two `#[cfg]` variants: `Py_3_12` adds the `autocommit` kwarg (default `LEGACY_TRANSACTION_CONTROL = -1`, accepts -1/0/1). Both delegate to `_connect_core`. **If you change the signature, update both variants.**
  - Remote path (`libsql://`, `http://`, `https://`) → `Database::open_remote_internal`.
  - `sync_url` set → embedded replica via `Builder::new_synced_database` (`sync_interval`, `offline` → `remote_writes(!offline)`); `encryption_key` is rejected here.
  - Otherwise local file/`:memory:` via `Builder::new_local`, optional `EncryptionConfig` (default cipher).
  - Underscore-prefixed args (`_check_same_thread`, `_uri`) are accepted for sqlite3 compat and ignored. `timeout` → `busy_timeout`.
- **`ConnectionGuard`** wraps `libsql::Connection` and enters the tokio runtime in `Drop` (hrana needs a runtime to spawn its close call). `Connection` and `Cursor` share it via `Arc`; closing a connection sets its `RefCell<Option<Arc<..>>>` to `None`.
- **`Connection`/`Cursor`** are `#[pyclass]` with `RefCell` interior state and `unsafe impl Send/Sync` (justified by libsql thread safety). Be careful: `RefCell` borrows can panic if held across re-entrant calls.
- **Transactions** emulate `sqlite3` legacy behavior in `execute()`: if not autocommit (`determine_autocommit`) and the statement is INSERT/UPDATE/DELETE (`stmt_is_dml`, prefix match on trimmed uppercased SQL) and the connection is in autocommit state, an implicit `BEGIN` is issued. `commit()`/`rollback()` run `COMMIT`/`ROLLBACK` only if a txn is open (a TODO notes switching to libsql's transaction API). `__exit__` commits on clean exit, rolls back on exception, never swallows exceptions.
- **`execute()` flow:** convert params → `prepare` → if the statement has columns, `query()` and store `Rows`, else `execute()` → accumulate `rowcount` (+= `changes()`) → store the statement (used for `description`, `lastrowid`).
- **Parameter conversion order:** None → `i32` → `f64` → `str` → `bytes`. Only positional (list/tuple) params; no named/dict params, no `bool`/`datetime`/adapter support.
- **Fetching:** `fetchmany` tracks a `done` flag because libsql's `Rows::next()` restarts iteration after exhaustion; keep that guard when touching fetch logic. `fetchone` has no such guard. `description` returns 7-tuples with only the name filled in.
- Module exports: `connect`, `Connection`, `Cursor`, `Error`, `LEGACY_TRANSACTION_CONTROL`, `paramstyle="qmark"`, hard-coded `sqlite_version_info=(3,42,0)`.

## Known quirks / gotchas (verify before relying on them)

- All libsql errors are raised as `ValueError` (`to_py_err`), not the exported `libsql.Error` (which is defined but unused) and not `sqlite3`-style exception hierarchies.
- `Connection.executescript` discards errors (`let _ = ...`); `Cursor.executescript` propagates them.
- Integers are extracted as `i32` first; values outside 32-bit range fall through to the `f64` branch (precision loss for large ints) — a likely bug.
- `executemany` requires a `list` of params (`unwrap()` on `None` panics) and runs statements one by one.
- `docs/api.md` and README lag behind the code; update docs when adding user-visible behavior.

## Conventions

- Keep the DB-API surface mirroring stdlib `sqlite3`; when adding behavior, add a test parametrized over `["libsql", "sqlite"]` so parity is checked.
- Run `cargo fmt` and `cargo clippy` before committing (no repo config; defaults are used).
- Don't commit `*.db`, `client_wal_index`, `target/`, `.env`, venvs (see `.gitignore`).
- Don't push to or release from `main` by hand: releases are tags `v*` handled by CI with `PYPI_API_TOKEN`.
