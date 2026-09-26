//! The unit of atomicity a query gets, which depends on what the writer already holds.
//!
//! Every query that must not half-apply asks for a transaction. With nothing
//! open, that is what it gets. But the writer groups concurrent calls into one
//! commit, and then the transaction it holds is the *batch's* — a `BEGIN` inside
//! it fails outright ("cannot start a transaction within a transaction"), which
//! would turn every grouped write into an error. A savepoint is the same
//! guarantee nested: rolled back on error, merged into the batch on success, and
//! the batch's commit is what makes it durable.
//!
//! So the choice belongs here, once, instead of in every query: [`Txn::Own`] for
//! a call on its own, [`Txn::Nested`] for a call inside a batch. Both roll
//! themselves back if they are dropped without a commit, so callers keep using
//! `?` and a plain `tx.commit()`.

use std::ops::Deref;

use rusqlite::{Connection, Result as SqlResult, Savepoint, Transaction};

/// A transaction or, when one is already open, a savepoint inside it.
pub(crate) enum Txn<'conn> {
    /// Nothing was open: a real transaction. Committing it is durable on its own.
    Own(Transaction<'conn>),
    /// The writer is mid-batch: a savepoint. Committing it merges the work into
    /// the batch, and the batch's commit is what reaches the disk.
    Nested(Savepoint<'conn>),
}

impl<'conn> Txn<'conn> {
    /// Open the narrowest unit of atomicity `conn` can offer.
    pub(crate) fn open(conn: &'conn mut Connection) -> SqlResult<Self> {
        if conn.is_autocommit() {
            Ok(Self::Own(conn.transaction()?))
        } else {
            Ok(Self::Nested(conn.savepoint()?))
        }
    }

    /// Keep the work. For [`Txn::Own`] that is the commit; for [`Txn::Nested`]
    /// it hands the work to whoever owns the enclosing transaction.
    pub(crate) fn commit(self) -> SqlResult<()> {
        match self {
            Self::Own(tx) => tx.commit(),
            Self::Nested(sp) => sp.commit(),
        }
    }
}

impl Deref for Txn<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        match self {
            Self::Own(tx) => tx,
            Self::Nested(sp) => sp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Txn;
    use rusqlite::Connection;

    fn table(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM t;", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn a_lone_query_commits_its_own_transaction() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (v INTEGER);").unwrap();
        let tx = Txn::open(&mut conn).unwrap();
        tx.execute("INSERT INTO t VALUES (1);", []).unwrap();
        tx.commit().unwrap();
        assert_eq!(table(&conn), 1);
        assert!(conn.is_autocommit());
    }

    #[test]
    fn a_dropped_unit_of_work_leaves_nothing_behind() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (v INTEGER);").unwrap();
        let tx = Txn::open(&mut conn).unwrap();
        tx.execute("INSERT INTO t VALUES (1);", []).unwrap();
        drop(tx);
        assert_eq!(table(&conn), 0);
        assert!(conn.is_autocommit());
    }

    #[test]
    fn a_query_inside_a_batch_gets_a_savepoint_that_commits_into_the_batch() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (v INTEGER); BEGIN;")
            .unwrap();

        let tx = Txn::open(&mut conn).unwrap();
        tx.execute("INSERT INTO t VALUES (1);", []).unwrap();
        tx.commit().unwrap();
        assert!(
            !conn.is_autocommit(),
            "committing a nested savepoint must leave the batch open"
        );

        // The batch is still open, so the rows are not visible from outside it.
        let visible: i64 = conn
            .query_row("SELECT COUNT(*) FROM t;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(visible, 1, "the writer reads through the same connection");

        conn.execute_batch("COMMIT;").unwrap();
        assert!(conn.is_autocommit());
    }

    #[test]
    fn a_failed_query_inside_a_batch_rolls_back_only_itself() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (v INTEGER NOT NULL);")
            .unwrap();
        conn.execute_batch("BEGIN; INSERT INTO t VALUES (1);")
            .unwrap();

        let failed = Txn::open(&mut conn).unwrap();
        failed
            .execute("INSERT INTO t VALUES (NULL);", [])
            .unwrap_err();
        drop(failed);

        conn.execute_batch("INSERT INTO t VALUES (2);").unwrap();
        conn.execute_batch("COMMIT;").unwrap();
        assert_eq!(table(&conn), 2, "the batch kept both good rows");
    }
}
