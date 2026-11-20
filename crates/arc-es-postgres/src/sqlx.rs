//! The subset of the `sqlx` 0.8 facade this crate uses, taken from `sqlx-core` and
//! `sqlx-postgres` directly.
//!
//! The `sqlx` facade crate lists its MySQL and SQLite drivers as optional dependencies,
//! and Cargo records optional dependencies in `Cargo.lock` even when their features are
//! off. Depending on the facade therefore pinned `libsqlite3-sys` to the vulnerable line
//! `sqlx-sqlite` 0.8 requires. The facade only re-exports these items, so the types are
//! identical: an application that depends on `sqlx` 0.8 passes its `sqlx::PgPool` and
//! receives `&mut sqlx::PgConnection` exactly as before.
#![allow(unused_imports)]

pub(crate) use sqlx_core::connection::Connection;
pub(crate) use sqlx_core::query::query;
pub(crate) use sqlx_core::query_scalar::query_scalar;
pub(crate) use sqlx_core::raw_sql::raw_sql;
pub(crate) use sqlx_core::row::Row;
pub(crate) use sqlx_core::transaction::Transaction;
pub(crate) use sqlx_core::Error;
pub(crate) use sqlx_postgres::{PgConnection, Postgres};

pub(crate) mod postgres {
    pub(crate) use sqlx_postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgRow};
}
