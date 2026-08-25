//! Downloading, initializing and supervising a local MariaDB for Cellar.
//!
//! Only reached when `[mariadb].managed = true`. Deliberately independent of
//! `cellar-store`: this crate never speaks the MySQL wire protocol itself,
//! it shells out to the bundled `mariadb`/`mariadb-admin`/`mariadb-install-db`
//! client tools for the one-time bootstrap in [`provision`], so `cellar-store`
//! stays the only crate in the workspace that opens a `sqlx` connection.
//! What this crate produces is a `mysql://` URL of the same shape
//! `CELLAR_DATABASE_URL` already expects, whether that URL points at a
//! managed instance or a remote one; see `[mariadb]` in
//! `cellar_core::config` for why the two stay decoupled.

pub mod backup;
pub mod credentials;
pub mod fetch;
pub mod install;
pub mod provision;
pub mod release;
pub mod supervisor;

pub use backup::{BackupError, create as backup};
pub use provision::{Marker, ProvisionError, provision, read_marker};
pub use supervisor::{Control, Handle, Status, Supervisor};
