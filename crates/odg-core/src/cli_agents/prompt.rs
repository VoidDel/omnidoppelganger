//! Prompt assembly, cross-engine handoff briefings, skill injection, image materialization,
//! and oversized/batch-script prompt delivery via `stdin` or temporary files.

use std::path::{Path, PathBuf};

use crate::events::{Attached, Event, Image};

use super::CliKind;

/// Safe command-line prompt byte budget before spilling to `stdin` (Codex / Claude Code / Cursor)
/// or a temporary UTF-8 file (Antigravity / Grok / OpenCode).
/// Windows `.cmd` wrappers cap command lines at ~8,191 chars (`CreateProcessW` at 32,767 chars,
/// returning `os error 206`) and Rust's `std::process::Command` rejects any `.cmd`/`.bat` argument
/// containing `\r` or `\n` (`batch file arguments are invalid`, CVE-2024-24576); Linux caps each
/// argv entry (`MAX_ARG_STRLEN`) at 128 KB.
#[cfg(windows)]
pub(super) const MAX_CLI_PROMPT_BYTES: usize = 6_000;
#[cfg(not(windows))]
pub(super) const MAX_CLI_PROMPT_BYTES: usize = 64_000;

/// Build a concise cross-engine handoff briefing from prior turns in the session log when a user
/// switches mid-session to a CLI engine that has no existing external session id yet.
pub fn build_handoff_brief(events: &[crate::events::Envelope], current_turn: u64) -> Option<String> {
    if current_turn <= 1 {
        return None;
    }
    let mut turn = 0u64;
    let mut latest_summary: Option<String> = None;
    let mut exchanges: Vec<(&'static str, String)> = Vec::new();
    let mut edited_files: Vec<String> = Vec::new();

    let clip_text = |s: &str, max_chars: usize| -> String {
        let trimmed = s.trim();
        if trimmed.chars().count() <= max_chars {
            trimmed.to_string()
        } else {
            format!("{}…", trimmed.chars().take(max_chars).collect::<String>())
        }
    };

    for env in events {
        match &env.event {
            Event::TurnStart { turn: t } => turn = *t,
            _ if turn >= current_turn => break,
            Event::ContextCompacted { summary, .. } if !summary.trim().is_empty() => {
                latest_summary = Some(clip_text(summary, 1200));
                exchanges.clear();
            }
            Event::UserMessage { text, images, .. } if !text.trim().is_empty() || !images.is_empty() => {
                let note = match (text.trim().is_empty(), images.len()) {
                    (true, n) => format!("[附带 {n} 张图片]"),
                    (false, 0) => clip_text(text, 500),
                    (false, n) => format!("{} [附带 {n} 张图片]", clip_text(text, 500)),
                };
                exchanges.push(("用户", note));
            }
            Event::AssistantMessage { content, .. } => {
                let text: String = content
                    .iter()
                    .filter(|b| b["type"].as_str() == Some("text"))
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    exchanges.push(("助手", clip_text(&text, 600)));
                }
            }
            Event::ToolResult {
                tool,
                is_error: false,
                meta,
                ..
            } if crate::tools::edits_files(tool) => {
                if let Some(path) = meta["path"].as_str().filter(|p| !p.is_empty())
                    && !edited_files.iter().any(|existing| existing == path)
                {
                    edited_files.push(path.to_string());
                }
            }
            _ => {}
        }
    }

    if latest_summary.is_none() && exchanges.is_empty() && edited_files.is_empty() {
        return None;
    }

    let mut out = String::from(
        "[会话交接上下文 / Cross-Engine Handoff]\n你正在接手同一个会话的后续任务。以下是此前由其他模型/引擎完成的对话与文件改动摘要，请在此基础上无缝继续：\n",
    );
    if let Some(summary) = latest_summary {
        out.push_str("\n前情摘要：\n");
        out.push_str(&summary);
        out.push('\n');
    }
    if !edited_files.is_empty() {
        out.push_str("\n此前已改动的文件：\n");
        for path in edited_files.iter().take(20) {
            out.push_str("- ");
            out.push_str(path);
            out.push('\n');
        }
    }
    if !exchanges.is_empty() {
        out.push_str("\n近期对话记录：\n");
        let start = exchanges.len().saturating_sub(10);
        for (who, msg) in &exchanges[start..] {
            out.push_str(&format!("[{who}] {msg}\n"));
        }
    }
    out.push_str("\n--- 以下是本轮用户的新指令 ---");
    Some(out)
}

