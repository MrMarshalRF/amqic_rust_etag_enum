//! Local SQLite snapshot journal. Transactions may contain local state changes
//! only; external effects require a separately committed intent (see `api`).
use crate::{check_id, Error, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::OpenOptions,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const APPLICATION_ID: i64 = 0x414d5145;
const VERSION: i64 = 1;
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
const TABLE: &str = "CREATE TABLE snapshots (namespace TEXT NOT NULL, scope TEXT NOT NULL, version INTEGER NOT NULL, payload BLOB NOT NULL, checksum TEXT NOT NULL, PRIMARY KEY(namespace, scope)) WITHOUT ROWID";

/// Clones share a connection. Independent opens/processes coordinate through
/// SQLite's BEGIN IMMEDIATE and a two-second busy timeout.
#[derive(Clone)]
pub struct Journal {
    connection: Arc<Mutex<Connection>>,
}

impl Journal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        // Reserve a new file exclusively. Existing empty/foreign/corrupt files
        // are never treated as a request to create a fresh ledger.
        let fresh = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => {
                file.sync_all().map_err(|_| Error::Storage)?;
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err(Error::Storage),
        };
        let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| Error::Storage)?;
        connection
            .busy_timeout(Duration::from_secs(2))
            .map_err(|_| Error::Storage)?;
        if !fresh {
            validate_schema(&connection)?;
        }
        let mode: String = connection
            .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
            .map_err(|_| Error::Storage)?;
        if !mode.eq_ignore_ascii_case("delete") {
            return Err(Error::Storage);
        }
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|_| Error::Storage)?;
        if fresh {
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| Error::Storage)?;
            tx.execute_batch(TABLE).map_err(|_| Error::Storage)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(|_| Error::Storage)?;
            tx.pragma_update(None, "user_version", VERSION)
                .map_err(|_| Error::Storage)?;
            tx.commit().map_err(|_| Error::Storage)?;
        }
        validate_schema(&connection)?;
        verify_pragmas(&connection)?;
        let integrity: String = connection
            .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
            .map_err(|_| Error::Storage)?;
        if integrity != "ok" {
            return Err(Error::Storage);
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn read<T: DeserializeOwned>(&self, namespace: &str, scope: &str) -> Result<Option<T>> {
        check_id(namespace)?;
        check_id(scope)?;
        let mut connection = self.connection.lock().map_err(|_| Error::Storage)?;
        let tx = connection.transaction().map_err(|_| Error::Storage)?;
        validate_schema(&tx)?;
        let value = load(&tx, namespace, scope)?;
        tx.commit().map_err(|_| Error::Storage)?;
        Ok(value)
    }

    /// The closure is executed once after writer admission. It must not perform
    /// external effects. No successful return is exposed until COMMIT succeeds.
    pub fn transact<T: Clone + Serialize + DeserializeOwned, R>(
        &self,
        namespace: &str,
        scope: &str,
        initial: &T,
        f: impl FnOnce(&mut T) -> Result<R>,
    ) -> Result<R> {
        check_id(namespace)?;
        check_id(scope)?;
        let mut connection = self.connection.lock().map_err(|_| Error::Storage)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error::Storage)?;
        validate_schema(&tx)?;
        verify_pragmas(&tx)?;
        let mut state = load(&tx, namespace, scope)?.unwrap_or_else(|| initial.clone());
        let result = f(&mut state)?;
        let payload = bounded_json(&state)?;
        let checksum = checksum(namespace, scope, &payload)?;
        tx.execute("INSERT INTO snapshots(namespace,scope,version,payload,checksum) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(namespace,scope) DO UPDATE SET version=excluded.version,payload=excluded.payload,checksum=excluded.checksum",
            params![namespace, scope, VERSION, payload, checksum]).map_err(|_| Error::Storage)?;
        tx.commit().map_err(|_| Error::Storage)?;
        Ok(result)
    }
}

fn validate_schema(connection: &Connection) -> Result<()> {
    let app: i64 = connection
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    if app != APPLICATION_ID || version != VERSION {
        return Err(Error::Storage);
    }
    let objects: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    let bounded: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND length(type)<=16 AND length(name)<=128 AND length(sql)<=4096", [], |r| r.get(0)).map_err(|_| Error::Storage)?;
    if objects != 1 || bounded != 1 {
        return Err(Error::Storage);
    }
    let mut stmt = connection
        .prepare(
            "SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(|_| Error::Storage)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|_| Error::Storage)?;
    let objects: Vec<_> = rows
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Storage)?;
    if objects != vec![("table".to_owned(), "snapshots".to_owned(), TABLE.to_owned())] {
        return Err(Error::Storage);
    }
    Ok(())
}
fn verify_pragmas(connection: &Connection) -> Result<()> {
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    let sync: i64 = connection
        .pragma_query_value(None, "synchronous", |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    if mode.eq_ignore_ascii_case("delete") && sync == 2 {
        Ok(())
    } else {
        Err(Error::Storage)
    }
}
fn checksum(namespace: &str, scope: &str, payload: &[u8]) -> Result<String> {
    let prefix = serde_json::to_vec(&("amqic.etag.journal.v1", namespace, scope, VERSION))
        .map_err(|_| Error::Storage)?;
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(prefix);
    hash.update([0]);
    hash.update(payload);
    Ok(format!("{:x}", hash.finalize()))
}
fn load<T: DeserializeOwned>(
    connection: &Connection,
    namespace: &str,
    scope: &str,
) -> Result<Option<T>> {
    let lengths: Option<(i64, i64)> = connection.query_row("SELECT length(payload),length(checksum) FROM snapshots WHERE namespace=?1 AND scope=?2", params![namespace, scope], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|_| Error::Storage)?;
    let Some((length, digest_length)) = lengths else {
        return Ok(None);
    };
    if length < 0 || length as u64 > MAX_STATE_BYTES as u64 || digest_length != 64 {
        return Err(Error::Storage);
    }
    let (version, payload, digest): (i64, Vec<u8>, String) = connection
        .query_row(
            "SELECT version,payload,checksum FROM snapshots WHERE namespace=?1 AND scope=?2",
            params![namespace, scope],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| Error::Storage)?;
    if version != VERSION
        || payload.len() > MAX_STATE_BYTES
        || checksum(namespace, scope, &payload)? != digest
    {
        return Err(Error::Storage);
    }
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|_| Error::Storage)
}
fn bounded_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_STATE_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("state bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded(Vec::new());
    serde_json::to_writer(&mut out, value).map_err(|_| Error::Storage)?;
    Ok(out.0)
}
