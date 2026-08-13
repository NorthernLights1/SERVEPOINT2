//! Verified SQLite backups (D17).
//!
//! Copying the database file is never safe while SQLite is in WAL mode: some
//! committed pages may still live in the `-wal` file.  This module therefore
//! uses `VACUUM INTO`, reopens the snapshot read-only, runs SQLite's full
//! integrity check, and compares the row count of every application table with
//! the source.  Only then is a path returned as a verified backup.
//!
//! [`BackupDestination`] is deliberately opaque.  A caller obtains one only
//! after proving that the target directory is on a different storage volume
//! from the live database.  Keeping that proof separate from the write also
//! makes it difficult for a later command handler to accidentally treat a
//! second directory on the till's internal disk as a backup.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    keep: usize,
}

impl RetentionPolicy {
    pub fn new(keep: usize) -> Result<Self, BackupError> {
        if keep == 0 {
            return Err(BackupError::InvalidRetention);
        }
        Ok(Self { keep })
    }
}

/// A directory proven to be on storage external to the live database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupDestination {
    directory: PathBuf,
    // Public construction always records the exact source database. `None`
    // exists only for private unit tests on CI's single filesystem.
    source_path: Option<PathBuf>,
}

impl BackupDestination {
    /// Validate and canonicalise the configured destination.
    ///
    /// The live database must be file-backed.  On Unix, device IDs distinguish
    /// mounted filesystems.  On Windows, different canonical path prefixes
    /// distinguish drive letters and UNC shares; an external volume mounted
    /// below the same drive is conservatively refused because the standard
    /// library cannot prove that it is separate storage.
    pub fn external_for(
        source: &Connection,
        directory: impl AsRef<Path>,
    ) -> Result<Self, BackupError> {
        let source_path = source_file_path(source)?;
        let directory = canonical_directory(directory.as_ref())?;
        if same_storage_volume(&source_path, &directory)? {
            return Err(BackupError::DestinationNotExternal { directory });
        }
        Ok(Self {
            directory,
            source_path: Some(source_path),
        })
    }

    pub fn path(&self) -> &Path {
        &self.directory
    }

    fn revalidate(&self, source: &Connection) -> Result<(), BackupError> {
        if let Some(expected_source) = &self.source_path {
            let source_path = source_file_path(source)?;
            if &source_path != expected_source {
                return Err(BackupError::DestinationForDifferentSource);
            }
            let directory = canonical_directory(&self.directory)?;
            if directory != self.directory || same_storage_volume(&source_path, &directory)? {
                return Err(BackupError::DestinationNotExternal { directory });
            }
        }
        Ok(())
    }
}

/// Evidence returned only after the written file has passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBackup {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub integrity: String,
    pub table_counts: BTreeMap<String, i64>,
    /// Old verified files removed after this snapshot passed verification.
    pub pruned: Vec<PathBuf>,
    /// Retention is best-effort after verification.  A verified current backup
    /// remains useful even when an old read-only file could not be removed;
    /// callers should put these warnings in the backup audit detail.
    pub retention_warnings: Vec<String>,
}