/// Build a skills briefing for external CLI turns:
/// - Whenever the user explicitly invokes an `odg` skill (e.g. `按秘籍「pptx」（skill）办` or `/pptx`),
///   inline that skill's full `SKILL.md` instructions so every CLI executes it faithfully.
/// - On the first turn of a CLI session (`include_catalog = true`), also include a compact catalog of
///   available `odg` skills with their `SKILL.md` file paths so the CLI can read any matching skill on demand.
pub fn build_skills_brief(skill_roots: &[PathBuf], user_text: &str, include_catalog: bool) -> Option<String> {
    let skills = crate::skills::discover(skill_roots);
    if skills.is_empty() {
        return None;
    }

    let mut triggered: Vec<&crate::skills::Skill> = Vec::new();
    for skill in &skills {
        let name = skill.name.as_str();
        let slash = format!("/{name}");
        let by_menu = user_text.contains(&format!("「{name}」"));
        let by_slash = user_text.split_whitespace().any(|tok| tok == slash);
        if (by_menu || by_slash) && triggered.len() < 3 {
            triggered.push(skill);
        }
    }

    if triggered.is_empty() && !include_catalog {
        return None;
    }

    let mut sections: Vec<String> = Vec::new();
    for skill in &triggered {
        if let Ok(body) = crate::skills::load(skill_roots, &skill.name) {
            sections.push(format!(
                "[已启用 odg 秘籍：{}（目录：{}）]\n请严格按照以下秘籍说明执行本轮任务：\n\n{}",
                skill.name,
                skill.directory.display(),
                body
            ));
        }
    }

    if include_catalog {
        let mut cat = String::from(
            "[可用 odg 秘籍 / Skills]\n若任务匹配以下专项秘籍，请先读取对应的 SKILL.md 并按其中规范执行：\n",
        );
        for skill in &skills {
            let md = skill.directory.join("SKILL.md");
            cat.push_str(&format!("- {}: {} ({})\n", skill.name, skill.description, md.display()));
        }
        sections.push(cat);
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

/// Build the prompt string passed to an external CLI, prepending any side-conversation briefing,
/// attached file paths (inlining extracted Office/PDF document text and small out-of-workspace files
/// so sandboxed or Python-less CLIs can still read them), and materialized user-attached image paths.
pub fn build_prompt(
    text: &str,
    files: &[Attached],
    image_paths: &[PathBuf],
    workspace: Option<&Path>,
    briefing: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(brief) = briefing
        && !brief.trim().is_empty()
    {
        parts.push(brief.trim().to_string());
    }
    if !files.is_empty() {
        let mut path_only = Vec::new();
        for file in files {
            let outside_workspace = workspace.is_some_and(|ws| !Path::new(&file.path).starts_with(ws));
            let should_inline = file.content.is_some() && (file.document.is_some() || outside_workspace);
            if should_inline {
                parts.push(crate::events::attached_text(file));
            } else {
                path_only.push(file.path.as_str());
            }
        }
        if !path_only.is_empty() {
            let mut list = String::from("Attached files:");
            for p in path_only {
                list.push_str("\n- ");
                list.push_str(p);
            }
            parts.push(list);
        }
    }
    if !image_paths.is_empty() {
        let mut list =
            String::from("Attached images (local files; please read/inspect them directly before answering):");
        for path in image_paths {
            list.push_str("\n- ");
            list.push_str(&path.to_string_lossy());
        }
        parts.push(list);
    }
    if !text.trim().is_empty() {
        parts.push(text.to_string());
    }
    parts.join("\n\n")
}

/// RAII guard cleaning up temporary image files (and their private per-turn directory) materialized for an external CLI turn.
pub struct TempImagesGuard {
    files: Vec<PathBuf>,
    dir: Option<PathBuf>,
}

impl Drop for TempImagesGuard {
    fn drop(&mut self) {
        for path in self.files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

pub(super) fn materialize_cli_images_in(
    base_home: &Path,
    images: &[Image],
    turn: u64,
) -> Result<(Vec<PathBuf>, TempImagesGuard), String> {
    if images.is_empty() {
        return Ok((
            Vec::new(),
            TempImagesGuard {
                files: Vec::new(),
                dir: None,
            },
        ));
    }
    let dir = crate::fsutil::create_private_subdir_in(base_home, "images")
        .map_err(|error| format!("无法创建私有临时图片目录：{error}"))?;
    let mut guard = TempImagesGuard {
        files: Vec::with_capacity(images.len()),
        dir: Some(dir.clone()),
    };
    for (idx, image) in images.iter().enumerate() {
        let ext = match image.media_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "bin",
        };
        let bytes =
            crate::imagegen::unbase64(&image.data).ok_or_else(|| "无法解码附带图片的 base64 数据".to_string())?;
        let path = dir.join(format!("image-{turn}-{idx}.{ext}"));
        crate::fsutil::write_private_atomically(&path, &bytes)
            .map_err(|error| format!("无法写入临时图片文件：{error}"))?;
        guard.files.push(path);
    }
    Ok((guard.files.clone(), guard))
}

/// Decode user-attached base64 images to private `0600` local files inside a dedicated `0700`
/// per-turn directory under `<ODG_HOME>/tmp/images` so external CLIs can inspect them safely.
pub fn materialize_cli_images(images: &[Image], turn: u64) -> Result<(Vec<PathBuf>, TempImagesGuard), String> {
    materialize_cli_images_in(&crate::settings::home(), images, turn)
}

pub(super) fn is_windows_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
}

pub(super) struct TempPromptGuard {
    pub(super) file: Option<PathBuf>,
    pub(super) dir: Option<PathBuf>,
}

impl TempPromptGuard {
    pub(super) fn none() -> Self {
        Self { file: None, dir: None }
    }

    pub(super) fn file_only(file: PathBuf) -> Self {
        Self {
            file: Some(file),
            dir: None,
        }
    }

    pub(super) fn with_subdir(file: PathBuf, dir: PathBuf) -> Self {
        Self {
            file: Some(file),
            dir: Some(dir),
        }
    }
}

impl Drop for TempPromptGuard {
    fn drop(&mut self) {
        if let Some(path) = self.file.take() {
            let _ = std::fs::remove_file(path);
        }
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

pub(super) struct PreparedPrompt {
    pub(super) cli_prompt: String,
    pub(super) stdin_bytes: Option<Vec<u8>>,
    pub(super) extra_dir: Option<PathBuf>,
    pub(super) _guard: TempPromptGuard,
}

pub(super) fn prepare_prompt_delivery_in(
    base_home: &Path,
    kind: CliKind,
    prompt: &str,
    _turn: u64,
    limit: usize,
    batch_script: bool,
) -> Result<PreparedPrompt, String> {
    let supports_stdin = matches!(kind, CliKind::Codex | CliKind::ClaudeCode | CliKind::Cursor);
    if supports_stdin && (batch_script || prompt.len() > limit) {
        return Ok(PreparedPrompt {
            cli_prompt: String::new(),
            stdin_bytes: Some(prompt.as_bytes().to_vec()),
            extra_dir: None,
            _guard: TempPromptGuard::none(),
        });
    }
    let needs_temp_file = prompt.len() > limit || (batch_script && prompt.contains(['\r', '\n']));
    if !needs_temp_file {
        return Ok(PreparedPrompt {
            cli_prompt: prompt.to_string(),
            stdin_bytes: None,
            extra_dir: None,
            _guard: TempPromptGuard::none(),
        });
    }
    // Create a fresh per-turn `0700` subdirectory `<ODG_HOME>/tmp/prompts/<uuid>/prompt.txt`
    // so `agy --add-dir` never exposes prompt files from sibling sessions.
    let dir = crate::fsutil::create_private_subdir_in(base_home, "prompts")
        .map_err(|error| format!("无法创建私有临时提示词目录：{error}"))?;
    let path = dir.join("prompt.txt");
    crate::fsutil::write_private_atomically(&path, prompt.as_bytes()).map_err(|error| {
        let _ = std::fs::remove_dir(&dir);
        format!("无法写入临时提示词文件：{error}")
    })?;
    // Keep this single-line (no `\r`/`\n`) so Windows `.cmd`/`.bat` wrappers never reject it.
    let cli_prompt = format!(
        "本轮用户的完整输入内容已保存到文件：{}。请先完整读取该文件的内容，再直接按其中的要求回答或处理。",
        path.display()
    );
    Ok(PreparedPrompt {
        cli_prompt,
        stdin_bytes: None,
        extra_dir: Some(dir.clone()),
        _guard: TempPromptGuard::with_subdir(path, dir),
    })
}

pub(super) fn prepare_prompt_delivery(
    kind: CliKind,
    prompt: &str,
    turn: u64,
    limit: usize,
    batch_script: bool,
) -> Result<PreparedPrompt, String> {
    prepare_prompt_delivery_in(&crate::settings::home(), kind, prompt, turn, limit, batch_script)
}

/// Adjust `build_args` output when the prompt is delivered via `stdin` or accompanied by an extra readable directory.
pub(super) fn apply_prompt_delivery_args(kind: CliKind, args: &mut Vec<String>, prepared: &PreparedPrompt) {
    if prepared.stdin_bytes.is_some() {
        // `Codex`, `ClaudeCode`, and `Cursor` append `["--", prompt]` at the tail of `build_args`.
        if args.len() >= 2 && args[args.len() - 2] == "--" {
            args.truncate(args.len() - 2);
        }
        if kind == CliKind::Codex {
            args.push("-".to_string());
        }
    }
    if kind == CliKind::Antigravity
        && let Some(dir) = prepared.extra_dir.as_deref().and_then(Path::to_str)
    {
        args.push("--add-dir".to_string());
        args.push(dir.to_string());
    }
}

/// Insert CLI flags before the trailing `-- <prompt>` (or `-`) separator when present, or append at the end.
pub(super) fn insert_before_prompt_tail(args: &mut Vec<String>, extra: Vec<String>) {
    if extra.is_empty() {
        return;
    }
    let pos = if args.len() >= 2 && args[args.len() - 2] == "--" {
        args.len() - 2
    } else if args.last().is_some_and(|a| a == "-") {
        args.len() - 1
    } else {
        args.len()
    };
    args.splice(pos..pos, extra);
}
