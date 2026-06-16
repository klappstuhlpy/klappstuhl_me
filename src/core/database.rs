//! A small async wrapper around SQLite.
//!
//! There is no async SQLite driver, so [`Database`] runs a fixed pool of OS
//! threads, each owning one blocking [`rusqlite::Connection`]. Queries are closures
//! shipped to an idle worker over a `crossbeam-channel`, with the result returned
//! over a `oneshot`. The public API ([`Database::get`], [`Database::all`],
//! [`Database::execute`], …) is `async` and mirrors rusqlite.
//!
//! Connections are tuned for a WAL, concurrent-reader workload and each worker
//! survives a panicking query, so one bad closure can't tear down the pool.

use std::{
    borrow::Cow,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use crossbeam_channel::{Receiver, Sender};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, trace, warn};

/// A trait that all table-like types must meet.
pub trait Table: Sized {
    /// The table name
    const NAME: &'static str;
    /// An array of all the columns of the table.
    const COLUMNS: &'static [&'static str];
    /// The type of the table's ID column
    type Id;

    /// Conversion from a rusqlite Row to the target type.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self>;

    /// Creates an update query with the given column names.
    ///
    /// The last parameter is the primary key of the row.
    ///
    /// Panics if the column is not in the COLUMNS array.
    fn update_query(columns: impl AsRef<[&'static str]>) -> String {
        let mut query = format!("UPDATE {} SET ", Self::NAME);
        let mut first = true;
        for column in columns.as_ref() {
            if first {
                first = false;
            } else {
                query.push_str(", ");
            }
            if Self::COLUMNS.contains(column) {
                query.push_str(column);
                query.push_str(" = ?");
            } else {
                panic!("Column {} is not in the COLUMNS array", column);
            }
        }
        query.push_str(" WHERE id = ?");
        query
    }
}

type SqliteCall = Box<dyn FnOnce(&mut rusqlite::Connection) + Send + 'static>;
/// The connection-initialisation hook (e.g. the migration runner), run once per
/// worker after the standard pragmas are applied.
type InitDyn = dyn Fn(&mut rusqlite::Connection) -> anyhow::Result<()> + Send + Sync + 'static;
type InitFn = Arc<InitDyn>;

const UNREACHABLE: &str = "connection communication channels unexpectedly terminated";

/// Connection-level pragmas applied to every worker connection before it is handed
/// out. These tune SQLite for the app's WAL, many-readers/few-writers workload.
fn configure_connection(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    // Wait rather than fail when another worker (or the migration race at startup)
    // holds the write lock. Five seconds is ample for any statement here.
    connection.busy_timeout(Duration::from_secs(5))?;
    // execute_batch happily steps through pragmas that return a row (journal_mode).
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;       -- concurrent readers alongside one writer
         PRAGMA foreign_keys = ON;        -- enforce referential integrity
         PRAGMA synchronous = NORMAL;     -- safe with WAL, far fewer fsyncs than FULL
         PRAGMA temp_store = MEMORY;      -- keep temp b-trees off disk
         PRAGMA cache_size = -16384;      -- 16 MiB page cache per connection
         PRAGMA mmap_size = 268435456;    -- 256 MiB memory-mapped I/O
         PRAGMA wal_autocheckpoint = 1000;-- checkpoint roughly every 4 MiB of WAL",
    )?;
    // The default prepared-statement cache holds only 16 statements; the app has
    // well over that many distinct cached queries per connection, so a small cache
    // thrashes (re-preparing on eviction). Raise it generously — statements are cheap.
    connection.set_prepared_statement_cache_capacity(128);
    Ok(())
}

/// The error reported when a worker thread drops a query's response channel — it
/// caught a panic inside the query closure. Surfacing this as a normal error keeps
/// a panicking query from taking down the calling request task.
fn worker_unavailable() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT),
        Some("database worker did not return a result (a query closure panicked)".to_owned()),
    )
}