/// Stable, non-SQL diagnostic fields for the later audit/service layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRecord {
    pub phase: &'static str,
    pub target_path: Option<PathBuf>,
    pub detail: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("backup retention must keep at least one verified backup")]
    InvalidRetention,

    #[error("backup label must be 1-64 ASCII letters, digits, '-' or '_'")]
    InvalidLabel,

    #[error("backup timestamp must not be negative")]
    InvalidTimestamp,

    #[error("the live database is not file-backed, so an external destination cannot be proven")]
    SourceNotFileBacked,

    #[error("could not read the live database location: {source}")]
    SourceLocation {
        #[source]
        source: rusqlite::Error,
    },

    #[error("backup source does not resolve to a regular file: {path}")]
    InvalidSource { path: PathBuf },

    #[error("backup destination does not resolve to a directory: {path}")]
    InvalidDestination { path: PathBuf },

    #[error("backup destination is on the live database volume: {directory}")]
    DestinationNotExternal { directory: PathBuf },

    #[error("backup destination was validated for a different live database")]
    DestinationForDifferentSource,

    #[error("this platform cannot prove that the backup destination is external")]
    VolumeCheckUnsupported,

    #[error("could not inspect {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("refusing to replace an existing backup: {path}")]
    AlreadyExists { path: PathBuf },

    #[error("backup path cannot be represented safely for SQLite: {path}")]
    NonUnicodePath { path: PathBuf },

    #[error("could not create SQLite snapshot {path}: {source}")]
    Snapshot {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    #[error("backup verification failed for {path}: {detail}")]
    Verification { path: PathBuf, detail: String },
}

impl BackupError {
    /// Shape an error for a future append-only `backups`/audit write without
    /// exposing SQL or raw tool output.  Recording remains the service layer's
    /// responsibility because it owns the surrounding business transaction.
    pub fn failure_record(&self) -> FailureRecord {
        let (phase, target_path) = match self {
            Self::InvalidRetention
            | Self::InvalidLabel
            | Self::InvalidTimestamp
            | Self::SourceNotFileBacked
            | Self::SourceLocation { .. }
            | Self::InvalidSource { .. }
            | Self::InvalidDestination { .. }
            | Self::DestinationNotExternal { .. }
            | Self::DestinationForDifferentSource
            | Self::VolumeCheckUnsupported
            | Self::Inspect { .. }
            | Self::NonUnicodePath { .. } => ("VALIDATE", error_path(self)),
            Self::AlreadyExists { .. } | Self::Snapshot { .. } => ("SNAPSHOT", error_path(self)),
            Self::Verification { .. } => ("VERIFY", error_path(self)),
        };
        FailureRecord {
            phase,
            target_path,
            detail: self.to_string(),
        }
    }
}

