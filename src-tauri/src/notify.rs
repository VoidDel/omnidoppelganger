//! Cross-platform zero-script-interpolation native desktop notifications (`P1-1`).

#[cfg(any(windows, target_os = "linux"))]
use std::path::PathBuf;

pub const NOTIFY_MAX_CHARS: usize = 256;

#[cfg(any(target_os = "macos", test))]
pub const MACOS_NOTIFY_SCRIPT: &str =
    "on run argv\ndisplay notification (item 1 of argv) with title (item 2 of argv)\nend run";

#[cfg(any(windows, test))]
pub const WIN_TOAST_SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$appId = 'com.omnidoppelganger.app'
if ($env:ODG_NOTIFY_ICON) {
  $reg = "HKCU:\Software\Classes\AppUserModelId\$appId"
  if (!(Test-Path -LiteralPath $reg)) { New-Item -Path $reg -Force > $null }
  Set-ItemProperty -LiteralPath $reg -Name 'DisplayName' -Value 'OmniDoppelganger'
  Set-ItemProperty -LiteralPath $reg -Name 'IconUri' -Value $env:ODG_NOTIFY_ICON
}
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] > $null
$xml = [Windows.Data.Xml.Dom.XmlDocument]::new()
$xml.LoadXml('<toast><visual><binding template="ToastGeneric"><text/><text/></binding></visual></toast>')
$texts = $xml.GetElementsByTagName('text')
[void]$texts.Item(0).AppendChild($xml.CreateTextNode([string]$env:ODG_NOTIFY_TITLE))
[void]$texts.Item(1).AppendChild($xml.CreateTextNode([string]$env:ODG_NOTIFY_BODY))
if ($env:ODG_NOTIFY_ICON_URI) {
  $img = $xml.CreateElement('image')
  $img.SetAttribute('placement', 'appLogoOverride')
  $img.SetAttribute('src', [string]$env:ODG_NOTIFY_ICON_URI)
  $binding = $xml.GetElementsByTagName('binding').Item(0)
  [void]$binding.InsertBefore($img, $texts.Item(0))
}
$toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($appId).Show($toast)
"#;

/// Materialize the embedded app icon once into `~/.odg/app-icon.png` so OS notifications can display it.
#[cfg(any(windows, target_os = "linux"))]
fn notification_icon_path() -> Option<PathBuf> {
    const ICON_PNG: &[u8] = include_bytes!("../icons/128x128.png");
    let dir = odg_core::settings::home();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("app-icon.png");
    let up_to_date = std::fs::metadata(&path)
        .ok()
        .is_some_and(|m| m.len() == ICON_PNG.len() as u64);
    if !up_to_date && std::fs::write(&path, ICON_PNG).is_err() {
        return None;
    }
    Some(path)
}

/// Strip control characters (normalizing CR/LF/TAB to spaces) and cap length at [`NOTIFY_MAX_CHARS`].
pub fn sanitize_notify_text(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    let mut count = 0usize;
    while let Some(ch) = chars.next() {
        let mapped = match ch {
            '\r' => {
                let _ = chars.next_if_eq(&'\n');
                Some(' ')
            }
            '\n' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        };
        if let Some(clean) = mapped {
            if count >= NOTIFY_MAX_CHARS {
                break;
            }
            out.push(clean);
            count += 1;
        }
    }
    out
}

#[cfg(any(target_os = "macos", test))]
pub fn build_macos_notify_command(title: &str, body: &str) -> std::process::Command {
    let title = sanitize_notify_text(title);
    let body = sanitize_notify_text(body);
    let mut cmd = std::process::Command::new("osascript");
    cmd.args(["-e", MACOS_NOTIFY_SCRIPT, "--", &body, &title]);
    cmd
}

