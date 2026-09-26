//! The login items `amux init` installs: each runs `amux supervise` at
//! login and restarts it on failure only, never after a clean exit, so a
//! deliberate stop stays stopped and a crash comes back.
//!
//! The service manager owns the supervisor and the supervisor owns the
//! daemon; neither may take the agents down. systemd's default stop kills
//! every process in the unit's cgroup and launchd's kills the job's process
//! group, agents and providers included, so the systemd unit says
//! `KillMode=process` and the LaunchAgent `AbandonProcessGroup`.
//!
//! Login items start with the service manager's sparse environment, so each
//! carries the PATH `amux init` ran with: agents find their providers by it.

use std::fmt::Write as _;
use std::path::Path;

/// The LaunchAgent's label and file stem.
pub const LAUNCH_AGENT_LABEL: &str = "sh.amux.supervise";
/// The systemd user unit's file name.
pub const SYSTEMD_UNIT: &str = "amux.service";
/// The Windows scheduled task's name.
pub const WINDOWS_TASK: &str = "amux";

/// What every login item needs to know.
pub struct LoginItem<'a> {
    /// The canonical install path.
    pub binary: &'a Path,
    /// The installation config, when not the default one.
    pub config: Option<&'a Path>,
    /// The PATH agents will see.
    pub path: &'a str,
    /// Where the service manager writes the supervisor's own output.
    pub log: &'a Path,
}

impl LoginItem<'_> {
    fn arguments(&self) -> Vec<String> {
        let mut arguments = vec![self.binary.display().to_string()];
        if let Some(config) = self.config {
            arguments.push("--config".into());
            arguments.push(config.display().to_string());
        }
        arguments.push("supervise".into());
        arguments
    }

    /// `~/Library/LaunchAgents/sh.amux.supervise.plist`: kept alive only
    /// after an unsuccessful exit.
    pub fn launch_agent(&self) -> String {
        let mut arguments = String::new();
        for argument in self.arguments() {
            let _ = writeln!(arguments, "\t\t<string>{}</string>", xml(&argument));
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCH_AGENT_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
{arguments}	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>{path}</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>AbandonProcessGroup</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
            path = xml(self.path),
            log = xml(&self.log.display().to_string()),
        )
    }

    /// `~/.config/systemd/user/amux.service`: restarted on failure only,
    /// and stopping it stops the supervisor alone.
    pub fn systemd_unit(&self) -> String {
        let exec = self
            .arguments()
            .iter()
            .map(|argument| systemd_quote(argument))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "[Unit]
Description=amux supervisor: runs the amux daemon and restarts it

[Service]
ExecStart={exec}
Environment={path}
Restart=on-failure
KillMode=process
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
",
            path = systemd_quote(&format!("PATH={}", self.path)),
            log = self.log.display(),
        )
    }

    /// A Task Scheduler definition: run at logon, restart on failure.
    pub fn windows_task(&self) -> String {
        let arguments = self.arguments();
        let command = xml(&arguments[0]);
        let rest = arguments[1..]
            .iter()
            .map(|argument| windows_quote(argument))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Runs amux supervise at logon and restarts it if it fails.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{rest}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
            rest = xml(&rest),
        )
    }
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Quotes one systemd command-line word when it needs it.
fn systemd_quote(word: &str) -> String {
    if !word.is_empty()
        && !word
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '$' | '%' | ';'))
    {
        return word.to_owned();
    }
    let escaped = word
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

/// Quotes one Windows command-line argument when it needs it.
fn windows_quote(word: &str) -> String {
    if !word.is_empty() && !word.chars().any(|c| c.is_whitespace() || c == '"') {
        return word.to_owned();
    }
    format!("\"{}\"", word.replace('"', "\\\""))
}
