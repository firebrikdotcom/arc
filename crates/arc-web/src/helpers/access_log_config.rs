//! Startup selection for the opt-in durable read audit journal.
use arc_core::access_log::{AccessLogger, NoOpAccessLogger};
use arc_es_sqlite::{AccessLogPrivacy, SqliteAccessLogger};
use std::{io, path::PathBuf, sync::Arc};

/// Build from an injectable environment reader (also useful for startup tests).
/// Invalid or unavailable configured sinks prevent startup; never fall back.
pub async fn build_access_logger(
    get: impl Fn(&str) -> Option<String>,
) -> io::Result<Arc<dyn AccessLogger>> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid access-log configuration",
        )
    };
    let boolean = |key: &str| match get(key).as_deref() {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        _ => Err(invalid()),
    };
    let required = boolean("ACCESS_LOG_REQUIRED")?;
    match get("ACCESS_LOG_DRIVER").as_deref().unwrap_or("noop") {
        "noop" if !required => {
            tracing::warn!("access logging disabled: instrumented reads are not persisted");
            Ok(Arc::new(NoOpAccessLogger))
        }
        "sqlite" => {
            let path = PathBuf::from(get("ACCESS_LOG_SQLITE_PATH").ok_or_else(invalid)?);
            let timeout = get("ACCESS_LOG_BUSY_TIMEOUT_MS")
                .unwrap_or_else(|| "1000".into())
                .parse::<u32>()
                .map_err(|_| invalid())?;
            let privacy = AccessLogPrivacy {
                include_source_ip: boolean("ACCESS_LOG_INCLUDE_SOURCE_IP")?,
                include_user_agent: boolean("ACCESS_LOG_INCLUDE_USER_AGENT")?,
            };
            let logger = SqliteAccessLogger::open(path, privacy, timeout)
                .await
                .map_err(|_| io::Error::other("could not initialize durable access journal"))?;
            tracing::info!("durable SQLite access logging initialized");
            Ok(Arc::new(logger))
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn configuration_rejects_missing_required_unknown_and_unavailable() {
        for settings in [
            vec![("ACCESS_LOG_REQUIRED", "true")],
            vec![("ACCESS_LOG_DRIVER", "typo")],
            vec![("ACCESS_LOG_DRIVER", "sqlite")],
            vec![("ACCESS_LOG_REQUIRED", "yes")],
            vec![
                ("ACCESS_LOG_DRIVER", "sqlite"),
                ("ACCESS_LOG_SQLITE_PATH", "/missing-parent/audit.db"),
            ],
            vec![
                ("ACCESS_LOG_DRIVER", "sqlite"),
                ("ACCESS_LOG_SQLITE_PATH", ":memory:"),
            ],
        ] {
            assert!(build_access_logger(|key| settings
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string()))
            .await
            .is_err());
        }
        assert!(build_access_logger(|_| None).await.is_ok());
    }
}
