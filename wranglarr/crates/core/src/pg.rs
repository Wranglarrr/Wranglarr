// SPDX-License-Identifier: GPL-3.0-or-later
//! pg_dump / pg_restore / psql wrappers (the CLI tools, no driver needed).

use std::path::Path;

use tokio::process::Command;

use crate::config::PostgresConfig;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PgError(pub String);

impl From<PgError> for crate::error::Error {
    fn from(e: PgError) -> Self {
        crate::error::Error::upstream(e.0)
    }
}

pub type PgResult<T> = Result<T, PgError>;

/// Only plain database names reach SQL text (they come from wranglarr.toml).
fn ident_ok(db: &str) -> PgResult<()> {
    if !db.is_empty() && db.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)) {
        Ok(())
    } else {
        Err(PgError(format!("bad database name {db:?}")))
    }
}

pub struct Postgres {
    pub cfg: PostgresConfig,
}

impl Postgres {
    pub fn new(cfg: PostgresConfig) -> Postgres {
        Postgres { cfg }
    }

    async fn run(&self, args: &[&str]) -> PgResult<String> {
        let timeout = match args.first().copied() {
            Some("pg_isready") => std::time::Duration::from_secs(10),
            Some("psql") => std::time::Duration::from_secs(30),
            _ => std::time::Duration::from_secs(900),
        };
        self.run_with_timeout(args, timeout).await
    }

    /// Bound both process lifetime and pipe memory. Dropping a cancelled
    /// command kills its child instead of leaving a dump running in the background.
    async fn run_with_timeout(&self, args: &[&str], timeout: std::time::Duration) -> PgResult<String> {
        use std::process::Stdio;
        use tokio::io::AsyncReadExt;
        async fn read_output(pipe: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
            const LIMIT: u64 = 16 * 1024 * 1024;
            let mut bytes = Vec::new();
            pipe.take(LIMIT + 1).read_to_end(&mut bytes).await?;
            if bytes.len() as u64 > LIMIT {
                return Err(std::io::Error::other("process output exceeds 16 MiB"));
            }
            Ok(bytes)
        }
        let program = args.first().ok_or_else(|| PgError("empty Postgres command".into()))?;
        let mut cmd = Command::new(program);
        cmd.args(&args[1..]).envs(self.cfg.env()).kill_on_drop(true).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| PgError(format!("{program}: {e}")))?;
        let stdout = child.stdout.take().ok_or_else(|| PgError("missing stdout pipe".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| PgError("missing stderr pipe".into()))?;
        let result = tokio::time::timeout(timeout, async {
            tokio::try_join!(child.wait(), read_output(stdout), read_output(stderr))
        })
        .await
        .map_err(|_| PgError(format!("{program}: timed out")))?
        .map_err(|e| PgError(format!("{program}: {e}")))?;
        let (status, stdout, stderr) = result;
        if !status.success() {
            let err = String::from_utf8_lossy(&stderr);
            // Tools sometimes echo connection parameters. Redact before taking
            // the tail so clipping cannot leave part of a password behind.
            let err = crate::http::redact(err.into_owned(), &[&self.cfg.password]);
            let tail: String = err.trim().chars().rev().take(800).collect::<Vec<_>>().into_iter().rev().collect();
            let code = status.code().map_or_else(|| "signal".to_string(), |c| c.to_string());
            return Err(PgError(format!("{} failed ({code}): {tail}", args[..args.len().min(2)].join(" "))));
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }

    pub async fn available(&self) -> bool {
        self.run(&["pg_isready", "-d", "postgres"]).await.is_ok()
    }

    pub async fn query(&self, sql: &str, db: &str) -> PgResult<String> {
        Ok(self.run(&["psql", "-v", "ON_ERROR_STOP=1", "-tAq", "-d", db, "-c", sql]).await?.trim().to_string())
    }

    pub async fn server_version(&self) -> String {
        self.query("SHOW server_version", "postgres").await.unwrap_or_default()
    }

    pub async fn database_exists(&self, db: &str) -> PgResult<bool> {
        ident_ok(db)?;
        Ok(self.query(&format!("SELECT 1 FROM pg_database WHERE datname = '{db}'"), "postgres").await? == "1")
    }

    pub async fn database_size(&self, db: &str) -> i64 {
        if ident_ok(db).is_err() {
            return 0;
        }
        self.query(&format!("SELECT pg_database_size('{db}')"), "postgres")
            .await
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    pub async fn ensure_database(&self, db: &str) -> PgResult<()> {
        if !self.database_exists(db).await? {
            tracing::info!("creating database {db}");
            let user = self.cfg.user.replace('"', "\"\"");
            self.query(&format!("CREATE DATABASE \"{db}\" OWNER \"{user}\""), "postgres").await?;
        }
        Ok(())
    }

    pub async fn dump(&self, db: &str, dest: &Path) -> PgResult<u64> {
        if let Some(p) = dest.parent() {
            tokio::fs::create_dir_all(p).await.map_err(|e| PgError(format!("{}: {e}", p.display())))?;
        }
        let d = dest.to_string_lossy();
        self.run(&["pg_dump", "-Fc", "--no-owner", "-f", &d, db]).await?;
        tokio::fs::metadata(dest).await.map(|m| m.len()).map_err(|e| PgError(format!("{d}: {e}")))
    }

    pub async fn dump_globals(&self, dest: &Path) -> PgResult<u64> {
        if let Some(p) = dest.parent() {
            tokio::fs::create_dir_all(p).await.map_err(|e| PgError(format!("{}: {e}", p.display())))?;
        }
        let d = dest.to_string_lossy();
        self.run(&["pg_dumpall", "--globals-only", "-f", &d]).await?;
        tokio::fs::metadata(dest).await.map(|m| m.len()).map_err(|e| PgError(format!("{d}: {e}")))
    }

    pub async fn restore(&self, db: &str, dump: &Path) -> PgResult<()> {
        self.ensure_database(db).await?;
        let d = dump.to_string_lossy();
        self.run(&["pg_restore", "--clean", "--if-exists", "--no-owner", "--single-transaction", "-d", db, &d]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn postgres() -> Postgres {
        Postgres::new(PostgresConfig {
            host: "127.0.0.1".into(),
            port: 5432,
            user: "user".into(),
            password: "test-password".into(),
            container: String::new(),
        })
    }

    #[tokio::test]
    async fn failed_tool_redacts_password() {
        let err = postgres().run(&["/bin/sh", "-c", "printf test-password >&2; exit 1"]).await.expect_err("failure");
        assert!(!err.to_string().contains("test-password"));
        assert!(err.to_string().contains("[redacted]"));
    }

    #[tokio::test]
    async fn hung_tool_has_deadline() {
        let err = postgres()
            .run_with_timeout(&["/bin/sleep", "10"], std::time::Duration::from_millis(20))
            .await
            .expect_err("timeout");
        assert!(err.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn tool_output_is_bounded() {
        let err = postgres().run(&["/bin/sh", "-c", "head -c 16777217 /dev/zero"]).await.expect_err("limit");
        assert!(err.to_string().contains("exceeds"));
    }
}
