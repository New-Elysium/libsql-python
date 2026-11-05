# Rust Noobie Best Practices
*Oscar Franco — September 2025, updated November 2026 for Rust 1.99 and PyO3 0.30*

I've been learning Rust by doing, sponsored directly by clients who needed Rust code in a React Native app and Python integrations where performance actually mattered.

As many others have pointed out before me, Rust has a steep learning curve once you go beyond the basics. You might be relying on an LLM to write some code for you, but the quality of the code LLMs spit out is dubious — they often regurgitate patterns that were fine two years ago and awkward today.

I've picked up some tricks and best practices over the last year. Here's what I wish someone had told me earlier.

---

## Architecture

**Prefer stateless architectures where you can.** Shared mutable state with mutexes and `Send + Sync` bounds is the source of a disproportionate amount of Rust pain. If you can design around it, do.

---

## Concurrency

**`RwLock` is often better than `Mutex`, but not always.** `RwLock` allows multiple concurrent readers without fully blocking your process, which is a win in read-heavy workloads. However, in write-heavy workloads it can perform worse than `Mutex` due to contention and platform-specific behavior. Profile before standardizing on either.

The most common footgun with `RwLock` is forgetting to drop a read lock before acquiring a write lock in the same function:

```rust
let my_read_var = MY_VAR.read().unwrap();
// Must explicitly drop before writing, or this deadlocks
drop(my_read_var);
let mut my_write_var = MY_VAR.write().unwrap();
```

---

## Global Variables

**For most globals, use `OnceLock` and `LazyLock` from std instead of `lazy_static` or `once_cell`.** A lot of generated code still pulls in third-party crates for this because older examples dominate search results. For common cases, std is enough and avoids an extra dependency.

```rust
// ❌ Don't
lazy_static! {
    // your global variables
}

// ✅ Do
use std::sync::{LazyLock, RwLock};

static MY_STR: LazyLock<RwLock<String>> =
    LazyLock::new(|| RwLock::new("Hello World!".into()));
```

**For async one-time initialization, `tokio::sync::OnceCell` works well:**

```rust
use tokio::sync::OnceCell;

static INIT: OnceCell<()> = OnceCell::const_new();

pub async fn init() {
    INIT.get_or_init(|| async {
        my_async_function().await;
    })
    .await;
}
```

---

## Testing

**For stateful or flaky test suites, `cargo-nextest` is often better than plain `cargo test`.** The default test runner executes tests in parallel within a single process, which makes isolating state between tests a pain. `cargo-nextest` gives you stronger isolation and better execution controls.

Here is the config I use:

```toml
[profile.default]
retries = 3
fail-fast = false
status-level = "all"
```

