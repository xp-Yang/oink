//! Opt-in monitor input selection for per-device hotkeys.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use oink_foundation::DeviceId;
use oink_routing::FocusLockError;
use serde::Deserialize;
use tokio::{
    process::Command,
    sync::{Mutex, oneshot},
    time::{Instant, sleep, timeout, timeout_at},
};

use crate::{
    node::Node,
    threads::input::{InputCommand, InputThread},
};

const CONFIG_NAME: &str = "ddc.json";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const FOCUS_TIMEOUT: Duration = Duration::from_secs(4);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Backend {
    M1ddc,
    M1ddcAlt,
    ControlMyMonitor,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    backend: Backend,
    program: PathBuf,
    display: String,
    inputs: BTreeMap<String, u16>,
}

impl Config {
    fn validate(&self) -> Result<()> {
        ensure!(self.program.is_absolute(), "DDC program must be an absolute path");
        ensure!(!self.display.trim().is_empty(), "DDC display selector is empty");
        ensure!(
            !self.inputs.is_empty()
                && self
                    .inputs
                    .iter()
                    .all(|(target, value)| !target.trim().is_empty() && *value > 0),
            "DDC inputs must map device names or IDs to positive input codes"
        );
        match self.backend {
            Backend::M1ddc | Backend::M1ddcAlt => ensure!(cfg!(target_os = "macos"), "m1ddc requires macOS"),
            Backend::ControlMyMonitor => {
                ensure!(cfg!(windows), "ControlMyMonitor requires Windows");
                ensure!(
                    self.program
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
                    "ControlMyMonitor program must be an .exe"
                );
            }
        }
        Ok(())
    }

    fn args(&self, source: u16) -> Vec<String> {
        let source = source.to_string();
        match self.backend {
            Backend::M1ddc | Backend::M1ddcAlt => {
                let feature = if matches!(self.backend, Backend::M1ddcAlt) {
                    "input-alt"
                } else {
                    "input"
                };
                vec![
                    "display".into(),
                    self.display.clone(),
                    "set".into(),
                    feature.into(),
                    source,
                ]
            }
            Backend::ControlMyMonitor => vec!["/SetValue".into(), self.display.clone(), "60".into(), source],
        }
    }

    async fn select(&self, source: u16) -> Result<()> {
        let mut command = Command::new(&self.program);
        command
            .args(self.args(source))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = command.spawn().context("start the DDC tool")?;
        let status = timeout(COMMAND_TIMEOUT, child.wait())
            .await
            .context("DDC command timed out")?
            .context("wait for the DDC tool")?;
        ensure!(status.success(), "DDC tool returned {status}");
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct Ddc {
    config: Arc<Config>,
    switching: Arc<Mutex<()>>,
}

impl Ddc {
    pub(crate) fn load(data_dir: &Path) -> Option<Self> {
        let path = data_dir.join(CONFIG_NAME);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(%error, "cannot read ddc.json");
                return None;
            }
        };
        match serde_json::from_slice::<Config>(&bytes)
            .context("read DDC configuration")
            .and_then(|config| {
                config.validate()?;
                Ok(config)
            }) {
            Ok(config) => Some(Self {
                config: Arc::new(config),
                switching: Arc::default(),
            }),
            Err(error) => {
                tracing::warn!(%error, "invalid ddc.json; monitor switching is off");
                None
            }
        }
    }

    /// True means this mapping owns the hotkey, including when a switch is rejected.
    pub(crate) fn request(&self, node: &Node, input: &Arc<InputThread>, target: DeviceId) -> bool {
        let code = self.config.inputs.get(&target.to_hex()).copied().or_else(|| {
            let name = node.device_name(target);
            let code = self.config.inputs.get(&name)?;
            let ambiguous = node
                .group_members()
                .into_iter()
                .chain([node.id()])
                .any(|id| id != target && node.device_name(id) == name);
            if ambiguous {
                tracing::warn!(%name, "DDC mapping needs device IDs for duplicate device names");
                return None;
            }
            Some(*code)
        });
        let Some(source) = code else { return false };
        let Ok(guard) = self.switching.clone().try_lock_owned() else {
            tracing::debug!("DDC switch already in progress");
            return true;
        };
        let status = input.status();
        if status.paused
            || status.local_paused
            || node.fully_paused()
            || (target != node.id()
                && !status
                    .peers
                    .iter()
                    .any(|peer| peer.peer == target && peer.attached && peer.online))
        {
            tracing::warn!(%target, "DDC switch skipped because input sharing is paused or the target is offline");
            return true;
        }
        let config = self.config.clone();
        let mut stopping = node.stop_receiver();
        let input = input.clone();
        node.inner.rt.spawn(async move {
            let _guard = guard;
            tokio::select! {
                biased;
                _ = stopping.wait_for(|stopped| *stopped) => {}
                result = switch(&config, &input, target, source) => {
                    if let Err(error) = result {
                        tracing::warn!(%target, source, error = %format_args!("{error:#}"), "shared display switch failed");
                    }
                }
            }
        });
        true
    }
}

async fn switch(config: &Config, input: &InputThread, target: DeviceId, source: u16) -> Result<()> {
    config.select(source).await?;
    let deadline = Instant::now() + FOCUS_TIMEOUT;
    // An inactive video input may reappear in the OS after the DDC write. Let
    // Oink's usual display and input discovery make it eligible for handoff.
    loop {
        let (reply, result) = oneshot::channel();
        input.send(InputCommand::MovePointerAfterDisplay { target, reply });
        match timeout_at(deadline, result)
            .await
            .context("input router did not answer the handoff")?
            .context("input router stopped before handoff")?
        {
            Ok(()) => break,
            Err(FocusLockError::Paused) => bail!("display command completed, but input sharing was paused"),
            Err(FocusLockError::Unavailable) => {
                if timeout_at(deadline, sleep(POLL_INTERVAL)).await.is_err() {
                    bail!("display command completed, but the destination is still unavailable for input");
                }
            }
        }
    }
    while input.status().focus != Some(target) {
        if timeout_at(deadline, sleep(POLL_INTERVAL)).await.is_err() {
            bail!("display command completed, but Oink did not confirm the keyboard and mouse handoff");
        }
    }
    tracing::info!(%target, source, "DDC command completed and Oink confirmed keyboard and mouse handoff");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(backend: Backend) -> Config {
        Config {
            backend,
            program: PathBuf::new(),
            display: "Monitor with spaces".into(),
            inputs: BTreeMap::new(),
        }
    }

    #[test]
    fn uses_backend_specific_input_select_without_splitting_the_monitor_selector() {
        assert_eq!(
            config(Backend::M1ddc).args(17),
            ["display", "Monitor with spaces", "set", "input", "17"]
        );
        assert_eq!(
            config(Backend::M1ddcAlt).args(208),
            ["display", "Monitor with spaces", "set", "input-alt", "208"]
        );
        assert_eq!(
            config(Backend::ControlMyMonitor).args(15),
            ["/SetValue", "Monitor with spaces", "60", "15"]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn waits_for_the_tool_and_reports_a_failed_write_or_timeout() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let mut config = config(Backend::M1ddc);
        config.program = directory.path().join("fake m1ddc");
        std::fs::write(
            &config.program,
            "#!/bin/sh\ncase \"$5\" in\n17) exit 0;;\n15) exit 1;;\n*) exec sleep 10;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&config.program, std::fs::Permissions::from_mode(0o700)).unwrap();

        config.select(17).await.unwrap();
        assert!(config.select(15).await.unwrap_err().to_string().contains("returned"));
        assert!(config.select(12).await.unwrap_err().to_string().contains("timed out"));
    }
}
