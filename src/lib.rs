use ::libsql as libsql_core;
use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList, PyModule, PyTuple};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use tokio::runtime::{Handle, Runtime};

const LEGACY_TRANSACTION_CONTROL: i32 = -1;

#[derive(Clone)]
enum ListOrTuple<'py> {
    List(Bound<'py, PyList>),
    Tuple(Bound<'py, PyTuple>),
}

struct ListOrTupleIterator<'py> {
    index: usize,
    inner: ListOrTuple<'py>,
}

fn rt() -> Handle {
    static RT: OnceLock<Runtime> = OnceLock::new();

    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    })
    .handle()
    .clone()
}

/// Lock a mutex, ignoring poisoning (the guarded state stays usable after a
/// panic in another thread).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run a future to completion on the shared runtime while detached from the
/// Python interpreter, so other Python threads can make progress.
fn block_on_detached<F, T>(py: Python<'_>, fut: F) -> T
where
    F: Future<Output = T> + Send,
    T: Send,
{
    py.detach(|| rt().block_on(fut))
}

/// Like [`block_on_detached`], but periodically re-attaches to check for
/// pending signals so that Ctrl-C can interrupt long-running operations.
fn block_on_interruptible<F, T>(py: Python<'_>, fut: F) -> PyResult<T>
where
    F: Future<Output = Result<T, libsql_core::errors::Error>> + Send,
    T: Send,
{
    py.detach(|| {
        rt().block_on(async move {
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    out = &mut fut => break out.map_err(to_py_err),
                    _ = tokio::time::sleep(Duration::from_millis(300)) => {
                        Python::attach(|py| py.check_signals())?;
                    }
                }
            }
        })
    })
}

fn to_py_err(error: libsql_core::errors::Error) -> PyErr {
    let msg = match error {
        libsql_core::Error::SqliteFailure(_, err) => err,
        _ => error.to_string(),
    };
    PyValueError::new_err(msg)
}

fn is_remote_path(path: &str) -> bool {
    path.starts_with("libsql://") || path.starts_with("http://") || path.starts_with("https://")
}

#[pyfunction]
#[cfg(not(Py_3_12))]
#[pyo3(signature = (database, timeout=5.0, isolation_level="DEFERRED".to_string(), _check_same_thread=true, _uri=false, sync_url=None, sync_interval=None, offline=false, auth_token="", encryption_key=None))]
fn connect(
    py: Python<'_>,
    database: String,
    timeout: f64,
    isolation_level: Option<String>,
    _check_same_thread: bool,
    _uri: bool,
    sync_url: Option<String>,
    sync_interval: Option<f64>,
    offline: bool,
    auth_token: &str,
    encryption_key: Option<String>,
) -> PyResult<Connection> {
    let conn = _connect_core(
        py,
        database,
        timeout,
        isolation_level,
        _check_same_thread,
        _uri,
        sync_url,
        sync_interval,
        offline,
        auth_token,
        encryption_key,
    )?;
    Ok(conn)
}

#[pyfunction]
#[cfg(Py_3_12)]
#[pyo3(signature = (database, timeout=5.0, isolation_level="DEFERRED".to_string(), _check_same_thread=true, _uri=false, sync_url=None, sync_interval=None, offline=false, auth_token="", encryption_key=None, autocommit = LEGACY_TRANSACTION_CONTROL))]
fn connect(
    py: Python<'_>,
    database: String,
    timeout: f64,
    isolation_level: Option<String>,
    _check_same_thread: bool,
    _uri: bool,
    sync_url: Option<String>,
    sync_interval: Option<f64>,
    offline: bool,
    auth_token: &str,
    encryption_key: Option<String>,
    autocommit: i32,
) -> PyResult<Connection> {
    let mut conn = _connect_core(
        py,
        database,
        timeout,
        isolation_level.clone(),
        _check_same_thread,
        _uri,
        sync_url,
        sync_interval,
        offline,
        auth_token,
        encryption_key,
    )?;

    conn.autocommit =
        if autocommit == LEGACY_TRANSACTION_CONTROL || autocommit == 1 || autocommit == 0 {
            autocommit
        } else {
            return Err(PyValueError::new_err(
                "autocommit must be True, False, or sqlite3.LEGACY_TRANSACTION_CONTROL",
            ));
        };
    Ok(conn)
}