fn error_path(error: &BackupError) -> Option<PathBuf> {
    match error {
        BackupError::InvalidSource { path }
        | BackupError::InvalidDestination { path }
        | BackupError::Inspect { path, .. }
        | BackupError::AlreadyExists { path }
        | BackupError::NonUnicodePath { path }
        | BackupError::Snapshot { path, .. }
        | BackupError::Verification { path, .. } => Some(path.clone()),
        BackupError::DestinationNotExternal { directory } => Some(directory.clone()),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Location {
    Source,
}

fn source_file_path(source: &Connection) -> Result<PathBuf, BackupError> {
    let path: String = source
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name = 'main'",
            [],
            |row| row.get(0),
        )
        .map_err(|source| BackupError::SourceLocation { source })?;
    if path.trim().is_empty() {
        return Err(BackupError::SourceNotFileBacked);
    }
    canonical_file(Path::new(&path), Location::Source)
}

/// Write and verify one backup into an already validated external directory.
///
/// `label` is part of the filename, usually a shift code.  It is strictly
/// limited rather than "sanitised": silently changing an identifier would
/// make operators believe a different shift was backed up.  `created_at` is
/// supplied by the service layer so the eventual audit row and filename share
/// one timestamp.
pub fn create_verified(
    source: &Connection,
    destination: &BackupDestination,
    label: &str,
    created_at: i64,
    retention: RetentionPolicy,
) -> Result<VerifiedBackup, BackupError> {
    validate_label(label)?;
    if created_at < 0 {
        return Err(BackupError::InvalidTimestamp);
    }

    // Recheck immediately before writing. A removable drive may have been
    // unplugged since setup, exposing an internal-disk directory at the same
    // mount path.
    destination.revalidate(source)?;

    let path = destination
        .directory
        .join(format!("servepoint-{label}-{created_at}.db"));
    match fs::symlink_metadata(&path) {
        Ok(_) => return Err(BackupError::AlreadyExists { path }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(BackupError::Inspect { path, source }),
    }
    let target_text = path
        .to_str()
        .ok_or_else(|| BackupError::NonUnicodePath { path: path.clone() })?;

    let source_version = data_version(source).map_err(|source| BackupError::Snapshot {
        path: path.clone(),
        source,
    })?;
    let source_counts = table_counts(source).map_err(|source| BackupError::Snapshot {
        path: path.clone(),
        source,
    })?;

    if let Err(source) = source.execute("VACUUM main INTO ?1", [target_text]) {
        remove_unverified(&path);
        return Err(BackupError::Snapshot { path, source });
    }

    let current_version = data_version(source).map_err(|source| {
        remove_unverified(&path);
        BackupError::Snapshot {
            path: path.clone(),
            source,
        }
    })?;
    if current_version != source_version {
        remove_unverified(&path);
        return Err(BackupError::Verification {
            path,
            detail: "the source changed while the snapshot was being written".into(),
        });
    }

    let verified = verify_snapshot(&path, &source_counts).map_err(|detail| {
        remove_unverified(&path);
        BackupError::Verification {
            path: path.clone(),
            detail,
        }
    })?;

    let (pruned, retention_warnings) = prune_old_backups(&destination.directory, &path, retention);
    Ok(VerifiedBackup {
        path,
        size_bytes: verified.size_bytes,
        integrity: "ok".into(),
        table_counts: verified.table_counts,
        pruned,
        retention_warnings,
    })
}

#[derive(Debug)]
struct Verification {
    size_bytes: u64,
    table_counts: BTreeMap<String, i64>,
}

fn verify_snapshot(path: &Path, expected: &BTreeMap<String, i64>) -> Result<Verification, String> {
    let size_bytes = fs::metadata(path)
        .map_err(|error| format!("could not read the snapshot metadata: {error}"))?
        .len();
    if size_bytes == 0 {
        return Err("the snapshot file is empty".into());
    }

    let backup = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("could not reopen the snapshot read-only: {error}"))?;

    let messages = integrity_messages(&backup)
        .map_err(|error| format!("integrity_check could not be read: {error}"))?;
    if messages.as_slice() != ["ok"] {
        return Err(format!("integrity_check reported: {}", messages.join("; ")));
    }

    let actual = table_counts(&backup)
        .map_err(|error| format!("authoritative row counts could not be read: {error}"))?;
    if actual != *expected {
        return Err(describe_count_mismatch(expected, &actual));
    }

    Ok(Verification {
        size_bytes,
        table_counts: actual,
    })
}

fn integrity_messages(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare("PRAGMA integrity_check")?;
    let messages = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(messages)
}

fn data_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA data_version", [], |row| row.get(0))
}

fn table_counts(conn: &Connection) -> rusqlite::Result<BTreeMap<String, i64>> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_schema
          WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
          ORDER BY name",
    )?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut counts = BTreeMap::new();
    for name in names {
        let quoted = name.replace('"', "\"\"");
        let count = conn.query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), [], |row| {
            row.get(0)
        })?;
        counts.insert(name, count);
    }
    Ok(counts)
}

fn describe_count_mismatch(
    expected: &BTreeMap<String, i64>,
    actual: &BTreeMap<String, i64>,
) -> String {
    let mut differences = Vec::new();
    for name in expected.keys().chain(actual.keys()) {
        if expected.get(name) != actual.get(name) {
            differences.push(format!(
                "{name}: source={}, backup={}",
                expected.get(name).map_or("missing".into(), i64::to_string),
                actual.get(name).map_or("missing".into(), i64::to_string)
            ));
        }
    }
    differences.sort();
    differences.dedup();
    format!("row-count mismatch ({})", differences.join(", "))
}

fn validate_label(label: &str) -> Result<(), BackupError> {
    let valid = !label.is_empty()
        && label.len() <= 64
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(BackupError::InvalidLabel)
    }
}

