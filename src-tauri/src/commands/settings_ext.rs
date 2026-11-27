//! Settings, provider & API key management, grant revocation, model usage & quotas,
//! MCP server config, skills (秘籍), schedules (功课), and prompt templates.

use std::path::{Path, PathBuf};

use odg_core::schedule::{self, Schedule};
use odg_core::settings::Settings;
use serde::Serialize;
use tauri::State;

use crate::state::{App, SettingsView, anthropic_base_url};

/// A provider put in the list, and the settings as they now are.
#[derive(Serialize)]
pub struct AddedProvider {
    pub id: String,
    pub view: SettingsView,
}

/// The 秘籍 page: the skills that come with the app, on or off, and the user's own, in the user's skills directory.
#[derive(Serialize)]
pub struct SkillsView {
    pub skills: Vec<odg_core::skills::Listed>,
    pub folder: String,
}

fn skills_view(folder: &Path) -> SkillsView {
    SkillsView {
        skills: odg_core::skills::catalog(folder),
        folder: folder.display().to_string(),
    }
}

#[tauri::command]
pub async fn get_settings(app: State<'_, App>) -> Result<SettingsView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || core.settings_view())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn save_settings(app: State<'_, App>, settings: Settings) -> Result<SettingsView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        core.settings.save(settings).map_err(|error| error.to_string())?;
        core.reconcile_live_models();
        Ok(core.settings_view())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn save_mcp_config(app: State<'_, App>, config: String) -> Result<SettingsView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        core.mcp.save_config(&config)?;
        Ok(core.settings_view())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn revoke_grant(app: State<'_, App>, workspace: String, grant: String) -> Result<SettingsView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        core.grants
            .remove(&workspace, &grant)
            .map_err(|error| error.to_string())?;
        Ok(core.settings_view())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn save_api_key(app: State<'_, App>, provider: String, key: String) -> Result<SettingsView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        core.settings
            .save_api_key(&provider, &key)
            .map_err(|error| error.to_string())?;
        Ok(core.settings_view())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn delete_api_key(app: State<'_, App>, provider: String) -> Result<SettingsView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        core.settings
            .delete_api_key(&provider)
            .map_err(|error| error.to_string())?;
        core.reconcile_live_models();
        Ok(core.settings_view())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn add_provider(app: State<'_, App>, kind: String, name: String) -> Result<AddedProvider, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        let id = core
            .settings
            .add_provider(&kind, &name)
            .map_err(|error| error.to_string())?;
        Ok(AddedProvider {
            id,
            view: core.settings_view(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn remove_provider(app: State<'_, App>, id: String) -> Result<SettingsView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        core.settings.remove_provider(&id).map_err(|error| error.to_string())?;
        core.reconcile_live_models();
        Ok(core.settings_view())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_models(app: State<'_, App>, provider: String) -> Result<Vec<odg_core::providers::Listed>, String> {
    if odg_core::providers::kind_id(&provider) == "moonshot" {
        let _ = odg_core::kimi::ensure_fresh_access_token(&app.home, &provider).await;
    }
    let endpoint = app.settings.endpoint(&provider);
    let base_url = if provider == "anthropic" {
        anthropic_base_url()
    } else {
        endpoint.base_url
    };
    let models = odg_core::providers::list_models(&provider, &base_url, endpoint.key.as_deref()).await?;
    let store = app.settings.clone();
    let prov = provider.clone();
    let listed = models.clone();
    let _ = tokio::task::spawn_blocking(move || store.record_models(&prov, &listed)).await;
    Ok(models)
}

#[tauri::command]
pub async fn check_app_update() -> Result<odg_core::update::UpdateStatus, String> {
    odg_core::update::check_latest(env!("CARGO_PKG_VERSION")).await
}

#[tauri::command]
pub async fn install_app_update(handle: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = handle.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    handle.restart()
}

#[cfg(unix)]
pub(crate) fn link_cli_binary_into(
    source_cli: &std::path::Path,
    target_bin_dir: &std::path::Path,
) -> Result<PathBuf, String> {
    if !source_cli.is_file() {
        return Err(format!("`odg` binary not found at {}", source_cli.display()));
    }
    std::fs::create_dir_all(target_bin_dir).map_err(|e| format!("cannot create {}: {e}", target_bin_dir.display()))?;
    let link_path = target_bin_dir.join("odg");
    if let Ok(meta) = std::fs::symlink_metadata(&link_path) {
        if meta.file_type().is_symlink() {
            std::fs::remove_file(&link_path)
                .map_err(|e| format!("cannot replace symlink {}: {e}", link_path.display()))?;
        } else if link_path != source_cli {
            return Err(format!(
                "{} already exists as a regular file; remove or rename it first",
                link_path.display()
            ));
        } else {
            return Ok(link_path);
        }
    }
    std::os::unix::fs::symlink(source_cli, &link_path).map_err(|e| {
        format!(
            "cannot symlink {} -> {}: {e}",
            link_path.display(),
            source_cli.display()
        )
    })?;
    Ok(link_path)
}

#[tauri::command]
pub fn install_cli_link() -> Result<String, String> {
    #[cfg(unix)]
    {
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        let candidates: Vec<PathBuf> = [
            current.parent().map(|dir| dir.join("odg")),
            Some(PathBuf::from("/usr/bin/odg")),
            Some(PathBuf::from("/usr/local/bin/odg")),
        ]
        .into_iter()
        .flatten()
        .collect();
        let Some(src) = candidates.into_iter().find(|p| p.is_file()) else {
            return Err(
                "bundled `odg` binary not found beside the desktop app; download `odg-cli` from GitHub Releases"
                    .to_string(),
            );
        };
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .ok_or("cannot determine HOME directory")?;
        let link = link_cli_binary_into(&src, &home.join(".local/bin"))?;
        Ok(link.display().to_string())
    }
    #[cfg(not(unix))]
    {
        Err("on Windows, download the standalone `odg-cli` zip from GitHub Releases and add it to PATH".to_string())
    }
}

#[tauri::command]
pub fn get_model_usage(app: State<'_, App>) -> Vec<odg_core::model_usage::ModelUsageEntry> {
    app.model_usage.list()
}

#[tauri::command]
pub fn clear_model_cooldown(
    app: State<'_, App>,
    provider: String,
    model: String,
) -> Vec<odg_core::model_usage::ModelUsageEntry> {
    app.model_usage.clear_cooldown(&provider, &model);
    app.model_usage.list()
}

#[tauri::command]
pub fn reset_model_usage(app: State<'_, App>) -> Vec<odg_core::model_usage::ModelUsageEntry> {
    app.model_usage.reset_all();
    app.model_usage.list()
}

#[tauri::command]
pub fn get_usage_ledger_summary(
    app: State<'_, App>,
    days: Option<u32>,
) -> odg_core::model_usage::UsageLedgerSummary {
    app.model_usage.summarize_ledger(days)
}

#[tauri::command]
pub fn get_price_catalog() -> odg_core::pricing::PriceCatalogView {
    odg_core::pricing::PriceCatalogStore::global().view()
}

#[tauri::command]
pub async fn sync_price_catalog() -> Result<odg_core::pricing::PriceCatalogView, String> {
    odg_core::pricing::PriceCatalogStore::global().sync_models_dev().await
}

#[tauri::command]
pub fn set_custom_price(
    key: String,
    rate: odg_core::pricing::ModelRate,
) -> Result<odg_core::pricing::PriceCatalogView, String> {
    odg_core::pricing::PriceCatalogStore::global().set_custom(&key, rate)
}

#[tauri::command]
pub fn delete_custom_price(key: String) -> Result<odg_core::pricing::PriceCatalogView, String> {
    odg_core::pricing::PriceCatalogStore::global().delete_custom(&key)
}

#[tauri::command]
pub async fn get_antigravity_quota(force: Option<bool>) -> Result<odg_core::antigravity::AntigravityQuota, String> {
    odg_core::antigravity::fetch_quota(force.unwrap_or(false)).await
}

#[tauri::command]
pub async fn get_magpie_quota(
    app: State<'_, App>,
    provider: String,
    force: Option<bool>,
) -> Result<odg_core::antigravity::AntigravityQuota, String> {
    let endpoint = app.settings.endpoint(&provider);
    odg_core::magpie::fetch_quota(&endpoint.base_url, endpoint.key.as_deref(), force.unwrap_or(false)).await
}

#[tauri::command]
pub async fn get_kimi_quota(
    app: State<'_, App>,
    provider: String,
    force: Option<bool>,
) -> Result<odg_core::antigravity::AntigravityQuota, String> {
    let endpoint = app.settings.endpoint(&provider);
    odg_core::kimi::fetch_quota_for_provider(
        &app.home,
        &provider,
        &endpoint.base_url,
        endpoint.key.as_deref(),
        force.unwrap_or(false),
    )
    .await
}

#[tauri::command]
pub async fn kimi_oauth_start(
    app: State<'_, App>,
    region: String,
    provider: Option<String>,
    name: Option<String>,
) -> Result<odg_core::kimi::KimiOAuthStart, String> {
    let region = odg_core::kimi::KimiRegion::from_str_lossy(&region);
    odg_core::kimi::start_oauth_login(&app.home, region, provider, name).await
}

#[tauri::command]
pub async fn kimi_oauth_poll(app: State<'_, App>, flow_id: String) -> Result<odg_core::kimi::KimiOAuthPoll, String> {
    let res = odg_core::kimi::poll_oauth_login(&app.home, &flow_id, &app.settings).await?;
    if res.authenticated {
        app.reconcile_live_models();
    }
    Ok(res)
}

#[tauri::command]
pub async fn kimi_oauth_cancel(flow_id: Option<String>) {
    odg_core::kimi::cancel_oauth_login(flow_id.as_deref()).await;
}

#[tauri::command]
pub async fn kimi_oauth_logout(app: State<'_, App>, provider: String) -> Result<SettingsView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        odg_core::kimi::logout_oauth(&core.home, &provider)?;
        core.reconcile_live_models();
        Ok(core.settings_view())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_skills(app: State<'_, App>) -> Result<SkillsView, String> {
    let folder = app.skills_dir();
    tokio::task::spawn_blocking(move || skills_view(&folder))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn switch_skill(
    app: State<'_, App>,
    name: String,
    on: bool,
    force: Option<bool>,
) -> Result<SkillsView, String> {
    let folder = app.skills_dir();
    tokio::task::spawn_blocking(move || {
        odg_core::skills::switch(&folder, &name, on, force == Some(true))?;
        Ok(skills_view(&folder))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn install_skill(
    app: State<'_, App>,
    source: String,
    path: Option<String>,
    replace: Option<bool>,
) -> Result<SkillsView, String> {
    let folder = app.skills_dir();
    let source = source.trim().to_string();
    let within = path
        .map(|p| p.trim().trim_matches('/').to_string())
        .filter(|p| !p.is_empty());
    let local = PathBuf::from(&source);
    let cancel = tokio_util::sync::CancellationToken::new();
    odg_core::skills::fetch(
        &source,
        local,
        within.as_deref(),
        &folder,
        replace == Some(true),
        &cancel,
    )
    .await?;
    tokio::task::spawn_blocking(move || skills_view(&folder))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn write_skill(
    app: State<'_, App>,
    name: String,
    description: String,
    body: String,
    replace: Option<bool>,
) -> Result<SkillsView, String> {
    let folder = app.skills_dir();
    tokio::task::spawn_blocking(move || {
        odg_core::skills::write(&folder, name.trim(), &description, &body, replace == Some(true))?;
        Ok(skills_view(&folder))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn remove_skill(app: State<'_, App>, name: String) -> Result<SkillsView, String> {
    let folder = app.skills_dir();
    tokio::task::spawn_blocking(move || {
        odg_core::skills::remove(&folder, &name)?;
        Ok(skills_view(&folder))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_schedules(app: State<'_, App>) -> Result<Vec<Schedule>, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || core.schedules.all())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn save_schedule(app: State<'_, App>, schedule: Schedule) -> Result<Vec<Schedule>, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        core.schedules.save(schedule, &schedule::now())?;
        Ok(core.schedules.all())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn delete_schedule(app: State<'_, App>, id: String) -> Result<Vec<Schedule>, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        core.schedules.remove(&id)?;
        Ok(core.schedules.all())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn run_schedule(app: State<'_, App>, id: String) -> Result<Vec<Schedule>, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        let schedule = core
            .schedules
            .all()
            .into_iter()
            .find(|kept| kept.id == id)
            .ok_or("this schedule is no longer there")?;
        core.schedules.record(&id, core.fire(&schedule))?;
        Ok(core.schedules.all())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_prompt_templates(
    app: State<'_, App>,
    id: Option<String>,
) -> Result<Vec<odg_core::templates::PromptTemplateInfo>, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        let (user_dir, proj_dir) = core.prompt_roots_for_session(id.as_deref());
        odg_core::templates::listing(&user_dir, proj_dir.as_deref())
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn expand_prompt_template(
    app: State<'_, App>,
    id: Option<String>,
    name: String,
    args: String,
) -> Result<String, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        let (user_dir, proj_dir) = core.prompt_roots_for_session(id.as_deref());
        odg_core::templates::expand_named(&user_dir, proj_dir.as_deref(), &name, &args)
    })
    .await
    .map_err(|e| e.to_string())?
}