fn _connect_core(
    py: Python<'_>,
    database: String,
    timeout: f64,
    isolation_level: Option<String>,
    _check_same_thread: bool,
    _uri: bool,
    sync_url: Option<String>,
    sync_interval: Option<f64>,
    offline: bool,
    auth_token: &str,
    encryption_key: Option<String>,
) -> PyResult<Connection> {
    let ver = env!("CARGO_PKG_VERSION");
    let ver = format!("libsql-python-rpc-{ver}");
    let rt = rt();
    let encryption_config = encryption_key.map(|key| {
        let cipher = libsql_core::Cipher::default();
        libsql_core::EncryptionConfig::new(cipher, key.into())
    });
    let db = if is_remote_path(&database) {
        let result = libsql_core::Database::open_remote_internal(database.clone(), auth_token, ver);
        result.map_err(to_py_err)?
    } else {
        match sync_url {
            Some(sync_url) => {
                if encryption_config.is_some() {
                    return Err(PyValueError::new_err(
                        "encryption is not supported for synced databases",
                    ));
                }
                let mut builder = libsql_core::Builder::new_synced_database(
                    database,
                    sync_url,
                    auth_token.to_string(),
                );
                if let Some(sync_interval) = sync_interval {
                    builder = builder.sync_interval(Duration::from_secs_f64(sync_interval));
                }
                builder = builder.remote_writes(!offline);
                block_on_interruptible(py, builder.build())?
            }
            None => {
                let mut builder = libsql_core::Builder::new_local(database);
                if let Some(config) = encryption_config {
                    builder = builder.encryption_config(config);
                }
                block_on_interruptible(py, builder.build())?
            }
        }
    };

    let autocommit = isolation_level.is_none() as i32;
    let conn = db.connect().map_err(to_py_err)?;
    let timeout = Duration::from_secs_f64(timeout);
    conn.busy_timeout(timeout).map_err(to_py_err)?;
    Ok(Connection {
        db,
        conn: Mutex::new(Some(Arc::new(ConnectionGuard {
            conn: Some(conn),
            handle: rt.clone(),
        }))),
        isolation_level,
        autocommit,
    })
}

// We need to add a drop guard that runs when we finally drop our
// only reference to libsql_core::Connection. This is because when
// hrana is enabled it needs access to the tokio api to spawn a close
// call in the background. So this adds the ability that when drop is called
// on ConnectionGuard it will drop the connection with a tokio context entered.
struct ConnectionGuard {
    conn: Option<libsql_core::Connection>,
    handle: tokio::runtime::Handle,
}

impl std::ops::Deref for ConnectionGuard {
    type Target = libsql_core::Connection;

    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().expect("Connection already dropped")
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let _enter = self.handle.enter();
        if let Some(conn) = self.conn.take() {
            drop(conn);
        }
    }
}

fn closed_err() -> PyErr {
    PyValueError::new_err("Connection already closed")
}

#[pyclass]
pub struct Connection {
    db: libsql_core::Database,
    conn: Mutex<Option<Arc<ConnectionGuard>>>,
    isolation_level: Option<String>,
    autocommit: i32,
}

impl Connection {
    fn handle(&self) -> PyResult<Arc<ConnectionGuard>> {
        lock(&self.conn).as_ref().cloned().ok_or_else(closed_err)
    }