#[cfg(any(target_os = "linux", test))]
pub fn build_linux_notify_command(
    title: &str,
    body: &str,
    icon: Option<&std::path::Path>,
) -> std::process::Command {
    let title = sanitize_notify_text(title);
    let body = sanitize_notify_text(body);
    let mut cmd = std::process::Command::new("notify-send");
    cmd.args(["-a", "OmniDoppelganger"]);
    if let Some(icon_path) = icon {
        cmd.arg("-i").arg(icon_path);
    }
    cmd.args(["--", &title, &body]);
    cmd
}

#[cfg(any(windows, test))]
pub fn build_windows_notify_command(
    title: &str,
    body: &str,
    icon: Option<&std::path::Path>,
) -> std::process::Command {
    let title = sanitize_notify_text(title);
    let body = sanitize_notify_text(body);
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", WIN_TOAST_SCRIPT])
        .env("ODG_NOTIFY_TITLE", &title)
        .env("ODG_NOTIFY_BODY", &body);
    if let Some(icon_path) = icon {
        let icon_str = icon_path.display().to_string();
        let icon_uri = format!("file:///{}", icon_str.replace('\\', "/").trim_start_matches('/'));
        cmd.env("ODG_NOTIFY_ICON", &icon_str)
            .env("ODG_NOTIFY_ICON_URI", icon_uri);
    } else {
        cmd.env_remove("ODG_NOTIFY_ICON")
            .env_remove("ODG_NOTIFY_ICON_URI");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Fire a best-effort native desktop notification when the app is unfocused (turn completed or waiting approval).
#[tauri::command]
pub async fn notify_desktop(title: String, body: String) {
    let title = sanitize_notify_text(&title);
    let body = sanitize_notify_text(&body);
    let _ = tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            let _ = build_macos_notify_command(&title, &body).output();
        }
        #[cfg(target_os = "linux")]
        {
            let icon = notification_icon_path();
            let _ = build_linux_notify_command(&title, &body, icon.as_deref()).output();
        }
        #[cfg(windows)]
        {
            let icon = notification_icon_path();
            let _ = build_windows_notify_command(&title, &body, icon.as_deref()).output();
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            let _ = (&title, &body);
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::{
        MACOS_NOTIFY_SCRIPT, NOTIFY_MAX_CHARS, WIN_TOAST_SCRIPT, build_linux_notify_command,
        build_macos_notify_command, build_windows_notify_command, sanitize_notify_text,
    };
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::path::Path;

    #[test]
    fn notify_command_never_interpolates_untrusted_text_into_script() {
        let untrusted_title =
            "-u critical x\u{2018}\u{2019}\u{201a}\u{201b}'); Start-Process calc; \"$(whoami)\" `id` </text><script>";
        let untrusted_body =
            "--expire-time=0 \u{2019}\u{2018}\u{201a}\u{201b}'\"$(touch /tmp/pwn)`uname`</binding></toast>";
        assert!(untrusted_title.chars().count() <= NOTIFY_MAX_CHARS);
        assert!(untrusted_body.chars().count() <= NOTIFY_MAX_CHARS);

        // Windows: static PowerShell script + isolated environment variables + DOM APIs.
        let win_icon = Path::new("C:\\Users\\o'connor\\.odg\\app-icon.png");
        let win_cmd = build_windows_notify_command(untrusted_title, untrusted_body, Some(win_icon));
        assert_eq!(win_cmd.get_program(), OsStr::new("powershell"));
        let win_args: Vec<&str> = win_cmd
            .get_args()
            .map(|a| a.to_str().expect("utf-8 arg"))
            .collect();
        assert_eq!(
            win_args,
            vec!["-NoProfile", "-NonInteractive", "-Command", WIN_TOAST_SCRIPT]
        );
        assert!(WIN_TOAST_SCRIPT.contains("CreateTextNode"));
        assert!(WIN_TOAST_SCRIPT.contains("SetAttribute"));
        for arg in &win_args {
            assert!(!arg.contains("calc"), "unexpected payload in arg: {arg}");
            assert!(!arg.contains("whoami"), "unexpected payload in arg: {arg}");
            assert!(!arg.contains("</text><script>"), "unexpected xml in arg: {arg}");
            assert!(!arg.contains("-u critical"), "unexpected title in arg: {arg}");
            assert!(!arg.contains("o'connor"), "unexpected icon path in arg: {arg}");
        }
        let win_envs: HashMap<&OsStr, Option<&OsStr>> = win_cmd.get_envs().collect();
        assert_eq!(
            win_envs.get(OsStr::new("ODG_NOTIFY_TITLE")).copied().flatten(),
            Some(OsStr::new(untrusted_title))
        );
        assert_eq!(
            win_envs.get(OsStr::new("ODG_NOTIFY_BODY")).copied().flatten(),
            Some(OsStr::new(untrusted_body))
        );
        assert_eq!(
            win_envs.get(OsStr::new("ODG_NOTIFY_ICON")).copied().flatten(),
            Some(OsStr::new("C:\\Users\\o'connor\\.odg\\app-icon.png"))
        );
        assert_eq!(
            win_envs.get(OsStr::new("ODG_NOTIFY_ICON_URI")).copied().flatten(),
            Some(OsStr::new("file:///C:/Users/o'connor/.odg/app-icon.png"))
        );

        let win_no_icon = build_windows_notify_command(untrusted_title, untrusted_body, None);
        let win_no_icon_envs: HashMap<&OsStr, Option<&OsStr>> = win_no_icon.get_envs().collect();
        assert_eq!(win_no_icon_envs.get(OsStr::new("ODG_NOTIFY_ICON")), Some(&None));
        assert_eq!(win_no_icon_envs.get(OsStr::new("ODG_NOTIFY_ICON_URI")), Some(&None));

        // macOS: static `on run argv` AppleScript + `--` positional arguments.
        let mac_cmd = build_macos_notify_command(untrusted_title, untrusted_body);
        assert_eq!(mac_cmd.get_program(), OsStr::new("osascript"));
        let mac_args: Vec<&str> = mac_cmd
            .get_args()
            .map(|a| a.to_str().expect("utf-8 arg"))
            .collect();
        assert_eq!(
            mac_args,
            vec!["-e", MACOS_NOTIFY_SCRIPT, "--", untrusted_body, untrusted_title]
        );
        assert!(!mac_args[1].contains("calc"));
        assert!(!mac_args[1].contains("pwn"));

        // Linux: `notify-send` options terminated by `--` immediately before title and body.
        for icon in [None, Some(Path::new("/tmp/app-icon.png"))] {
            let linux_cmd = build_linux_notify_command(untrusted_title, untrusted_body, icon);
            assert_eq!(linux_cmd.get_program(), OsStr::new("notify-send"));
            let linux_args: Vec<&str> = linux_cmd
                .get_args()
                .map(|a| a.to_str().expect("utf-8 arg"))
                .collect();
            let title_pos = linux_args
                .iter()
                .position(|a| *a == untrusted_title)
                .expect("title positional arg");
            assert!(title_pos >= 1);
            assert_eq!(linux_args[title_pos - 1], "--");
            assert_eq!(linux_args[title_pos + 1], untrusted_body);
            assert_eq!(linux_args.len(), title_pos + 2);
        }

        // Control-character scrubbing and <= 256-char multibyte-safe truncation.
        let dirty = format!(
            "头\r\n行\n次\t列\0\x07\x1b\u{007f}\u{0085}{}",
            "敕\u{2019}".repeat(200)
        );
        let cleaned = sanitize_notify_text(&dirty);
        assert_eq!(cleaned.chars().count(), NOTIFY_MAX_CHARS);
        assert!(cleaned.starts_with("头 行 次 列敕\u{2019}"));
        assert!(cleaned.chars().all(|c| !c.is_control()));
    }
}