fn canonical_file(path: &Path, location: Location) -> Result<PathBuf, BackupError> {
    let canonical = fs::canonicalize(path).map_err(|source| BackupError::Inspect {
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_file() {
        return match location {
            Location::Source => Err(BackupError::InvalidSource { path: canonical }),
        };
    }
    Ok(canonical)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, BackupError> {
    let canonical = fs::canonicalize(path).map_err(|source| BackupError::Inspect {
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(BackupError::InvalidDestination { path: canonical });
    }
    Ok(canonical)
}

#[cfg(unix)]
fn same_storage_volume(source: &Path, destination: &Path) -> Result<bool, BackupError> {
    use std::os::unix::fs::MetadataExt as _;

    let source_device = fs::metadata(source)
        .map_err(|error| BackupError::Inspect {
            path: source.to_path_buf(),
            source: error,
        })?
        .dev();
    let destination_device = fs::metadata(destination)
        .map_err(|error| BackupError::Inspect {
            path: destination.to_path_buf(),
            source: error,
        })?
        .dev();
    Ok(source_device == destination_device)
}

#[cfg(windows)]
fn same_storage_volume(source: &Path, destination: &Path) -> Result<bool, BackupError> {
    fn prefix(path: &Path) -> Result<String, BackupError> {
        use std::path::Component;
        match path.components().next() {
            Some(Component::Prefix(value)) => Ok(value.as_os_str().to_string_lossy().to_string()),
            _ => Err(BackupError::VolumeCheckUnsupported),
        }
    }
    Ok(prefix(source)?.eq_ignore_ascii_case(&prefix(destination)?))
}

#[cfg(not(any(unix, windows)))]
fn same_storage_volume(_source: &Path, _destination: &Path) -> Result<bool, BackupError> {
    Err(BackupError::VolumeCheckUnsupported)
}

fn remove_unverified(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

fn prune_old_backups(
    directory: &Path,
    current: &Path,
    retention: RetentionPolicy,
) -> (Vec<PathBuf>, Vec<String>) {
    let mut warnings = Vec::new();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("could not scan old backups: {error}"));
            return (Vec::new(), warnings);
        }
    };
    let mut prior = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!(
                    "could not inspect a backup-directory entry: {error}"
                ));
                continue;
            }
        };
        let path = entry.path();
        if path == current || !has_managed_backup_name(&entry) {
            continue;
        }
        if let Err(detail) = verify_prior_backup(&path) {
            warnings.push(format!(
                "{} failed verification and was excluded from retention: {detail}",
                path.display()
            ));
            continue;
        }
        prior.push((backup_timestamp(&path).unwrap_or(i64::MIN), path));
    }
    prior.sort_by(|left, right| right.cmp(left));

    let mut pruned = Vec::new();
    for (_, path) in prior.into_iter().skip(retention.keep.saturating_sub(1)) {
        match fs::remove_file(&path) {
            Ok(()) => pruned.push(path),
            Err(error) => warnings.push(format!("could not remove {}: {error}", path.display())),
        }
    }
    (pruned, warnings)
}

fn has_managed_backup_name(entry: &fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_file()) && backup_timestamp(&entry.path()).is_some()
}

fn verify_prior_backup(path: &Path) -> Result<(), String> {
    let size = fs::metadata(path)
        .map_err(|error| format!("could not read metadata: {error}"))?
        .len();
    if size == 0 {
        return Err("file is empty".into());
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("could not reopen read-only: {error}"))?;
    let messages = integrity_messages(&conn).map_err(|error| error.to_string())?;
    if messages.as_slice() != ["ok"] {
        return Err(format!("integrity_check reported: {}", messages.join("; ")));
    }
    let identifying_tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
              WHERE type = 'table' AND name IN ('schema_migrations', 'backups')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if identifying_tables != 2 {
        return Err("file is not a ServePoint database".into());
    }
    Ok(())
}