    fn finish_transaction(&self, py: Python<'_>, sql: &'static str) -> PyResult<()> {
        // TODO: Switch to libSQL transaction API
        let conn = self.handle()?;
        if !conn.is_autocommit() {
            block_on_detached(py, conn.execute(sql, ())).map_err(to_py_err)?;
        }
        Ok(())
    }
}

#[pymethods]
impl Connection {
    fn close(&self) -> PyResult<()> {
        let _enter = rt().enter();
        drop(lock(&self.conn).take());
        Ok(())
    }

    fn cursor(&self) -> PyResult<Cursor> {
        Ok(Cursor {
            arraysize: 1,
            conn: Mutex::new(Some(self.handle()?)),
            stmt: Mutex::new(None),
            rows: Mutex::new(None),
            rowcount: AtomicI64::new(0),
            autocommit: self.autocommit,
            isolation_level: self.isolation_level.clone(),
            done: AtomicBool::new(false),
        })
    }

    fn sync(&self, py: Python<'_>) -> PyResult<()> {
        let fut = {
            let _enter = rt().enter();
            self.db.sync()
        };
        block_on_interruptible(py, fut)?;
        Ok(())
    }

    fn commit(&self, py: Python<'_>) -> PyResult<()> {
        self.finish_transaction(py, "COMMIT")
    }

    fn rollback(&self, py: Python<'_>) -> PyResult<()> {
        self.finish_transaction(py, "ROLLBACK")
    }

    #[pyo3(signature = (sql, parameters=None))]
    fn execute(
        &self,
        py: Python<'_>,
        sql: String,
        parameters: Option<ListOrTuple<'_>>,
    ) -> PyResult<Cursor> {
        let cursor = self.cursor()?;
        execute(py, &cursor, sql, parameters)?;
        Ok(cursor)
    }

    #[pyo3(signature = (sql, parameters=None))]
    fn executemany(
        &self,
        py: Python<'_>,
        sql: String,
        parameters: Option<&Bound<'_, PyList>>,
    ) -> PyResult<Cursor> {
        let cursor = self.cursor()?;
        execute_many(py, &cursor, &sql, parameters)?;
        Ok(cursor)
    }

    fn executescript(&self, py: Python<'_>, script: String) -> PyResult<()> {
        let conn = self.handle()?;
        // NOTE: errors from the script are intentionally ignored here to keep
        // the existing behavior; `Cursor.executescript` does propagate them.
        let _ =
            block_on_detached(py, async { conn.execute_batch(&script).await }).map_err(to_py_err);
        Ok(())
    }

    #[getter]
    fn isolation_level(&self) -> Option<String> {
        self.isolation_level.clone()
    }

    #[getter]
    fn in_transaction(&self) -> PyResult<bool> {
        let in_txn = !self.handle()?.is_autocommit();
        Ok(in_txn || (cfg!(Py_3_12) && self.autocommit == 0))
    }

    #[getter]
    #[cfg(Py_3_12)]
    fn get_autocommit(&self) -> i32 {
        self.autocommit
    }

    #[setter]
    #[cfg(Py_3_12)]
    fn set_autocommit(&mut self, autocommit: i32) -> PyResult<()> {
        if autocommit != LEGACY_TRANSACTION_CONTROL && autocommit != 1 && autocommit != 0 {
            return Err(PyValueError::new_err(
                "autocommit must be True, False, or sqlite3.LEGACY_TRANSACTION_CONTROL",
            ));
        }
        self.autocommit = autocommit;
        Ok(())
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        if exc_type.is_none() {
            // Commit on clean exit
            self.commit(py)?;
        } else {
            // Rollback on error
            self.rollback(py)?;
        }
        Ok(false) // Always propagate exceptions
    }
}

#[pyclass]
pub struct Cursor {
    #[pyo3(get, set)]
    arraysize: usize,
    conn: Mutex<Option<Arc<ConnectionGuard>>>,
    stmt: Mutex<Option<libsql_core::Statement>>,
    rows: Mutex<Option<libsql_core::Rows>>,
    rowcount: AtomicI64,
    done: AtomicBool,
    isolation_level: Option<String>,
    autocommit: i32,
}

