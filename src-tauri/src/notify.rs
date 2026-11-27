//! Cross-platform zero-script-interpolation native desktop notifications (`P1-1`).
//!
//! On macOS, notifications are delivered in-process via `UNUserNotificationCenter` (falling back
//! in-process to `NSUserNotificationCenter` if `usernoted` rejects ad-hoc signatures) rather than
//! `osascript`, ensuring notifications display `OmniDoppelganger.app`'s own icon and clicking a
//! notification activates OmniDoppelganger and navigates to the target session instead of opening
//! Apple's Script Editor (`脚本编辑器`).

#[cfg(any(windows, target_os = "linux"))]
use std::path::PathBuf;

use serde::Serialize;

pub const NOTIFY_MAX_CHARS: usize = 256;
pub const NOTIFICATION_ACTIVATED_EVENT: &str = "notification-activated";

#[derive(Debug, Clone, Serialize)]
pub struct NotificationActivatedPayload {
    pub session_id: Option<String>,
    pub page: Option<String>,
}

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

#[cfg(any(target_os = "linux", test))]
pub fn build_linux_notify_command(title: &str, body: &str, icon: Option<&std::path::Path>) -> std::process::Command {
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
pub fn build_windows_notify_command(title: &str, body: &str, icon: Option<&std::path::Path>) -> std::process::Command {
    let title = sanitize_notify_text(title);
    let body = sanitize_notify_text(body);
    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-Command",
        WIN_TOAST_SCRIPT,
    ])
    .env("ODG_NOTIFY_TITLE", &title)
    .env("ODG_NOTIFY_BODY", &body);
    if let Some(icon_path) = icon {
        let icon_str = icon_path.display().to_string();
        let icon_uri = format!("file:///{}", icon_str.replace('\\', "/").trim_start_matches('/'));
        cmd.env("ODG_NOTIFY_ICON", &icon_str)
            .env("ODG_NOTIFY_ICON_URI", icon_uri);
    } else {
        cmd.env_remove("ODG_NOTIFY_ICON").env_remove("ODG_NOTIFY_ICON_URI");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
    use std::sync::{Once, OnceLock};
    use std::time::Duration;

    use block2::{Block, RcBlock};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, AnyProtocol, Bool, ClassBuilder, Sel};
    use objc2::{class, msg_send, sel};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{MainThreadMarker, NSBundle, NSError, NSString};
    use tauri::{AppHandle, Emitter};

    use super::{NOTIFICATION_ACTIVATED_EVENT, NotificationActivatedPayload};

    #[link(name = "UserNotifications", kind = "framework")]
    unsafe extern "C" {}

    const AUTH_PENDING: u8 = 0;
    const AUTH_GRANTED: u8 = 1;
    const AUTH_FALLBACK_LEGACY: u8 = 2;
    const AUTH_DENIED: u8 = 3;

    // UNAuthorizationOptionBadge (1 << 0) | Sound (1 << 1) | Alert (1 << 2)
    const UN_AUTH_OPTIONS: usize = (1 << 0) | (1 << 1) | (1 << 2);
    // UNNotificationPresentationOptionSound (1 << 1) | List (1 << 3) | Banner (1 << 4)
    const UN_PRESENT_OPTIONS: usize = (1 << 1) | (1 << 3) | (1 << 4);

    static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
    static DELEGATE_ONCE: Once = Once::new();
    static AUTH_STATE: AtomicU8 = AtomicU8::new(AUTH_PENDING);
    static REQ_SEQ: AtomicU64 = AtomicU64::new(1);

    /// `UNUserNotificationCenter` throws an Objective-C `NSInternalInconsistencyException` if called
    /// from an unbundled executable (e.g. `cargo tauri dev` or `cargo test`). Guard strictly on a
    /// real `.app` bundle with a non-empty `CFBundleIdentifier`.
    pub fn is_bundled_app() -> bool {
        let bundle = NSBundle::mainBundle();
        let has_id = bundle
            .bundleIdentifier()
            .is_some_and(|id| !id.to_string().trim().is_empty());
        let bundle_path = bundle.bundlePath().to_string();
        has_id && bundle_path.ends_with(".app")
    }

    unsafe fn dict_string(dict: *const AnyObject, key: &str) -> Option<String> {
        if dict.is_null() {
            return None;
        }
        let ns_key = NSString::from_str(key);
        let val: *mut AnyObject = unsafe { msg_send![dict, objectForKey: &*ns_key] };
        if val.is_null() {
            return None;
        }
        let is_str: Bool = unsafe { msg_send![val, isKindOfClass: class!(NSString)] };
        if !is_str.as_bool() {
            return None;
        }
        let ns_val = unsafe { &*(val.cast::<NSString>()) };
        let s = ns_val.to_string();
        (!s.is_empty()).then_some(s)
    }

    fn activate_and_route(session_id: Option<String>, page: Option<String>) {
        let Some(handle) = APP_HANDLE.get().cloned() else {
            return;
        };
        let emit_handle = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                #[allow(deprecated)]
                {
                    let ns_app = NSApplication::sharedApplication(mtm);
                    ns_app.activateIgnoringOtherApps(true);
                }
            }
            crate::tray::show_main_window(&emit_handle);
            let _ = emit_handle.emit(
                NOTIFICATION_ACTIVATED_EVENT,
                NotificationActivatedPayload { session_id, page },
            );
        });
    }

    unsafe extern "C-unwind" fn will_present(
        _this: *mut AnyObject,
        _cmd: Sel,
        _center: *mut AnyObject,
        _notification: *mut AnyObject,
        completion: *mut Block<dyn Fn(usize)>,
    ) {
        if let Some(block) = unsafe { completion.as_ref() } {
            block.call((UN_PRESENT_OPTIONS,));
        }
    }

    unsafe extern "C-unwind" fn did_receive_response(
        _this: *mut AnyObject,
        _cmd: Sel,
        _center: *mut AnyObject,
        response: *mut AnyObject,
        completion: *mut Block<dyn Fn()>,
    ) {
        let mut session_id = None;
        let mut page = None;
        if !response.is_null() {
            let notification: *mut AnyObject = unsafe { msg_send![response, notification] };
            if !notification.is_null() {
                let request: *mut AnyObject = unsafe { msg_send![notification, request] };
                if !request.is_null() {
                    let content: *mut AnyObject = unsafe { msg_send![request, content] };
                    if !content.is_null() {
                        let user_info: *const AnyObject = unsafe { msg_send![content, userInfo] };
                        session_id = unsafe { dict_string(user_info, "session_id") };
                        page = unsafe { dict_string(user_info, "page") };
                    }
                }
            }
        }
        activate_and_route(session_id, page);
        if let Some(block) = unsafe { completion.as_ref() } {
            block.call(());
        }
    }

    unsafe extern "C-unwind" fn should_present_legacy(
        _this: *mut AnyObject,
        _cmd: Sel,
        _center: *mut AnyObject,
        _notification: *mut AnyObject,
    ) -> Bool {
        Bool::YES
    }

    unsafe extern "C-unwind" fn did_activate_legacy(
        _this: *mut AnyObject,
        _cmd: Sel,
        center: *mut AnyObject,
        notification: *mut AnyObject,
    ) {
        let mut session_id = None;
        let mut page = None;
        if !notification.is_null() {
            let user_info: *const AnyObject = unsafe { msg_send![notification, userInfo] };
            session_id = unsafe { dict_string(user_info, "session_id") };
            page = unsafe { dict_string(user_info, "page") };
            if !center.is_null() {
                let _: () = unsafe { msg_send![center, removeDeliveredNotification: notification] };
            }
        }
        activate_and_route(session_id, page);
    }

    fn request_un_authorization(center: *mut AnyObject, wait: bool) {
        if center.is_null() {
            AUTH_STATE.store(AUTH_FALLBACK_LEGACY, Ordering::Release);
            return;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel::<u8>(1);
        let block = RcBlock::new(move |granted: Bool, err: *mut NSError| {
            let next = if !err.is_null() {
                AUTH_FALLBACK_LEGACY
            } else if granted.as_bool() {
                AUTH_GRANTED
            } else {
                AUTH_DENIED
            };
            AUTH_STATE.store(next, Ordering::Release);
            let _ = tx.send(next);
        });
        unsafe {
            let _: () = msg_send![center, requestAuthorizationWithOptions: UN_AUTH_OPTIONS, completionHandler: &*block];
        }
        if wait {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }

    pub fn init(app: &AppHandle) {
        let _ = APP_HANDLE.set(app.clone());
        if !is_bundled_app() {
            return;
        }
        DELEGATE_ONCE.call_once(|| {
            let Some(mut builder) = ClassBuilder::new(c"OdgNotificationDelegate", class!(NSObject)) else {
                return;
            };
            if let Some(proto) = AnyProtocol::get(c"UNUserNotificationCenterDelegate") {
                builder.add_protocol(proto);
            }
            if let Some(proto) = AnyProtocol::get(c"NSUserNotificationCenterDelegate") {
                builder.add_protocol(proto);
            }
            unsafe {
                builder.add_method(
                    sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
                    will_present
                        as unsafe extern "C-unwind" fn(
                            *mut AnyObject,
                            Sel,
                            *mut AnyObject,
                            *mut AnyObject,
                            *mut Block<dyn Fn(usize)>,
                        ),
                );
                builder.add_method(
                    sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
                    did_receive_response
                        as unsafe extern "C-unwind" fn(
                            *mut AnyObject,
                            Sel,
                            *mut AnyObject,
                            *mut AnyObject,
                            *mut Block<dyn Fn()>,
                        ),
                );
                builder.add_method(
                    sel!(userNotificationCenter:shouldPresentNotification:),
                    should_present_legacy
                        as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject) -> Bool,
                );
                builder.add_method(
                    sel!(userNotificationCenter:didActivateNotification:),
                    did_activate_legacy
                        as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
                );
            }
            let cls = builder.register();
            unsafe {
                let delegate: Retained<AnyObject> = msg_send![cls, new];
                let un_center: *mut AnyObject = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
                if !un_center.is_null() {
                    let _: () = msg_send![un_center, setDelegate: &*delegate];
                }
                let ns_center: *mut AnyObject =
                    msg_send![class!(NSUserNotificationCenter), defaultUserNotificationCenter];
                if !ns_center.is_null() {
                    let _: () = msg_send![ns_center, setDelegate: &*delegate];
                }
                // Both notification centers hold an unretained/weak reference to `delegate`; keep it alive forever.
                std::mem::forget(delegate);
                request_un_authorization(un_center, false);
            }
        });
    }

    unsafe fn build_user_info(session_id: Option<&str>, page: Option<&str>) -> Retained<AnyObject> {
        let dict: Retained<AnyObject> = unsafe { msg_send![class!(NSMutableDictionary), new] };
        if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
            let k = NSString::from_str("session_id");
            let v = NSString::from_str(sid);
            let _: () = unsafe { msg_send![&*dict, setObject: &*v, forKey: &*k] };
        }
        if let Some(pg) = page.filter(|s| !s.is_empty()) {
            let k = NSString::from_str("page");
            let v = NSString::from_str(pg);
            let _: () = unsafe { msg_send![&*dict, setObject: &*v, forKey: &*k] };
        }
        dict
    }

    unsafe fn deliver_legacy(title: &str, body: &str, user_info: &AnyObject) -> Result<(), String> {
        let center: *mut AnyObject =
            unsafe { msg_send![class!(NSUserNotificationCenter), defaultUserNotificationCenter] };
        if center.is_null() {
            return Err("NSUserNotificationCenter unavailable".to_string());
        }
        let notif: Retained<AnyObject> = unsafe { msg_send![class!(NSUserNotification), new] };
        let ns_title = NSString::from_str(title);
        let ns_body = NSString::from_str(body);
        let ns_sound = NSString::from_str("DefaultSoundName");
        unsafe {
            let _: () = msg_send![&*notif, setTitle: &*ns_title];
            let _: () = msg_send![&*notif, setInformativeText: &*ns_body];
            let _: () = msg_send![&*notif, setSoundName: &*ns_sound];
            let _: () = msg_send![&*notif, setUserInfo: user_info];
            let _: () = msg_send![center, deliverNotification: &*notif];
        }
        Ok(())
    }

    pub fn send(title: &str, body: &str, session_id: Option<&str>, page: Option<&str>) -> Result<(), String> {
        if !is_bundled_app() {
            return Err("unbundled macOS dev process; delegating to WebView Notification".to_string());
        }

        let user_info = unsafe { build_user_info(session_id, page) };
        let un_center: *mut AnyObject =
            unsafe { msg_send![class!(UNUserNotificationCenter), currentNotificationCenter] };

        if AUTH_STATE.load(Ordering::Acquire) == AUTH_PENDING {
            request_un_authorization(un_center, true);
        }

        match AUTH_STATE.load(Ordering::Acquire) {
            AUTH_DENIED => Ok(()),
            AUTH_FALLBACK_LEGACY => unsafe { deliver_legacy(title, body, &user_info) },
            _ => {
                if un_center.is_null() {
                    return unsafe { deliver_legacy(title, body, &user_info) };
                }
                let ns_title = NSString::from_str(title);
                let ns_body = NSString::from_str(body);
                let req_id = NSString::from_str(&format!("odg-{}", REQ_SEQ.fetch_add(1, Ordering::Relaxed)));
                let (tx, rx) = std::sync::mpsc::sync_channel::<bool>(1);
                let completion = RcBlock::new(move |err: *mut NSError| {
                    let ok = err.is_null();
                    if !ok {
                        AUTH_STATE.store(AUTH_FALLBACK_LEGACY, Ordering::Release);
                    }
                    let _ = tx.send(ok);
                });
                unsafe {
                    let content: Retained<AnyObject> = msg_send![class!(UNMutableNotificationContent), new];
                    let _: () = msg_send![&*content, setTitle: &*ns_title];
                    let _: () = msg_send![&*content, setBody: &*ns_body];
                    let sound: *mut AnyObject = msg_send![class!(UNNotificationSound), defaultSound];
                    if !sound.is_null() {
                        let _: () = msg_send![&*content, setSound: sound];
                    }
                    let _: () = msg_send![&*content, setUserInfo: &*user_info];
                    let req: Retained<AnyObject> = msg_send![
                        class!(UNNotificationRequest),
                        requestWithIdentifier: &*req_id,
                        content: &*content,
                        trigger: std::ptr::null::<AnyObject>()
                    ];
                    let _: () = msg_send![
                        un_center,
                        addNotificationRequest: &*req,
                        withCompletionHandler: &*completion
                    ];
                }
                if rx.recv_timeout(Duration::from_secs(2)) == Ok(false) {
                    return unsafe { deliver_legacy(title, body, &user_info) };
                }
                Ok(())
            }
        }
    }
}

