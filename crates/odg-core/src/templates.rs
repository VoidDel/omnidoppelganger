//! Parameterized Markdown prompt templates (`~/.odg/prompts/*.md` and `<workspace>/.odg/prompts/*.md`).
//!
//! Each `.md` file defines a reusable slash command named after its filename stem (or frontmatter `name`),
//! with optional YAML frontmatter (`description`, `argument-hint` / `argument_hint`) and shell-style
//! positional parameter substitution (`$1`, `$2`, `$@`, `$ARGUMENTS`, `${1:-default}`, `${@:2}`, `${@:2:3}`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_TEMPLATES: usize = 80;
const DESCRIPTION_CHARS: usize = 240;
const BODY_CHARS: usize = 50_000;

/// Metadata of a discovered prompt template surfaced to the UI / CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptTemplateInfo {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    /// `"user"` (`~/.odg/prompts`) or `"project"` (`<workspace>/.odg/prompts`).
    pub scope: String,
}

/// Full template including its unexpanded Markdown body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub info: PromptTemplateInfo,
    pub body: String,
    pub path: PathBuf,
}

fn unescape_double_quoted(quoted: &str) -> String {
    let mut out = String::with_capacity(quoted.len());
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some(next @ ('"' | '\\'))) => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

fn parse_frontmatter(text: &str) -> (Option<String>, Option<String>, Option<String>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---") else {
        return (None, None, None, text);
    };
    if !rest.starts_with('\n') && !rest.starts_with("\r\n") {
        return (None, None, None, text);
    }
    let Some(end) = rest.find("\n---") else {
        return (None, None, None, text);
    };
    let header = &rest[..end];
    let body = rest[end + 4..].trim_start_matches('-').trim_start_matches(['\r', '\n']);

    let field = |keys: &[&str]| -> Option<String> {
        header.lines().find_map(|line| {
            let trimmed = line.trim_start();
            for key in keys {
                if let Some(after_key) = trimmed.strip_prefix(key) {
                    let Some(raw) = after_key.trim_start().strip_prefix(':') else {
                        continue;
                    };
                    let val = raw.trim();
                    let val = match val.strip_prefix('"').and_then(|inner| inner.strip_suffix('"')) {
                        Some(inner) => unescape_double_quoted(inner),
                        None => val
                            .strip_prefix('\'')
                            .and_then(|inner| inner.strip_suffix('\''))
                            .unwrap_or(val)
                            .to_string(),
                    };
                    if !val.is_empty() {
                        return Some(val);
                    }
                }
            }
            None
        })
    };

    (
        field(&["name"]),
        field(&["description"]),
        field(&["argument-hint", "argument_hint", "args"]),
        body,
    )
}

fn sanitize_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        .collect()
}

fn read_template_file(path: &Path, scope: &str) -> Option<PromptTemplate> {
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return None;
    }
    let stem = path.file_stem()?.to_string_lossy();
    let text = std::fs::read_to_string(path).ok()?;
    let (fm_name, fm_desc, fm_hint, body) = parse_frontmatter(&text);
    let name = sanitize_name(fm_name.as_deref().unwrap_or(&stem));
    if name.is_empty() {
        return None;
    }
    let body_trimmed = body.trim();
    if body_trimmed.is_empty() {
        return None;
    }
    let fallback_desc = || {
        body_trimmed
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .trim_start_matches('#')
            .trim()
            .to_string()
    };
    let description: String = fm_desc
        .unwrap_or_else(fallback_desc)
        .chars()
        .take(DESCRIPTION_CHARS)
        .collect();
    let clipped_body: String = body_trimmed.chars().take(BODY_CHARS).collect();
    Some(PromptTemplate {
        info: PromptTemplateInfo {
            name,
            description,
            argument_hint: fm_hint,
            scope: scope.to_string(),
        },
        body: clipped_body,
        path: path.to_path_buf(),
    })
}