The [`assert2`](https://crates.io/crates/assert2) crate makes test failures dramatically easier to debug by printing actual values with colors, rather than opaque assertion errors.

---

## Error Handling

**Enforce idiomatic `Result` and `Option` usage on your team.** These patterns are central to Rust's style. If teammates keep working around them, you end up with code that looks like Rust but misses what makes Rust pleasant.

**Use `?` everywhere, but annotate at boundaries.** The `?` operator is still the right default for propagation.- Use [`anyhow`](https://crates.io/crates/anyhow) for **application code** — it gives you rich context chains via `.context()`.
- Use [`thiserror`](https://crates.io/crates/thiserror) combined with `std::backtrace::Backtrace` for **library code** where you want typed errors with precise origins.

See [this thread](https://users.rust-lang.org/t/anyhow-backtraces/107437) for more detail.

---

## WASM

**WASM is painful to retrofit.** WASM is single-threaded, which means `Send + Sync` bounds, async traits, and anything that assumes multi-threading will need to be refactored or gated behind `cfg` flags. If WASM is on the roadmap at all, design for it from the start. 
Don't count on `wasm32-wasip2` (and the like) being available.

---

## FFI / C API

**The C-API memory model requires explicit deallocation round-trips.** If you're exposing a C API and returning `CString` to a foreign caller, that memory must be returned to Rust to be freed safely. The allocator boundary matters: memory allocated by Rust should be freed by Rust.

```rust
use std::ffi::CString;
use std::os::raw::c_char;

#[no_mangle]
unsafe extern "C" fn get_a_string() -> *mut c_char {
    let data = CString::new("Hello World!").unwrap();
    data.into_raw()
}

#[no_mangle]
unsafe extern "C" fn free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    let _ = CString::from_raw(ptr);
    // Dropped here, memory safely freed
}
```

---

## Cloudflare Workers (Rust / WASM)

**Use `workers-rs` (`worker` crate) for Rust Cloudflare Workers.** The crate provides the `#[event(fetch)]` macro for entry points and `#[durable_object]` for Durable Objects.

**Durable Object `fetch` takes `&self`, not `&mut self`.** The `#[durable_object]` macro generates wasm-bindgen glue that calls your trait method with `&self`. This is not negotiable — it's baked into the macro's code generation (see `worker-macros/src/durable_object.rs`). For interior mutability, use `UnsafeCell`. This is safe because the Cloudflare runtime guarantees single-threaded execution per DO instance:

```rust
use std::cell::UnsafeCell;
use worker::*;

#[durable_object]
pub struct MyCounter {
    count: UnsafeCell<u32>,
}

// Required — UnsafeCell is not Send/Sync by default.
unsafe impl Send for MyCounter {}
unsafe impl Sync for MyCounter {}

impl DurableObject for MyCounter {
    fn new(_state: State, _env: Env) -> Self {
        Self { count: UnsafeCell::new(0) }
    }

    async fn fetch(&self, _req: Request) -> Result<Response> {
        // SAFETY: DOs are single-threaded — no concurrent fetch calls.
        let count = unsafe { &mut *self.count.get() };
        *count += 1;
        Response::ok(format!("count is now {}", *count))
    }
}
```

**The `#[durable_object]` macro goes on the struct only, NOT on the impl block.** Putting it on `impl DurableObject for ...` was required in older versions but is now a compile error in `worker` 0.8+. The trait implementation is a plain `impl DurableObject for MyStruct`.

**Free-plan DOs must use `new_sqlite_classes` in migrations.** The `new_classes` migration only works on paid plans. Free-plan Workers get this error on deploy:

```
In order to use Durable Objects with a free plan, you must create a
namespace using a `new_sqlite_classes` migration.
```

Fix: use `new_sqlite_classes` in `wrangler.toml`:

```toml
[[migrations]]
tag = "v1"
new_sqlite_classes = ["RateLimiter"]  # NOT new_classes
```

You don't need to actually use the SQLite storage — the migration just registers the class under the SQLite-backed infrastructure. In-memory state via `UnsafeCell<HashMap>` works fine.

**DO instances are auto-created on first access.** You never manually instantiate a DO. `namespace.id_from_name("shard-5")?.get_stub()?` lazily creates the instance the first time it's called. The `new()` constructor runs once, then the DO stays in memory until idle eviction (~30s of no requests). Migrations only run on the first `wrangler deploy` that introduces them.

**Prefer Durable Objects over KV for counters.** KV is eventually-consistent and optimized for read-heavy workloads. Every counter increment requires a read-modify-write cycle — the worst-case access pattern for KV. Each operation can take 200–400ms due to cross-colo replication. A DO keeps counters in-process (single-digit ms) and is strongly consistent.

**Shard DOs for throughput.** A single DO serializes all requests. For rate-limiting where you track many independent keys, hash the key to a fixed number of DO instances (e.g. 64 shards). This distributes load while keeping each shard warm in memory:

```rust
const NUM_SHARDS: u32 = 64;

fn shard_for(key: &str) -> u32 {
    let mut h: u32 = 2166136261; // FNV-1a offset basis
    for b in key.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619); // FNV-1a prime
    }
    h % NUM_SHARDS
}
```

**Use `ctx.wait_until()` for fire-and-forget writes.** Counter increments that don't affect the request's decision should be deferred so they don't add latency to the response:

```rust
let env2 = env.clone();
let key2 = my_key.clone();
ctx.wait_until(async move {
    let _ = do_inc(&env2, &key2, WINDOW_S).await;
});
```

**Use `futures::join!` to parallelize independent DO reads.** When you need two counter values, fetch them concurrently instead of sequentially:

```rust
let (a, b) = futures::join!(do_get(&env, &key_a), do_get(&env, &key_b));
```

**Two-phase read-then-write for rate limiting.** Read the counter, make the block/pass decision, respond to the client, then write the increment in `wait_until`. The counter is at most 1 request stale — negligible for rate limiting:

```rust
// Read (fast, on hot path)
let count = do_get(&env, &key).await?;
if count >= LIMIT {
    return edge_response(429, "rate-limited");
}

// Respond to client immediately
let resp = Fetch::Request(req).send().await?;

// Write (deferred, doesn't add latency)
let env2 = env.clone();
ctx.wait_until(async move {
    let _ = do_inc(&env2, &key, WINDOW_S).await;
});
```

**WASM binary size matters.** CF Workers limit is 1MB compressed. Use `opt-level = "s"`, `lto = true`, and `codegen-units = 1` in `Cargo.toml` `[profile.release]`. Check with `gzip -c target/wasm32-unknown-unknown/release/your_crate.wasm | wc -c`.

---

## Cross-Platform

**Cross-platform is possible but has real landmines.** A few specific ones:

- **Android TLS:** On Android, non-JVM code cannot access the system TLS certificate store, so native HTTPS can get annoying fast. If you are targeting multiple platforms, `rustls` is usually the least painful default.
- **Crypto providers:** `ring`'s maintenance hiatus is over, but the ecosystem has largely moved to `aws-lc-rs` — it's now `rustls`'s default provider. Either can be fine; the real footgun is not knowing which one your dependency tree uses. Verify it.

---

## Crate Features

**Don't assume default feature flags are what you want.** Accepting defaults can silently pull in code you do not need. Feature selection is part of supply-chain hygiene as much as binary size.

The `cargo-appraiser` VSCode extension helps by showing outdated packages and surfacing crate feature flags on hover.

---

## Conditional Compilation

**Be careful with `#[cfg(test)]` — prefer `if cfg!(...)` where feasible.** Attribute-style conditional compilation creates dead zones that do not get compiled outside their target configuration. Using `if cfg!(...)` can be better when you want both branches compiled and checked, but it is not a universal replacement for real conditional compilation.

---

## Documentation

**The documentation situation is rough, but not uniform.** A lot of autogenerated Rust docs mirror signatures and comments without showing usage flow. Some crates do this well, but many do not. Reading source and searching for real-world examples is still part of the workflow.

---

## Python FFI (PyO3)

**`PyO3` + `maturin` is the default stack if Python is your only foreign consumer.** Do not reach for a raw C-API unless C compatibility itself is a requirement.

**Be strict about interpreter attachment.** In modern PyO3, `Python::with_gil` was renamed to `Python::attach`, and `Python::allow_threads` was renamed to `Python::detach`. Use attachment only when you need to touch Python objects, and do CPU-bound Rust work detached from Python.

**Use the modern object model.** The old GIL-ref style APIs are gone, and the `Bound<'py, T>` / `Py<T>` split is the core mental model now. `Bound<'py, T>` is for interpreter-attached work; `Py<T>` is the owned handle you can store and move around.

**Prefer modern type names and helpers.** `PyObject` is deprecated as a convenience alias; prefer `Py<PyAny>`. Old examples using `downcast()` should increasingly be thought of as `cast()` examples, and raw-pointer constructors on `Py<T>` are being phased out in favor of `Bound` constructors plus explicit checked or unchecked casts.

**Design for free-threaded Python.** Since PyO3's recent releases, free-threaded Python support is no longer a side path. Module defaults now assume free-threaded support unless you opt back into GIL dependence, and CPython officially declares free-threading supported starting with 3.14 (PyO3 dropped 3.13t). There is even a free-threaded stable ABI (PEP 803, the `abi3t` features) for shipping wheels that work across free-threaded versions. `#[pyclass]` types need to be safe in that world, and older GIL-based synchronization helpers are being removed.

**Use `PyOnceLock`, not `GILOnceCell`.** `GILOnceCell` was tied to the old GIL-centric model and allowed racy initialization behavior. `PyOnceLock` is the intended replacement for Python-attached single initialization, and `std::sync::OnceLock` plus PyO3 extension traits is often a good fit too.

**Avoid `GILProtected`.** It is deprecated; use standard synchronization primitives that still make sense under free-threaded Python, such as `Mutex`, together with PyO3's helper traits where needed.

**Conversion cost is a real performance cliff.** Crossing the Python boundary with huge lists, dicts, or strings can erase your gains. For hot paths, prefer bytes, buffers, numpy interop, or pushing work onto plain Rust-owned structures as quickly as possible.

**Byte conversions changed in your favor.** Modern PyO3 converts `Vec<u8>` and similar byte collections to `PyBytes` rather than Python lists, which is usually what you wanted in the first place.

**Return conversions were redesigned.** The old `IntoPy` / `ToPyObject` direction is being replaced by `IntoPyObject`, which is more explicit, more flexible, and better for owned and borrowed conversions. If you have custom conversion code in an older extension, this is one of the first places to audit.

**Extraction traits changed too.** `FromPyObject` now has a more flexible lifetime story and an associated error type, and generic container extraction often wants `FromPyObjectOwned` bounds rather than the older simpler patterns.

**Packaging is part of the design, not the finish line.** `maturin` makes wheel building nice, but wheels are still platform-specific artifacts. Treat CI, wheel builds, and target matrix decisions as first-class concerns early. As of PyO3 0.30 the MSRV is Rust 1.85, and free-threaded Python support is opt-out rather than opt-in — assume free-threaded users unless you explicitly mark otherwise. Note that the long-deprecated `extension-module` cargo feature is gone entirely in 0.30; `maturin` 1.9.4+ sets the `PYO3_BUILD_EXTENSION_MODULE` env var instead, so keep maturin up to date rather than carrying the old feature flag.

### Async Rust from Python

**For simple cases, PyO3's built-in async support may be enough.** `async fn` works directly in `#[pyfunction]` and `#[pymethods]` behind the `experimental-async` feature. The constraint: the resulting future must be `Send + 'static`, so arguments can't borrow from Python across await points — extract owned data up front.

**For runtime bridging, use `pyo3-async-runtimes`.** A lot of examples still point at `pyo3-asyncio`; those are stale. If you need to drive Rust futures on a specific runtime (tokio, async-std) or await Python coroutines from Rust, use the maintained bridge crate (0.30, tracking PyO3 0.30), and keep Python object access at the edges.

**Be careful with runtime assumptions.** Async interoperability is solid when your Python app is running on `asyncio`, which covers FastAPI and Starlette deployments. If your stack may run under Trio-backed `anyio`, assume more friction and design accordingly.

---

## PyO3 migration notes worth knowing

If you're upgrading older code rather than starting fresh, these are the changes that matter most in practice:

### 0.20 -> 0.30 (consolidated)

If you're jumping from the `Bound`-era or earlier, here is the whole arc, grouped the way you'll actually hit the changes:

**The object model was rebuilt around `Bound`.** The old GIL-ref API (`&PyAny` and friends) is gone, along with `PyCell` and the `gil-refs` feature. `Bound<'py, T>` is for interpreter-attached work, `Py<T>` is the owned handle. Casting moved from `PyTryFrom` to `downcast` and finally to the `cast` / `CastError` family (`DowncastError` no longer exists). Raw-pointer constructors on `Py<T>` are deprecated in favor of `Bound` constructors.

**The GIL vocabulary was renamed.** `Python::with_gil` → `Python::attach`, `Python::allow_threads` → `Python::detach`, `prepare_freethreaded_python` → `Python::initialize`. `GILOnceCell` → `PyOnceLock`, `GILProtected` is deprecated, and the `PyObject` alias is deprecated in favor of `Py<PyAny>`. Refcount accessors (`get_refcnt`) are deprecated in favor of `ffi::Py_REFCNT`.

**Conversion traits were redesigned.** `IntoPy` / `ToPyObject` / `AsPyPointer` are gone; `IntoPyObject` is the single Rust→Python story. `FromPyObject` gained a second lifetime (so it can borrow, e.g. `&str` from Python `str`), an associated error type, and `FromPyObjectOwned` as the convenient bound for owned data. `Vec<u8>` and friends now convert to `PyBytes` instead of lists. `Path`/`PathBuf` convert to `pathlib.Path` instead of strings.

**Free-threaded Python went from experiment to default.** Support landed seriously in 0.23 (`#[pyclass]` types must now be `Sync`), became opt-out in 0.28, and by 0.29 targets the PEP 803 free-threaded stable ABI via the `abi3t` / `abi3t-py315` features. CPython officially supports free-threading from 3.14 — 3.13t is no longer supported. Design your classes and synchronization for a GIL-optional world.

**Signatures and classes became more explicit.** Trailing `Option<T>` arguments no longer get an implicit `None` default. `Py<T>: Clone` is opt-in behind a feature. Simple enum equality must be declared via `eq` / `eq_int`. Automatic by-value `FromPyObject` for `#[pyclass] + Clone` types is being phased out (`from_py_object` / `skip_from_py_object` to control it). `#[pyfn]` is gone. `PyTypeCheck` is an unsafe trait.

**Module initialization modernized.** Multi-phase initialization (PEP-489) landed in 0.28, and on Python 3.15+ PyO3 uses the PEP 820 PySlot / `PyModExport` APIs — the long-term direction toward subinterpreters and per-module state. Untyped and typed buffer APIs were split apart.

**Platform and packaging notes.** MSRV is Rust 1.85 (since 0.30; it was 1.83 in 0.28/0.29). Python 3.8 and 3.7 support are dropped; Python 3.15 final is supported (PyPy 3.12 and GraalPy ≥ 25.4 too), and meson-python is now usable as a build backend alongside maturin. The `extension-module` feature was removed in 0.30 — maturin 1.9.4+ sets `PYO3_BUILD_EXTENSION_MODULE` instead. Windows builds link via `raw-dylib` by default (`generate-import-lib` deprecated; opt out with `PYO3_USE_RAW_DYLIB=0`). `pyo3-ffi` is now `no_std`.

**Smaller but visible changes.** `PyEnvironmentError` / `PyIOError` / `PyWindowsError` are aliases of `PyOSError`. `#[pyfunction]` argument-extraction errors enrich via `PyErr::add_note` instead of replacing the `TypeError`. `PyMemoryError` / `io::ErrorKind::OutOfMemory` conversions got more precise. In 0.30, `PyBuffer::as_slice` / `as_mut_slice` / `as_fortran_slice` (and `_mut`) were deprecated as unsound — use the `as_slice_ptr` / `as_fortran_slice_ptr` family instead.

---

## Practical Python FFI rule of thumb

If the hot path is CPU-bound, move plain data into Rust quickly, detach from Python, do the work, and only re-attach at the edges. If the hot path is mostly object shuffling, FFI may not buy you much and can even make things worse.