pub fn init(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    macos::init(app);
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

pub async fn dispatch_notification(
    title: String,
    body: String,
    session_id: Option<String>,
    page: Option<String>,
) -> Result<(), String> {
    let title = sanitize_notify_text(&title);
    let body = sanitize_notify_text(&body);
    tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            macos::send(&title, &body, session_id.as_deref(), page.as_deref())
        }
        #[cfg(target_os = "linux")]
        {
            let _ = (&session_id, &page);
            let icon = notification_icon_path();
            build_linux_notify_command(&title, &body, icon.as_deref())
                .output()
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        #[cfg(windows)]
        {
            let _ = (&session_id, &page);
            let icon = notification_icon_path();
            build_windows_notify_command(&title, &body, icon.as_deref())
                .output()
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            let _ = (&title, &body, &session_id, &page);
            Ok(())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Fire a best-effort native desktop notification when the app is unfocused (turn completed or waiting approval).
#[tauri::command]
pub async fn notify_desktop(
    title: String,
    body: String,
    session_id: Option<String>,
    page: Option<String>,
) -> Result<(), String> {
    dispatch_notification(title, body, session_id, page).await
}

#[cfg(test)]
mod tests {
    use super::{
        NOTIFY_MAX_CHARS, WIN_TOAST_SCRIPT, build_linux_notify_command, build_windows_notify_command,
        sanitize_notify_text,
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
        let win_args: Vec<&str> = win_cmd.get_args().map(|a| a.to_str().expect("utf-8 arg")).collect();
        assert_eq!(
            win_args,
            vec![
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                WIN_TOAST_SCRIPT,
            ]
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

        // Linux: `notify-send` options terminated by `--` immediately before title and body.
        for icon in [None, Some(Path::new("/tmp/app-icon.png"))] {
            let linux_cmd = build_linux_notify_command(untrusted_title, untrusted_body, icon);
            assert_eq!(linux_cmd.get_program(), OsStr::new("notify-send"));
            let linux_args: Vec<&str> = linux_cmd.get_args().map(|a| a.to_str().expect("utf-8 arg")).collect();
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
        let dirty = format!("头\r\n行\n次\t列\0\x07\x1b\u{007f}\u{0085}{}", "敕\u{2019}".repeat(200));
        let cleaned = sanitize_notify_text(&dirty);
        assert_eq!(cleaned.chars().count(), NOTIFY_MAX_CHARS);
        assert!(cleaned.starts_with("头 行 次 列敕\u{2019}"));
        assert!(cleaned.chars().all(|c| !c.is_control()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unbundled_test_runner_gracefully_skips_un_notification_center_without_osascript() {
        assert!(!super::macos::is_bundled_app());
        let err = super::macos::send("测试标题", "测试内容", Some("sess-1"), None)
            .expect_err("unbundled test binary must not invoke UNUserNotificationCenter or osascript");
        assert!(err.contains("unbundled"));
    }
}
