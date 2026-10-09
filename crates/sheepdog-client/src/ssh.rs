use anyhow::{Result, bail, ensure};
use sheepdog_proto::Bootstrap;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

async fn output(mut command: Command, deadline: Duration) -> Result<Vec<u8>> {
    tokio::time::timeout(deadline, async {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(bytes.len() <= 64 * 1024, "SSH reply exceeds budget");
        ensure!(
            child.wait().await?.success(),
            "SSH failed; verify the target and that sheepdog-server serve is running"
        );
        Ok(bytes)
    })
    .await?
}

#[derive(Clone, Debug)]
pub struct SshOptions {
    /// Parsed as argv, never executed through a local shell.
    pub command: String,
    pub target: String,
    pub state_directory: Option<String>,
    /// QUIC must be directly reachable even when SSH uses a jump host.
    pub quic_host: Option<String>,
}
impl SshOptions {
    fn argv(&self) -> Result<Vec<String>> {
        ensure!(
            !self.target.is_empty()
                && !self.target.starts_with('-')
                && !self.target.chars().any(char::is_control),
            "invalid SSH target"
        );
        let args = shlex::split(&self.command)
            .ok_or_else(|| anyhow::anyhow!("invalid SSH command quoting"))?;
        ensure!(!args.is_empty(), "empty SSH command");
        Ok(args)
    }
    fn remote_command(&self) -> Result<String> {
        Ok(match &self.state_directory {
            Some(directory) => format!(
                "sheepdog-server --state-dir {} bootstrap",
                shlex::try_quote(directory)?
            ),
            None => "sheepdog-server bootstrap".to_owned(),
        })
    }
}
fn hostname(config: &[u8]) -> Result<String> {
    for line in std::str::from_utf8(config)?.lines() {
        if let Some(host) = line.strip_prefix("hostname ") {
            ensure!(
                !host.is_empty() && !host.chars().any(char::is_whitespace),
                "invalid SSH hostname"
            );
            return Ok(host.trim_matches(['[', ']']).to_owned());
        }
    }
    bail!("SSH configuration did not supply a hostname")
}
/// Call before entering raw TTY mode: OpenSSH may ask for host-key acceptance,
/// a key passphrase, or login authentication on the controlling terminal.
pub async fn bootstrap_ssh(options: &SshOptions) -> Result<(String, Bootstrap)> {
    let args = options.argv()?;
    let host = match &options.quic_host {
        Some(host) => host.clone(),
        None => {
            let mut command = Command::new(&args[0]);
            command
                .args(&args[1..])
                .args(["-G", "--", &options.target])
                .stdin(Stdio::null());
            hostname(&output(command, Duration::from_secs(10)).await?)?
        }
    };
    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .args(["-T", "--", &options.target, &options.remote_command()?])
        .stdin(Stdio::inherit());
    let bootstrap: Bootstrap =
        serde_json::from_slice(&output(command, Duration::from_secs(120)).await?)?;
    Ok((host, bootstrap))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uses_resolved_ssh_host_and_quotes_remote_paths() {
        assert_eq!(
            hostname(b"host agents\nhostname 192.0.2.10\nuser jt\n").unwrap(),
            "192.0.2.10"
        );
        assert_eq!(
            hostname(b"hostname [2001:db8::1]\n").unwrap(),
            "2001:db8::1"
        );
        let options = SshOptions {
            command: "ssh -i 'key with space'".into(),
            target: "jt@agents".into(),
            state_directory: Some("/home/jt/space's; $(touch bad)".into()),
            quic_host: None,
        };
        assert_eq!(options.argv().unwrap(), ["ssh", "-i", "key with space"]);
        assert_eq!(
            shlex::split(&options.remote_command().unwrap()).unwrap(),
            [
                "sheepdog-server",
                "--state-dir",
                "/home/jt/space's; $(touch bad)",
                "bootstrap"
            ]
        );
    }
    #[tokio::test]
    async fn bounds_subprocess_stdout_before_collecting_an_unbounded_reply() {
        let mut command = Command::new("printf");
        command.arg("x".repeat(65537));
        assert!(
            output(command, Duration::from_secs(5))
                .await
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
    }
    #[tokio::test]
    async fn stalled_helper_is_deadlined_and_killed_on_drop() {
        let mut command = Command::new("sleep");
        command.arg("30");
        assert!(output(command, Duration::from_millis(20)).await.is_err());
    }
    #[test]
    fn rejects_option_targets_and_bad_local_quoting() {
        let mut options = SshOptions {
            command: "ssh".into(),
            target: "-oProxyCommand=bad".into(),
            state_directory: None,
            quic_host: None,
        };
        assert!(options.argv().is_err());
        options.target = "user@host".into();
        options.command = "ssh '".into();
        assert!(options.argv().is_err());
        assert!(hostname(b"hostname evil invalid\n").is_err());
    }
}