impl Cursor {
    fn handle(&self) -> PyResult<Arc<ConnectionGuard>> {
        lock(&self.conn).as_ref().cloned().ok_or_else(closed_err)
    }

    /// Fetch up to `limit` rows (all remaining rows when `None`) as plain Rust
    /// values, detached from the interpreter. Returns `None` when the last
    /// statement produced no result set.
    fn fetch_values(
        &self,
        py: Python<'_>,
        limit: Option<usize>,
    ) -> PyResult<Option<Vec<Vec<libsql_core::Value>>>> {
        py.detach(|| {
            let mut guard = lock(&self.rows);
            let Some(rows) = guard.as_mut() else {
                return Ok(None);
            };
            let mut out = vec![];
            // The libSQL Rows.next() method restarts the iteration if it
            // has reached the end, which is why we need to check if we're
            // done before iterating.
            if self.done.load(Ordering::Acquire) {
                return Ok(Some(out));
            }
            let column_count = rows.column_count();
            rt().block_on(async {
                while limit.is_none_or(|limit| out.len() < limit) {
                    match rows.next().await.map_err(to_py_err)? {
                        Some(row) => {
                            let mut values = Vec::with_capacity(column_count as usize);
                            for col_idx in 0..column_count {
                                values.push(row.get_value(col_idx).map_err(to_py_err)?);
                            }
                            out.push(values);
                        }
                        None => {
                            self.done.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
                Ok::<(), PyErr>(())
            })?;
            Ok(Some(out))
        })
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        let _enter = rt().enter();
        drop(lock(&self.conn).take());
        drop(lock(&self.stmt).take());
        drop(lock(&self.rows).take());
    }
}

#[pymethods]
impl Cursor {
    fn close(&self) -> PyResult<()> {
        let _enter = rt().enter();
        drop(lock(&self.conn).take());
        drop(lock(&self.stmt).take());
        drop(lock(&self.rows).take());
        Ok(())
    }

    #[pyo3(signature = (sql, parameters=None))]
    fn execute<'a>(
        self_: PyRef<'a, Self>,
        py: Python<'_>,
        sql: String,
        parameters: Option<ListOrTuple<'_>>,
    ) -> PyResult<PyRef<'a, Self>> {
        execute(py, &self_, sql, parameters)?;
        Ok(self_)
    }

    #[pyo3(signature = (sql, parameters=None))]
    fn executemany<'a>(
        self_: PyRef<'a, Self>,
        py: Python<'_>,
        sql: String,
        parameters: Option<&Bound<'_, PyList>>,
    ) -> PyResult<PyRef<'a, Self>> {
        execute_many(py, &self_, &sql, parameters)?;
        Ok(self_)
    }

    fn executescript<'a>(
        self_: PyRef<'a, Self>,
        py: Python<'_>,
        script: String,
    ) -> PyResult<PyRef<'a, Self>> {
        let conn = self_.handle()?;
        block_on_detached(py, async { conn.execute_batch(&script).await }).map_err(to_py_err)?;
        Ok(self_)
    }

    #[getter]
    fn description<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyTuple>>> {
        let stmt = lock(&self.stmt);
        let Some(stmt) = stmt.as_ref() else {
            return Ok(None);
        };
        let mut elements = vec![];
        for column in stmt.columns() {
            let element = (
                column.name(),
                py.None(),
                py.None(),
                py.None(),
                py.None(),
                py.None(),
                py.None(),
            )
                .into_pyobject(py)?;
            elements.push(element);
        }
        Ok(Some(PyTuple::new(py, elements)?))
    }

    fn fetchone<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyTuple>>> {
        match self.fetch_values(py, Some(1))? {
            Some(rows) => rows
                .into_iter()
                .next()
                .map(|row| convert_row(py, row))
                .transpose(),
            None => Ok(None),
        }
    }

    #[pyo3(signature = (size=None))]
    fn fetchmany<'py>(
        &self,
        py: Python<'py>,
        size: Option<i64>,
    ) -> PyResult<Option<Bound<'py, PyList>>> {
        let size = size.unwrap_or(self.arraysize as i64).max(0) as usize;
        self.fetch_values(py, Some(size))?
            .map(|rows| convert_rows(py, rows))
            .transpose()
    }

    fn fetchall<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        self.fetch_values(py, None)?
            .map(|rows| convert_rows(py, rows))
            .transpose()
    }

    #[getter]
    fn lastrowid(&self) -> PyResult<Option<i64>> {
        if lock(&self.stmt).is_some() {
            Ok(Some(self.handle()?.last_insert_rowid()))
        } else {
            Ok(None)
        }
    }

    #[getter]
    fn rowcount(&self) -> i64 {
        self.rowcount.load(Ordering::Acquire)
    }
}