enum Message {
    Call(SqliteCall),
    Terminate,
}

/// Builder to help open a database connection.
///
/// This is retrieved from [`Database::file`].
pub struct DatabaseBuilder {
    path: PathBuf,
    max_connections: usize,
    init: Option<InitFn>,
}

impl DatabaseBuilder {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            max_connections: 10,
            init: None,
        }
    }

    /// Configure how many connections to open.
    ///
    /// These connections are each a separate thread.
    pub fn connections(mut self, max_connections: usize) -> Self {
        self.max_connections = max_connections.max(1);
        self
    }

    /// Configure the function to call when the connection is successfully opened.
    ///
    /// Connection-level pragmas (WAL, foreign keys, busy timeout, …) are always
    /// applied *before* this runs, so the init function is free to assume them.
    /// This is the right place to run migrations.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use klappstuhl_me::Database;
    /// let db = Database::file("app.db")
    ///     .with_init(|c| {
    ///         c.execute_batch("CREATE TABLE IF NOT EXISTS foo(id INTEGER PRIMARY KEY);")?;
    ///         Ok(())
    ///     })
    ///     .open();
    /// ```
    pub fn with_init<F>(mut self, init: F) -> Self
    where
        F: Fn(&mut rusqlite::Connection) -> anyhow::Result<()> + Send + Sync + 'static,
    {
        self.init = Some(Arc::new(init));
        self
    }

    /// Opens the database and the background threads needed for the connection pooling mechanism.
    ///
    /// If any of the connections fail to open (or fail their init) then the failure is returned and
    /// all already-spawned workers are torn down.
    pub async fn open(self) -> anyhow::Result<Database> {
        let (result_sender, mut result_receiver) = mpsc::channel(self.max_connections);
        let (sender, receiver) = crossbeam_channel::unbounded();

        info!(
            "creating a threaded connection pool with {} maximum connections to {}",
            self.max_connections,
            self.path.display()
        );
        let mut workers = Vec::with_capacity(self.max_connections);
        for i in 0..self.max_connections {
            workers.push(Worker::new(
                i,
                self.path.clone(),
                result_sender.clone(),
                self.init.clone(),
                receiver.clone(),
            ));
        }

        // Drop our handle so the channel closes once every worker has reported in.
        drop(result_sender);

        let db = Database { sender, workers };

        // Each worker sends exactly one result (Ok once ready, or Err on failure)
        // and then drops its sender. The loop ends when all senders are gone.
        while let Some(result) = result_receiver.recv().await {
            if let Err(e) = result {
                error!(error = %e, "a database connection failed to initialise; tearing down the pool");
                // `db` drops here, terminating and joining the other workers.
                return Err(e);
            }
        }

        Ok(db)
    }
}

/// The handle responsible for all database related queries.
///
/// This manages a few background threads in order to implement proper
/// connection pooling. Each thread maintains one SQLite connection.
pub struct Database {
    sender: Sender<Message>,
    workers: Vec<Worker>,
}

impl Database {
    /// Returns a builder to open the database at the specified file location.
    ///
    /// The :memory: path can be used to denote an in-memory database.
    pub fn file<P: AsRef<Path>>(path: P) -> DatabaseBuilder {
        DatabaseBuilder::new(path.as_ref().to_owned())
    }

    /// Call a function in a background thread with a connection and get the result asynchronously.
    ///
    /// This is the raw escape hatch. If `func` panics, the worker catches it (the
    /// pool stays alive) but this method's caller cannot be handed a value, so the
    /// caller's task panics in turn. Prefer the fallible typed helpers ([`get`],
    /// [`all`], [`execute`], [`transaction`], …) which turn such a panic into an
    /// `Err` instead.
    ///
    /// [`get`]: Database::get
    /// [`all`]: Database::all
    /// [`execute`]: Database::execute
    /// [`transaction`]: Database::transaction
    pub async fn call<F, R>(&self, func: F) -> R
    where
        F: FnOnce(&mut rusqlite::Connection) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (sender, receiver) = oneshot::channel();