/// Discover templates in `user_dir` (`~/.odg/prompts`) and optionally `project_dir`
/// (`<workspace>/.odg/prompts`, passed only when the workspace is trusted).
/// Project templates override user templates with the same `name`.
pub fn discover(user_dir: &Path, project_dir: Option<&Path>) -> Vec<PromptTemplate> {
    let mut templates: Vec<PromptTemplate> = Vec::new();
    let mut scan = |dir: &Path, scope: &str| {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let Some(tpl) = read_template_file(&path, scope) else {
                continue;
            };
            templates.retain(|existing| !existing.info.name.eq_ignore_ascii_case(&tpl.info.name));
            templates.push(tpl);
        }
    };
    scan(user_dir, "user");
    if let Some(proj) = project_dir {
        scan(proj, "project");
    }
    templates.sort_by(|a, b| a.info.name.cmp(&b.info.name));
    templates.truncate(MAX_TEMPLATES);
    templates
}

/// Metadata listing for UI / slash menu.
pub fn listing(user_dir: &Path, project_dir: Option<&Path>) -> Vec<PromptTemplateInfo> {
    discover(user_dir, project_dir).into_iter().map(|t| t.info).collect()
}

/// Find and expand a named template with `raw_args`.
pub fn expand_named(user_dir: &Path, project_dir: Option<&Path>, name: &str, raw_args: &str) -> Result<String, String> {
    let all = discover(user_dir, project_dir);
    let tpl = all
        .into_iter()
        .find(|t| t.info.name.eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| format!("未找到提示词模板 `/{name}`"))?;
    Ok(expand(&tpl.body, raw_args))
}

/// Split an argument string into shell-like positional words, honoring single quotes,
/// double quotes (`\"` / `\\`), and backslash-escaped spaces.
pub fn split_args(raw: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = raw.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            Some('"') => match c {
                '"' => quote = None,
                '\\' => {
                    if let Some(&next @ ('"' | '\\' | '$')) = chars.peek() {
                        cur.push(next);
                        chars.next();
                    } else {
                        cur.push('\\');
                    }
                }
                other => cur.push(other),
            },
            _ => match c {
                c if c.is_whitespace() => {
                    if in_word {
                        args.push(std::mem::take(&mut cur));
                        in_word = false;
                    }
                }
                '\'' | '"' => {
                    in_word = true;
                    quote = Some(c);
                }
                '\\' => {
                    in_word = true;
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    }
                }
                other => {
                    in_word = true;
                    cur.push(other);
                }
            },
        }
    }
    if in_word {
        args.push(cur);
    }
    args
}