fn backup_timestamp(path: &Path) -> Option<i64> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix("servepoint-")?.strip_suffix(".db")?;
    let (_, timestamp) = rest.rsplit_once('-')?;
    if timestamp.is_empty() || !timestamp.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    timestamp.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_must_keep_at_least_one_backup() {
        assert!(matches!(
            RetentionPolicy::new(0),
            Err(BackupError::InvalidRetention)
        ));
        assert_eq!(
            RetentionPolicy::new(3).unwrap(),
            RetentionPolicy { keep: 3 }
        );
    }

    #[test]
    fn creates_a_verified_snapshot_with_authoritative_row_counts() {
        let source = crate::db::open_in_memory().unwrap();
        source
            .execute(
                "UPDATE settings SET value = 'Snapshot Venue' WHERE key = 'receipt.business_name'",
                [],
            )
            .unwrap();
        let expected_settings: i64 = source
            .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
            .unwrap();
        let expected_migrations: i64 = source
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        let directory = test_directory("success");
        // `BackupDestination` is opaque outside this module.  Unit tests can
        // construct the token directly because CI does not have a second
        // physical volume; validation itself is exercised separately below.
        let destination = BackupDestination {
            directory: directory.clone(),
            source_path: None,
        };

        let result = create_verified(
            &source,
            &destination,
            "shift-000042",
            1_754_000_000_000,
            RetentionPolicy::new(3).unwrap(),
        )
        .unwrap();

        assert_eq!(result.integrity, "ok");
        assert!(result.size_bytes > 0);
        assert_eq!(
            result.table_counts.get("settings"),
            Some(&expected_settings)
        );
        assert_eq!(
            result.table_counts.get("schema_migrations"),
            Some(&expected_migrations)
        );
        assert!(result.path.starts_with(&directory));
        assert_eq!(
            result.path.extension().and_then(|value| value.to_str()),
            Some("db")
        );
        let backup = rusqlite::Connection::open(&result.path).unwrap();
        let venue: String = backup
            .query_row(
                "SELECT value FROM settings WHERE key = 'receipt.business_name'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(venue, "Snapshot Venue");

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_in_memory_source_cannot_claim_an_external_destination() {
        let source = crate::db::open_in_memory().unwrap();
        let directory = test_directory("memory-source");

        let error = BackupDestination::external_for(&source, &directory).unwrap_err();

        assert!(matches!(error, BackupError::SourceNotFileBacked));
        assert_eq!(error.failure_record().phase, "VALIDATE");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_second_directory_on_the_database_disk_is_not_external() {
        let directory = test_directory("same-volume");
        let source_path = directory.join("live.db");
        let source = crate::db::open(&source_path).unwrap();

        let error = BackupDestination::external_for(&source, &directory).unwrap_err();

        assert!(matches!(error, BackupError::DestinationNotExternal { .. }));
        assert_eq!(
            error.failure_record().target_path.as_deref(),
            Some(directory.as_path())
        );
        drop(source);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_validated_destination_cannot_be_reused_for_another_database() {
        let directory = test_directory("wrong-source");
        let first_path = directory.join("first.db");
        let second_path = directory.join("second.db");
        let first = crate::db::open(&first_path).unwrap();
        let second = crate::db::open(&second_path).unwrap();
        let destination = BackupDestination {
            directory: directory.clone(),
            source_path: Some(std::fs::canonicalize(&first_path).unwrap()),
        };

        let error = create_verified(
            &second,
            &destination,
            "shift",
            1,
            RetentionPolicy::new(2).unwrap(),
        )
        .unwrap_err();

        assert!(matches!(error, BackupError::DestinationForDifferentSource));
        drop((first, second));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unsafe_labels_are_rejected_before_a_file_is_created() {
        let source = crate::db::open_in_memory().unwrap();
        let directory = test_directory("unsafe-label");
        let destination = BackupDestination {
            directory: directory.clone(),
            source_path: None,
        };

        let error = create_verified(
            &source,
            &destination,
            "../../other-venue",
            1,
            RetentionPolicy::new(2).unwrap(),
        )
        .unwrap_err();

        assert!(matches!(error, BackupError::InvalidLabel));
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_existing_path_is_never_overwritten() {
        let source = crate::db::open_in_memory().unwrap();
        let directory = test_directory("no-overwrite");
        let destination = BackupDestination {
            directory: directory.clone(),
            source_path: None,
        };
        let existing = directory.join("servepoint-shift-1.db");
        std::fs::write(&existing, b"keep me").unwrap();

        let error = create_verified(
            &source,
            &destination,
            "shift",
            1,
            RetentionPolicy::new(2).unwrap(),
        )
        .unwrap_err();

        assert!(matches!(error, BackupError::AlreadyExists { .. }));
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
        assert_eq!(error.failure_record().phase, "SNAPSHOT");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn retention_keeps_the_current_and_newest_prior_verified_files_only() {
        let source = crate::db::open_in_memory().unwrap();
        let directory = test_directory("retention");
        let destination = BackupDestination {
            directory: directory.clone(),
            source_path: None,
        };
        let policy = RetentionPolicy::new(2).unwrap();
        let first = create_verified(&source, &destination, "shift", 1, policy.clone()).unwrap();
        let second = create_verified(&source, &destination, "shift", 2, policy.clone()).unwrap();
        let unrelated = directory.join("venue-notes.db");
        std::fs::write(&unrelated, b"not managed by ServePoint backup").unwrap();

        let third = create_verified(&source, &destination, "shift", 3, policy).unwrap();

        assert!(!first.path.exists());
        assert!(second.path.exists());
        assert!(third.path.exists());
        assert!(unrelated.exists());
        assert_eq!(third.pruned, vec![first.path]);
        assert!(third.retention_warnings.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn retention_does_not_count_a_crash_remnant_as_a_verified_backup() {
        let source = crate::db::open_in_memory().unwrap();
        let directory = test_directory("retention-remnant");
        let destination = BackupDestination {
            directory: directory.clone(),
            source_path: None,
        };
        let policy = RetentionPolicy::new(2).unwrap();
        let valid = create_verified(&source, &destination, "shift", 1, policy.clone()).unwrap();
        let remnant = directory.join("servepoint-shift-2.db");
        std::fs::write(&remnant, b"an interrupted VACUUM INTO").unwrap();

        let current = create_verified(&source, &destination, "shift", 3, policy).unwrap();

        assert!(valid.path.exists(), "the older verified backup must remain");
        assert!(current.path.exists());
        assert!(
            remnant.exists(),
            "unknown data is warned about, not deleted"
        );
        assert_eq!(current.retention_warnings.len(), 1);
        assert!(current.retention_warnings[0].contains("failed verification"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn verification_rejects_a_snapshot_with_different_authoritative_counts() {
        let source = crate::db::open_in_memory().unwrap();
        let expected = table_counts(&source).unwrap();
        let directory = test_directory("count-mismatch");
        let path = directory.join("candidate.db");
        source
            .execute("VACUUM main INTO ?1", [path.to_str().unwrap()])
            .unwrap();
        let changed = rusqlite::Connection::open(&path).unwrap();
        changed
            .execute("CREATE TABLE unexpected_row (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        changed
            .execute("INSERT INTO unexpected_row DEFAULT VALUES", [])
            .unwrap();
        drop(changed);

        let detail = verify_snapshot(&path, &expected).unwrap_err();

        assert!(detail.contains("row-count mismatch"), "got: {detail}");
        assert!(detail.contains("unexpected_row"), "got: {detail}");
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn test_directory(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "servepoint-backup-test-{}-{label}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        directory
    }
}