        self.sender
            .send(Message::Call(Box::new(move |conn| {
                let _ = sender.send(func(conn));
            })))
            .expect(UNREACHABLE);

        receiver.await.expect(UNREACHABLE)
    }

    /// Dispatch a fallible query closure to a worker, mapping a worker-side panic
    /// (signalled by a dropped response channel) into a [`worker_unavailable`] error
    /// rather than panicking the caller. All the typed helpers below build on this.
    async fn dispatch<F, T>(&self, func: F) -> rusqlite::Result<T>
    where
        F: FnOnce(&mut rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (sender, receiver) = oneshot::channel();

        self.sender
            .send(Message::Call(Box::new(move |conn| {
                let _ = sender.send(func(conn));
            })))
            .expect(UNREACHABLE);

        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(worker_unavailable()),
        }
    }

    /// Number of queries currently queued waiting for a free worker.
    ///
    /// Persistently non-zero means the pool is saturated — every connection is busy
    /// and work is backing up. The maintenance task samples this to warn.
    pub fn queued(&self) -> usize {
        self.sender.len()
    }

    /// Number of worker connections in the pool.
    pub fn pool_size(&self) -> usize {
        self.workers.len()
    }

    /// Execute the given query with the given parameters with a connection from the pool.
    pub async fn execute<Q, P>(&self, query: Q, params: P) -> rusqlite::Result<usize>
    where
        Q: Into<Cow<'static, str>> + Send,
        P: rusqlite::Params + Send + 'static,
    {
        let query = query.into();
        self.dispatch(move |conn| conn.execute(query.as_ref(), params)).await
    }

    /// Execute the given query with a connection from the pool.
    pub async fn execute_batch<Q>(&self, query: Q) -> rusqlite::Result<()>
    where
        Q: Into<Cow<'static, str>> + Send,
    {
        let query = query.into();
        self.dispatch(move |conn| conn.execute_batch(query.as_ref())).await
    }

    /// Execute the query with the given parameters and get the first result, if any.
    ///
    /// This converts the row to the specified type. If no row is found then `None` is returned.
    pub async fn get<T, Q, P>(&self, query: Q, params: P) -> rusqlite::Result<Option<T>>
    where
        T: Table + Send + 'static,
        P: rusqlite::Params + Send + 'static,
        Q: Into<Cow<'static, str>> + Send,
    {
        let query = query.into();
        self.dispatch(move |conn| -> rusqlite::Result<Option<T>> {
            let mut stmt = conn.prepare_cached(query.as_ref())?;
            match stmt.query_row(params, T::from_row) {
                Ok(value) => Ok(Some(value)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e),
            }
        })
        .await
    }

    /// Execute the query with the given parameters and get the first result, if any.
    ///
    /// This converts the row to the specified type. If no row is found then `None` is returned.
    pub async fn get_row<F, R, Q, P>(&self, query: Q, params: P, func: F) -> rusqlite::Result<R>
    where
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
        P: rusqlite::Params + Send + 'static,
        Q: Into<Cow<'static, str>> + Send,
    {
        let query = query.into();
        self.dispatch(move |conn| -> rusqlite::Result<R> {
            let mut stmt = conn.prepare_cached(query.as_ref())?;
            stmt.query_row(params, func)
        })
        .await
    }

    /// Gets a row from its ID.
    pub async fn get_by_id<T>(&self, id: T::Id) -> rusqlite::Result<Option<T>>
    where
        T: Table + Send + 'static,
        T::Id: rusqlite::ToSql + Send + 'static,
    {
        self.dispatch(move |conn| -> rusqlite::Result<Option<T>> {
            let query = format!("SELECT * FROM {} WHERE id=?", T::NAME);
            let mut stmt = conn.prepare_cached(&query)?;
            match stmt.query_row(rusqlite::params![id], T::from_row) {
                Ok(value) => Ok(Some(value)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e),
            }
        })
        .await
    }

    /// Execute the query with the given parameters and returns all results.
    ///
    /// This converts the row to the specified type.
    pub async fn all<T, Q, P>(&self, query: Q, params: P) -> rusqlite::Result<Vec<T>>
    where
        T: Table + Send + 'static,
        P: rusqlite::Params + Send + 'static,
        Q: Into<Cow<'static, str>> + Send,
    {
        let query = query.into();
        self.dispatch(move |conn| -> rusqlite::Result<Vec<T>> {
            let mut stmt = conn.prepare_cached(query.as_ref())?;
            let result = match stmt.query_map(params, T::from_row) {
                Ok(value) => value.collect(),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Vec::new()),
                Err(e) => Err(e),
            };
            result
        })
        .await
    }

    /// Executes the given function within a transaction.
    pub async fn transaction<F, R>(&self, func: F) -> rusqlite::Result<R>
    where
        F: FnOnce(Transaction) -> rusqlite::Result<R> + Send + Sync + 'static,
        R: Send + 'static,
    {
        self.dispatch(move |conn| -> rusqlite::Result<R> {
            let tx = Transaction {
                inner: conn.transaction()?,
            };
            func(tx)
        })
        .await
    }

    /// Gets the value from the key-value store. Returns `None` if not found.
    ///
    /// Unlike other functions here, all errors are coerced into `None` for usability here.
    pub async fn get_from_storage<T>(&self, key: &'static str) -> Option<T>
    where
        T: rusqlite::types::FromSql + Send + 'static,
    {
        self.dispatch(move |conn| -> rusqlite::Result<Option<T>> {
            let query = "SELECT value FROM storage WHERE name = ?";
            let mut stmt = conn.prepare_cached(query)?;
            let result = stmt.query_row([key], |row| row.get(0));
            match result {
                Ok(value) => Ok(Some(value)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e),
            }
        })
        .await
        .ok()
        .flatten()
    }

    /// Sets the value in the key-value store, inserting the key if it does not yet
    /// exist. A plain `UPDATE` would silently no-op for a missing key, so this
    /// upserts.
    pub async fn update_storage<T>(&self, key: &'static str, value: T) -> rusqlite::Result<()>
    where
        T: rusqlite::types::ToSql + Send + 'static,
    {
        self.dispatch(move |conn| {
            let query = "INSERT INTO storage(name, value) VALUES (?, ?) \
                         ON CONFLICT(name) DO UPDATE SET value = excluded.value";
            let mut stmt = conn.prepare_cached(query)?;
            stmt.execute((key, value))?;
            Ok(())
        })
        .await
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        trace!("sending terminate message to all database connections");

        for worker in &self.workers {
            if !worker.is_finished() {
                // Ignore error because the only error happens if both sides are disconnected
                let _ = self.sender.send(Message::Terminate);
            }
        }

        debug!("shutting down all database connections...");
        for worker in &mut self.workers {
            trace!("terminating database connection worker {}", worker.id);
            worker.terminate();
        }
    }
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").field("workers", &self.workers).finish()
    }
}