/// Expand parameter placeholders in `body` in a single pass so substituted argument text is never re-parsed.
///
/// Supported forms:
/// - `$1` .. `$99`: 1-based positional parameter (empty if omitted)
/// - `$@`, `$ARGUMENTS`: all positional parameters joined with spaces
/// - `${1:-default}`: positional parameter `1`, or `default` when empty/omitted
/// - `${@:-default}` / `${ARGUMENTS:-default}`: all parameters, or `default` when none given
/// - `${@:N}` / `${@:N:L}`: 1-based slice of parameters starting at `N` (optionally length `L`)
/// - `$$`: literal `$`
pub fn expand(body: &str, raw_args: &str) -> String {
    let args = split_args(raw_args);
    let all_joined = args.join(" ");
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len() + raw_args.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            let ch = body[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // Starts at '$'
        if i + 1 >= bytes.len() {
            out.push('$');
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            b'$' => {
                out.push('$');
                i += 2;
            }
            b'@' => {
                out.push_str(&all_joined);
                i += 2;
            }
            b'{' => {
                if let Some(rel_end) = body[i + 2..].find('}') {
                    let expr = &body[i + 2..i + 2 + rel_end];
                    if let Some(replaced) = eval_brace_expr(expr, &args, &all_joined) {
                        out.push_str(&replaced);
                        i += 2 + rel_end + 1;
                        continue;
                    }
                }
                out.push('$');
                i += 1;
            }
            b'1'..=b'9' => {
                let mut end = i + 2;
                while end < bytes.len() && bytes[end].is_ascii_digit() && end - (i + 1) < 3 {
                    end += 1;
                }
                if let Ok(idx) = body[i + 1..end].parse::<usize>() {
                    if let Some(val) = idx.checked_sub(1).and_then(|zero| args.get(zero)) {
                        out.push_str(val);
                    }
                    i = end;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            _ => {
                if body[i + 1..].starts_with("ARGUMENTS")
                    && !body[i + 1 + "ARGUMENTS".len()..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    out.push_str(&all_joined);
                    i += 1 + "ARGUMENTS".len();
                } else {
                    out.push('$');
                    i += 1;
                }
            }
        }
    }
    out
}

fn eval_brace_expr(expr: &str, args: &[String], all_joined: &str) -> Option<String> {
    if expr == "@" || expr == "ARGUMENTS" {
        return Some(all_joined.to_string());
    }
    if let Some(default) = expr.strip_prefix("@:-").or_else(|| expr.strip_prefix("ARGUMENTS:-")) {
        return Some(if all_joined.is_empty() {
            default.to_string()
        } else {
            all_joined.to_string()
        });
    }
    if let Some(slice_spec) = expr.strip_prefix("@:") {
        let mut parts = slice_spec.splitn(2, ':');
        let start_1: usize = parts.next()?.parse().ok()?;
        let start = start_1.saturating_sub(1);
        if start >= args.len() {
            return Some(String::new());
        }
        let slice = if let Some(len_str) = parts.next() {
            let len: usize = len_str.parse().ok()?;
            &args[start..(start + len).min(args.len())]
        } else {
            &args[start..]
        };
        return Some(slice.join(" "));
    }
    if let Some((num_str, default)) = expr.split_once(":-") {
        let idx: usize = num_str.parse().ok()?;
        let val = idx
            .checked_sub(1)
            .and_then(|zero| args.get(zero))
            .map(String::as_str)
            .unwrap_or("");
        return Some(if val.is_empty() {
            default.to_string()
        } else {
            val.to_string()
        });
    }
    if let Ok(idx) = expr.parse::<usize>() {
        return Some(
            idx.checked_sub(1)
                .and_then(|zero| args.get(zero))
                .cloned()
                .unwrap_or_default(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_shell_arguments_and_expands_placeholders_once() {
        let parsed = split_args(r#"auth.rs "fix login bug" 'keep $2 literal' escaped\ word"#);
        assert_eq!(
            parsed,
            vec!["auth.rs", "fix login bug", "keep $2 literal", "escaped word"]
        );

        let tpl = "检查文件 $1（分支 ${2:-main}），其余参数：${@:3:2}；全部：$ARGUMENTS；字面量 $$1";
        let expanded = expand(tpl, r#""$2_injected.rs" feat/sso --strict --verbose extra"#);
        assert_eq!(
            expanded,
            "检查文件 $2_injected.rs（分支 feat/sso），其余参数：--strict --verbose；全部：$2_injected.rs feat/sso --strict --verbose extra；字面量 $1"
        );

        assert_eq!(expand("目标：${1:-HEAD} / ${@:-默认全仓}", ""), "目标：HEAD / 默认全仓");
    }

    #[test]
    fn discovers_user_and_trusted_project_templates_with_override() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user_prompts");
        let proj = dir.path().join("repo/.odg/prompts");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&proj).unwrap();

        std::fs::write(
            user.join("check-pr.md"),
            "---\ndescription: 全局 PR 审查\nargument-hint: <branch>\n---\n请对比 ${1:-main} 分支审查变更。",
        )
        .unwrap();
        std::fs::write(
            user.join("ship.md"),
            "# 构建并检查发布清单\n运行测试并核对 CHANGELOG：$@",
        )
        .unwrap();
        std::fs::write(
            proj.join("check-pr.md"),
            "---\ndescription: 项目专属 PR 审查\nargument-hint: [base-branch]\n---\n按项目规范审查相对 ${1:-develop} 的改动。",
        )
        .unwrap();

        // Untrusted project: project_dir = None -> only user templates
        let untrusted = listing(&user, None);
        assert_eq!(untrusted.len(), 2);
        let user_pr = untrusted.iter().find(|t| t.name == "check-pr").unwrap();
        assert_eq!(user_pr.scope, "user");
        assert_eq!(user_pr.description, "全局 PR 审查");
        assert_eq!(user_pr.argument_hint.as_deref(), Some("<branch>"));

        // Trusted project: project overrides check-pr
        let trusted = listing(&user, Some(&proj));
        assert_eq!(trusted.len(), 2);
        let proj_pr = trusted.iter().find(|t| t.name == "check-pr").unwrap();
        assert_eq!(proj_pr.scope, "project");
        assert_eq!(proj_pr.description, "项目专属 PR 审查");
        assert_eq!(proj_pr.argument_hint.as_deref(), Some("[base-branch]"));

        let out = expand_named(&user, Some(&proj), "check-pr", "").unwrap();
        assert_eq!(out, "按项目规范审查相对 develop 的改动。");
    }
}