async fn run_statement(
    conn: &libsql_core::Connection,
    sql: &str,
    params: libsql_core::params::Params,
    begin_transaction: bool,
) -> PyResult<(libsql_core::Statement, Option<libsql_core::Rows>)> {
    if begin_transaction && conn.is_autocommit() {
        conn.execute("BEGIN", ()).await.map_err(to_py_err)?;
    }
    let stmt = conn.prepare(sql).await.map_err(to_py_err)?;
    let rows = if !stmt.columns().is_empty() {
        Some(stmt.query(params).await.map_err(to_py_err)?)
    } else {
        stmt.execute(params).await.map_err(to_py_err)?;
        None
    };
    Ok((stmt, rows))
}

fn execute(
    py: Python<'_>,
    cursor: &Cursor,
    sql: String,
    parameters: Option<ListOrTuple<'_>>,
) -> PyResult<()> {
    let conn = cursor.handle()?;
    // Convert parameters while attached to the interpreter; everything after
    // this point is plain Rust data and runs detached.
    let params = convert_params(parameters)?;
    let begin_transaction = !determine_autocommit(cursor) && stmt_is_dml(&sql);

    let (stmt, rows) =
        block_on_detached(py, run_statement(&conn, &sql, params, begin_transaction))?;

    cursor.done.store(false, Ordering::Release);
    cursor
        .rowcount
        .fetch_add(conn.changes() as i64, Ordering::AcqRel);

    // Dropping the previous statement/rows may need a tokio context.
    let _enter = rt().enter();
    *lock(&cursor.rows) = rows;
    *lock(&cursor.stmt) = Some(stmt);
    Ok(())
}

fn execute_many(
    py: Python<'_>,
    cursor: &Cursor,
    sql: &str,
    parameters: Option<&Bound<'_, PyList>>,
) -> PyResult<()> {
    let parameters = parameters
        .ok_or_else(|| PyValueError::new_err("executemany requires a list of parameters"))?;
    for parameters in parameters.iter() {
        let parameters = parameters.extract::<ListOrTuple>()?;
        execute(py, cursor, sql.to_string(), Some(parameters))?;
    }
    Ok(())
}

fn convert_params(parameters: Option<ListOrTuple<'_>>) -> PyResult<libsql_core::params::Params> {
    let Some(parameters) = parameters else {
        return Ok(libsql_core::params::Params::None);
    };
    let mut params = vec![];
    for param in parameters.iter() {
        let param = if param.is_none() {
            libsql_core::Value::Null
        } else if let Ok(value) = param.extract::<i64>() {
            libsql_core::Value::Integer(value)
        } else if let Ok(value) = param.extract::<f64>() {
            libsql_core::Value::Real(value)
        } else if let Ok(value) = param.extract::<&str>() {
            libsql_core::Value::Text(value.to_string())
        } else if let Ok(value) = param.extract::<&[u8]>() {
            libsql_core::Value::Blob(value.to_vec())
        } else {
            return Err(PyValueError::new_err(format!(
                "Unsupported parameter type {}",
                param
            )));
        };
        params.push(param);
    }
    Ok(libsql_core::params::Params::Positional(params))
}

