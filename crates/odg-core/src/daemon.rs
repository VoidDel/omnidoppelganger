//! Cross-platform user-level background daemon management and desktop/daemon coordination.
//!
//! Keeps scheduled tasks (`功课`) running even after the desktop window is closed or after login:
//! - macOS: `~/Library/LaunchAgents/com.omnidoppelganger.daemon.plist`
//! - Linux: `~/.config/systemd/user/omnidoppelganger-daemon.service`
//! - Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (`OmniDoppelgangerDaemon`)

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

pub const MACOS_LABEL: &str = "com.omnidoppelganger.daemon";
pub const LINUX_SERVICE: &str = "omnidoppelganger-daemon.service";
pub const WINDOWS_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
pub const WINDOWS_RUN_VALUE: &str = "OmniDoppelgangerDaemon";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub installed: bool,
    pub running: bool,
    pub platform: String,
    pub service_path: String,
    pub executable: String,
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Render a macOS LaunchAgent plist that launches `exe` with `args` at login and restarts on crash,
/// while staying stopped after a clean `exit(0)` (e.g. user clicked "完全退出" in the tray).
pub fn macos_launch_agent_plist(exe: &Path, args: &[&str], log_dir: &Path) -> String {
    let mut program_args = String::new();
    program_args.push_str(&format!(
        "        <string>{}</string>\n",
        xml_escape(&exe.display().to_string())
    ));
    for arg in args {
        program_args.push_str(&format!("        <string>{}</string>\n", xml_escape(arg)));
    }
    let stdout = xml_escape(&log_dir.join("daemon.out.log").display().to_string());
    let stderr = xml_escape(&log_dir.join("daemon.err.log").display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{MACOS_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{program_args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#
    )
}

/// Render a Linux `systemd --user` service unit.
pub fn linux_systemd_unit(exe: &Path, args: &[&str]) -> String {
    let mut parts = vec![quote_systemd_arg(&exe.display().to_string())];
    parts.extend(args.iter().map(|a| quote_systemd_arg(a)));
    let exec_start = parts.join(" ");
    format!(
        "[Unit]\nDescription=OmniDoppelganger Background Schedule Daemon\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={exec_start}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n"
    )
}

fn quote_systemd_arg(arg: &str) -> String {
    if arg.chars().any(|c| c.is_whitespace() || c == '"' || c == '\\') {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}

/// Render the Windows HKCU Run registry command value.
pub fn windows_run_command(exe: &Path, args: &[&str]) -> String {
    let mut out = format!("\"{}\"", exe.display());
    for arg in args {
        out.push(' ');
        if arg.chars().any(char::is_whitespace) {
            out.push('"');
            out.push_str(arg);
            out.push('"');
        } else {
            out.push_str(arg);
        }
    }
    out
}

/// Path where the desktop app records its PID while running so a standalone CLI daemon yields to it.
pub fn desktop_pid_path(home: &Path) -> PathBuf {
    home.join("desktop.pid")
}

/// Record the current desktop process PID.
pub fn write_desktop_pid(home: &Path) {
    let _ = std::fs::create_dir_all(home);
    let _ = std::fs::write(desktop_pid_path(home), format!("{}\n", std::process::id()));
}

/// Remove the desktop PID file if it still belongs to the current process.
pub fn clear_desktop_pid(home: &Path) {
    let path = desktop_pid_path(home);
    if let Ok(raw) = std::fs::read_to_string(&path)
        && raw.trim().parse::<u32>().ok() == Some(std::process::id())
    {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether a desktop instance is currently running (checked by the headless CLI daemon before firing schedules).
pub fn is_desktop_running(home: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(desktop_pid_path(home)) else {
        return false;
    };
    let Ok(pid) = raw.trim().parse::<u32>() else {
        return false;
    };
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    is_pid_alive(pid)
}

#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(windows)]
fn is_pid_alive(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", &format!("PID eq {pid}"), "/NH"]);
    cmd.creation_flags(0x0800_0000);
    cmd.output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn is_pid_alive(_pid: u32) -> bool {
    false
}

fn user_home() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

pub fn service_location() -> String {
    #[cfg(target_os = "macos")]
    {
        user_home()
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{MACOS_LABEL}.plist"))
            .display()
            .to_string()
    }
    #[cfg(target_os = "linux")]
    {
        user_home()
            .join(".config")
            .join("systemd")
            .join("user")
            .join(LINUX_SERVICE)
            .display()
            .to_string()
    }
    #[cfg(windows)]
    {
        format!(r"{WINDOWS_RUN_KEY}\{WINDOWS_RUN_VALUE}")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        String::new()
    }
}

pub fn status(exe: &Path) -> DaemonStatus {
    let platform = std::env::consts::OS.to_string();
    let service_path = service_location();
    let executable = exe.display().to_string();

    #[cfg(target_os = "macos")]
    {
        let plist = PathBuf::from(&service_path);
        let installed = plist.exists();
        let running = if installed {
            Command::new("launchctl")
                .args(["list", MACOS_LABEL])
                .output()
                .is_ok_and(|o| o.status.success())
        } else {
            false
        };
        DaemonStatus {
            installed,
            running,
            platform,
            service_path,
            executable,
        }
    }

    #[cfg(target_os = "linux")]
    {
        let unit = PathBuf::from(&service_path);
        let installed = unit.exists();
        let running = if installed {
            Command::new("systemctl")
                .args(["--user", "is-active", "--quiet", LINUX_SERVICE])
                .status()
                .is_ok_and(|s| s.success())
        } else {
            false
        };
        DaemonStatus {
            installed,
            running,
            platform,
            service_path,
            executable,
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("reg");
        cmd.args(["query", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE]);
        cmd.creation_flags(0x0800_0000);
        let installed = cmd.output().is_ok_and(|o| o.status.success());
        DaemonStatus {
            installed,
            running: installed,
            platform,
            service_path,
            executable,
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        DaemonStatus {
            installed: false,
            running: false,
            platform,
            service_path,
            executable,
        }
    }
}

pub fn install(exe: &Path, args: &[&str], home: &Path) -> Result<DaemonStatus, String> {
    let log_dir = home.join("logs");
    std::fs::create_dir_all(&log_dir).map_err(|e| format!("创建日志目录失败：{e}"))?;

    #[cfg(target_os = "macos")]
    {
        let plist_path = PathBuf::from(service_location());
        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建 LaunchAgents 目录失败：{e}"))?;
        }
        let plist = macos_launch_agent_plist(exe, args, &log_dir);
        std::fs::write(&plist_path, plist).map_err(|e| format!("写入 LaunchAgent 配置失败：{e}"))?;
        let path_str = plist_path.display().to_string();
        let _ = Command::new("launchctl").args(["unload", "-w", &path_str]).output();
        let out = Command::new("launchctl")
            .args(["load", "-w", &path_str])
            .output()
            .map_err(|e| format!("调用 launchctl 失败：{e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!("launchctl load 失败：{err}"));
        }
        return Ok(status(exe));
    }

    #[cfg(target_os = "linux")]
    {
        let unit_path = PathBuf::from(service_location());
        if let Some(parent) = unit_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建 systemd 用户目录失败：{e}"))?;
        }
        let unit = linux_systemd_unit(exe, args);
        std::fs::write(&unit_path, unit).map_err(|e| format!("写入 systemd 服务文件失败：{e}"))?;
        let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).output();
        let out = Command::new("systemctl")
            .args(["--user", "enable", "--now", LINUX_SERVICE])
            .output()
            .map_err(|e| format!("调用 systemctl 失败：{e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!("systemctl enable 失败：{err}"));
        }
        return Ok(status(exe));
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let value = windows_run_command(exe, args);
        let mut cmd = Command::new("reg");
        cmd.args([
            "add",
            WINDOWS_RUN_KEY,
            "/v",
            WINDOWS_RUN_VALUE,
            "/t",
            "REG_SZ",
            "/d",
            &value,
            "/f",
        ]);
        cmd.creation_flags(0x0800_0000);
        let out = cmd.output().map_err(|e| format!("写入注册表开机启动项失败：{e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!("写入 Windows 启动项失败：{err}"));
        }
        return Ok(status(exe));
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (exe, args, home);
        Err("当前系统暂不支持自动安装后台守护进程。".to_string())
    }
}

pub fn uninstall(exe: &Path) -> Result<DaemonStatus, String> {
    #[cfg(target_os = "macos")]
    {
        let plist_path = PathBuf::from(service_location());
        if plist_path.exists() {
            let path_str = plist_path.display().to_string();
            let _ = Command::new("launchctl").args(["unload", "-w", &path_str]).output();
            std::fs::remove_file(&plist_path).map_err(|e| format!("删除 LaunchAgent 配置失败：{e}"))?;
        }
        return Ok(status(exe));
    }

    #[cfg(target_os = "linux")]
    {
        let unit_path = PathBuf::from(service_location());
        if unit_path.exists() {
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "--now", LINUX_SERVICE])
                .output();
            std::fs::remove_file(&unit_path).map_err(|e| format!("删除 systemd 服务文件失败：{e}"))?;
            let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).output();
        }
        return Ok(status(exe));
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("reg");
        cmd.args(["delete", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE, "/f"]);
        cmd.creation_flags(0x0800_0000);
        let _ = cmd.output();
        return Ok(status(exe));
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = exe;
        Err("当前系统暂不支持卸载后台守护进程。".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_cross_platform_daemon_configs_safely() {
        let plist = macos_launch_agent_plist(
            Path::new("/Applications/OmniDoppelganger.app/Contents/MacOS/OmniDoppelganger"),
            &["--background"],
            Path::new("/Users/me/.odg/logs"),
        );
        assert!(plist.contains(MACOS_LABEL));
        assert!(plist.contains("<string>--background</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        assert!(plist.contains("<false/>"));

        let unit = linux_systemd_unit(Path::new("/usr/local/bin/my app"), &["daemon", "run"]);
        assert!(unit.contains("ExecStart=\"/usr/local/bin/my app\" daemon run"));
        assert!(unit.contains("Restart=on-failure"));

        let win = windows_run_command(
            Path::new(r"C:\Program Files\OmniDoppelganger\omnidoppelganger.exe"),
            &["--background"],
        );
        assert_eq!(
            win,
            r#""C:\Program Files\OmniDoppelganger\omnidoppelganger.exe" --background"#
        );
    }

    #[test]
    fn desktop_pid_write_and_clear_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        write_desktop_pid(dir.path());
        assert!(desktop_pid_path(dir.path()).exists());
        // Current process is never treated as "another" desktop instance.
        assert!(!is_desktop_running(dir.path()));
        clear_desktop_pid(dir.path());
        assert!(!desktop_pid_path(dir.path()).exists());
    }
}