struct Worker {
    id: usize,
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    fn new(
        id: usize,
        path: PathBuf,
        result_sender: mpsc::Sender<anyhow::Result<()>>,
        init: Option<InitFn>,
        receiver: Receiver<Message>,
    ) -> Self {
        let thread = thread::Builder::new()
            .name(format!("sqlite-worker-{id}"))
            .spawn(move || {
                let mut connection = match open_and_init(id, &path, init.as_deref()) {
                    Ok(connection) => connection,
                    Err(e) => {
                        let _ = result_sender.blocking_send(Err(e));
                        return;
                    }
                };

                trace!("database connection worker {} signaling readiness", id);
                if result_sender.blocking_send(Ok(())).is_err() {
                    trace!("database connection worker {} could not signal readiness", id);
                    return;
                }
                // Drop early so the readiness aggregation in `open` completes as soon
                // as every worker has reported, rather than at thread exit.
                drop(result_sender);

                while let Ok(msg) = receiver.recv() {
                    match msg {
                        Message::Call(func) => {
                            // A panic inside a query closure must not kill the worker
                            // (which would silently shrink the pool). Catch it; the
                            // caller observes a dropped oneshot and surfaces the error.
                            if std::panic::catch_unwind(AssertUnwindSafe(|| func(&mut connection))).is_err() {
                                error!(
                                    "database connection worker {} caught a panic while executing a query; \
                                     the connection is preserved",
                                    id
                                );
                            }
                        }
                        Message::Terminate => break,
                    }
                }
            })
            .expect("failed to spawn database worker thread");

        Worker {
            id,
            thread: Some(thread),
        }
    }