fn determine_autocommit(cursor: &Cursor) -> bool {
    if cfg!(Py_3_12) {
        match cursor.autocommit {
            LEGACY_TRANSACTION_CONTROL => cursor.isolation_level.is_none(),
            _ => cursor.autocommit != 0,
        }
    } else {
        cursor.isolation_level.is_none()
    }
}

fn stmt_is_dml(sql: &str) -> bool {
    let sql = sql.trim();
    let sql = sql.to_uppercase();
    sql.starts_with("INSERT") || sql.starts_with("UPDATE") || sql.starts_with("DELETE")
}

fn convert_row(py: Python<'_>, row: Vec<libsql_core::Value>) -> PyResult<Bound<'_, PyTuple>> {
    let mut elements: Vec<Bound<'_, PyAny>> = Vec::with_capacity(row.len());
    for value in row {
        let value = match value {
            libsql_core::Value::Integer(v) => v.into_pyobject(py)?.into_any(),
            libsql_core::Value::Real(v) => v.into_pyobject(py)?.into_any(),
            libsql_core::Value::Text(v) => v.into_pyobject(py)?.into_any(),
            libsql_core::Value::Blob(v) => PyBytes::new(py, &v).into_any(),
            libsql_core::Value::Null => py.None().into_bound(py),
        };
        elements.push(value);
    }
    PyTuple::new(py, elements)
}

fn convert_rows(py: Python<'_>, rows: Vec<Vec<libsql_core::Value>>) -> PyResult<Bound<'_, PyList>> {
    let rows = rows
        .into_iter()
        .map(|row| convert_row(py, row))
        .collect::<PyResult<Vec<_>>>()?;
    PyList::new(py, rows)
}

create_exception!(libsql, Error, pyo3::exceptions::PyException);

impl<'a, 'py> FromPyObject<'a, 'py> for ListOrTuple<'py> {
    type Error = PyErr;

    fn extract(ob: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        if let Ok(list) = ob.cast::<PyList>() {
            Ok(ListOrTuple::List(list.to_owned()))
        } else if let Ok(tuple) = ob.cast::<PyTuple>() {
            Ok(ListOrTuple::Tuple(tuple.to_owned()))
        } else {
            Err(PyValueError::new_err(
                "Expected a list or tuple for parameters",
            ))
        }
    }
}

impl<'py> ListOrTuple<'py> {
    pub fn iter(&self) -> ListOrTupleIterator<'py> {
        ListOrTupleIterator {
            index: 0,
            inner: self.clone(),
        }
    }
}

impl<'py> Iterator for ListOrTupleIterator<'py> {
    type Item = Bound<'py, PyAny>;

    fn next(&mut self) -> Option<Self::Item> {
        let rv = match &self.inner {
            ListOrTuple::List(list) => list.get_item(self.index),
            ListOrTuple::Tuple(tuple) => tuple.get_item(self.index),
        };

        rv.ok().map(|item| {
            self.index += 1;
            item
        })
    }
}

#[pymodule]
fn libsql(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    let _ = tracing_subscriber::fmt::try_init();
    m.add("LEGACY_TRANSACTION_CONTROL", LEGACY_TRANSACTION_CONTROL)?;
    m.add("paramstyle", "qmark")?;
    m.add("sqlite_version_info", (3, 42, 0))?;
    m.add("Error", py.get_type::<Error>())?;
    m.add_function(wrap_pyfunction!(connect, m)?)?;
    m.add_class::<Connection>()?;
    m.add_class::<Cursor>()?;
    Ok(())
}
