use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub struct Sdk {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Sdk {
    pub fn start(root: &Path, script: &str) -> Result<Self> {
        let node = std::env::var("NODE_BIN").unwrap_or_else(|_| "node".into());
        let mut child = Command::new(node)
            .arg(root.join(script))
            .current_dir(root)
            // Do not inherit key paths, private keys, database/API credentials or NODE_OPTIONS.
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("start Node SDK probe")?;
        let input = child.stdin.take().context("Node stdin unavailable")?;
        let output = BufReader::new(child.stdout.take().context("Node stdout unavailable")?);
        Ok(Self {
            child,
            input,
            output,
        })
    }

    pub async fn request(&mut self, request: Value) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(240), async {
            self.input
                .write_all(serde_json::to_string(&request)?.as_bytes())
                .await?;
            self.input.write_all(b"\n").await?;
            self.input.flush().await?;
            let mut line = String::new();
            self.output
                .read_line(&mut line)
                .await
                .context("read SDK response")?;
            ensure!(!line.is_empty(), "Node SDK exited without a response");
            let response: Value = serde_json::from_str(&line).context("invalid SDK response")?;
            // Never echo an untrusted SDK error, which may contain private payloads.
            ensure!(response.get("error").is_none(), "Node SDK operation failed");
            Ok(response)
        })
        .await
        .context("Node SDK request timed out")?
    }

    pub async fn close(mut self) -> Result<()> {
        self.request(serde_json::json!({"command":"quit"})).await?;
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .context("Node SDK shutdown timed out")??;
        ensure!(status.success(), "Node SDK did not exit successfully");
        Ok(())
    }
}