    fn terminate(&mut self) {
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                warn!(
                    "connection pool worker {} has panicked while cleaning up, ignoring.",
                    self.id
                );
            }
        }
    }

    fn is_finished(&self) -> bool {
        match &self.thread {
            Some(thread) => thread.is_finished(),
            None => true,
        }
    }
}

/// Opens a connection, applies the standard pragmas, then runs the optional init.
fn open_and_init(id: usize, path: &Path, init: Option<&InitDyn>) -> anyhow::Result<rusqlite::Connection> {
    let mut connection = rusqlite::Connection::open(path)
        .map_err(|e| anyhow::Error::from(e).context(format!("worker {id} could not open database connection")))?;
    trace!("database connection worker {} created connection", id);

    configure_connection(&connection)
        .map_err(|e| anyhow::Error::from(e).context(format!("worker {id} could not configure connection pragmas")))?;

    if let Some(init) = init {
        init(&mut connection).map_err(|e| e.context(format!("worker {id} failed during database init")))?;
        trace!("database connection worker {} initialised connection", id);
    }

    Ok(connection)
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("id", &self.id)
            .field("finished", &self.is_finished())
            .finish()
    }
}

/// A macro to generate parameters suitable for sending to a connection's thread.
///
/// This causes one allocation per parameter. This is due to a limitation in Rust's
/// type system not allowing variadic generic arguments, compounded by the fact that
/// rusqlite does not support variadic tuples.
#[macro_export]
macro_rules! boxed_params {
    () => {
        [] as [Box<dyn rusqlite::ToSql>; 0]
    };
    ($($param:expr),+ $(,)?) => {
        rusqlite::params_from_iter([$(Box::new($param) as Box<dyn rusqlite::ToSql + Send>),+])
    };
}

pub use boxed_params;

/// An opaque handle to a transaction.
///
/// This automatically dereferences to the inner transaction type.
pub struct Transaction<'conn> {
    inner: rusqlite::Transaction<'conn>,
}

impl<'conn> std::ops::Deref for Transaction<'conn> {
    type Target = rusqlite::Transaction<'conn>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'conn> std::ops::DerefMut for Transaction<'conn> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<'conn> Transaction<'conn> {
    /// Execute the query with the given parameters and get the first result, if any.
    ///
    /// This converts the row to the specified type. If no row is found then `None` is returned.
    pub fn get<T, P>(&self, query: &str, params: P) -> rusqlite::Result<Option<T>>
    where
        T: Table,
        P: rusqlite::Params,
    {
        let mut stmt = self.inner.prepare_cached(query)?;
        match stmt.query_row(params, T::from_row) {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Execute the query with the given parameters and returns all results.
    ///
    /// This converts the row to the specified type. If no row is found then `None` is returned.
    pub fn all<T, P>(&self, query: &str, params: P) -> rusqlite::Result<Vec<T>>
    where
        T: Table,
        P: rusqlite::Params,
    {
        let mut stmt = self.inner.prepare_cached(query)?;
        let result = match stmt.query_map(params, T::from_row) {
            Ok(value) => value.collect(),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Vec::new()),
            Err(e) => Err(e),
        };
        result
    }
}

/// Checks whether an error is a unique constraint violation.
pub fn is_unique_constraint_violation(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(error, _) => error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
        _ => false,
    }
}

/// Returns (and creates) the directory for the main.db file
pub fn directory() -> anyhow::Result<PathBuf> {
    use anyhow::Context;

    let mut path = dirs::data_dir().context("could not find a data directory for the current user")?;
    path.push(crate::PROGRAM_NAME);
    // create_dir_all is idempotent (no error if it already exists) and tolerates a
    // missing intermediate data dir on first run.
    std::fs::create_dir_all(&path).context("could not create application local data directory")?;
    path.push("main.db");
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Eq, PartialEq, PartialOrd, Ord)]
    struct Foo {
        id: i64,
        name: String,
        age: i64,
    }

    impl Table for Foo {
        const NAME: &'static str = "foo";
        const COLUMNS: &'static [&'static str] = &["id", "name", "age"];
        type Id = i64;

        fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
            Ok(Foo {
                id: row.get("id")?,
                name: row.get("name")?,
                age: row.get("age")?,
            })
        }
    }

    #[tokio::test]
    async fn test_basic_connection() {
        let conn = Database::file(":memory:")
            .with_init(|con| {
                con.execute_batch(
                    "CREATE TABLE IF NOT EXISTS foo(id INTEGER PRIMARY KEY, name TEXT, age INTEGER);
            INSERT INTO foo(name, age) VALUES ('bob', 20), ('tanya', 25), ('phil', 25);",
                )?;
                Ok(())
            })
            .open()
            .await
            .expect("could not connect DB");

        conn.execute("INSERT INTO foo(name, age) VALUES (?, ?)", boxed_params!("someone", 13))
            .await
            .expect("execute failed to run");
        let foo: Option<Foo> = conn
            .get("SELECT * FROM foo WHERE id=?", boxed_params!(1))
            .await
            .expect("get failed to run");

        assert!(foo.is_some());
        assert_eq!(
            foo,
            Some(Foo {
                id: 1,
                name: "bob".to_owned(),
                age: 20
            })
        );
    }

    #[tokio::test]
    async fn worker_survives_panicking_query() {
        let conn = Arc::new(Database::file(":memory:").connections(1).open().await.expect("open db"));

        // A panicking query closure drops its oneshot sender, so the awaiting
        // caller's task panics. Run it on a spawned task so we can absorb that
        // panic without failing the test.
        let task_conn = conn.clone();
        let handle = tokio::spawn(async move {
            task_conn.call(|_c| panic!("boom")).await;
        });
        assert!(handle.await.is_err(), "the panicking call's task should fail");

        // The single worker must still be alive and serving queries.
        let n: i64 = conn.call(|c| c.query_row("SELECT 1", [], |r| r.get(0)).unwrap()).await;
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn typed_query_returns_error_on_panic() {
        let conn = Database::file(":memory:").connections(1).open().await.expect("open db");

        // A panicking closure routed through the typed API must surface as an Err,
        // never a panic in the caller's task.
        let result = conn
            .transaction(|_tx| -> rusqlite::Result<()> { panic!("boom inside transaction") })
            .await;
        assert!(result.is_err(), "panicking transaction should yield Err, got {result:?}");

        // And the pool is still usable afterwards.
        conn.execute_batch("CREATE TABLE t(x);").await.expect("pool still works");
    }

    #[test]
    fn test_update_query_creation() {
        let query = Foo::update_query(["name", "age"]);
        assert_eq!(query, "UPDATE foo SET name = ?, age = ? WHERE id = ?");
    }
}
