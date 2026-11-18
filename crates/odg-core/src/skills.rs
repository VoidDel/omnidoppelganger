//! Skills: instructions for particular kinds of work, kept out of the prompt
//! until a task calls for them.
//!
//! A skill is a directory holding `SKILL.md`. The file opens with a block
//! between `---` lines that gives its `name` and a one-line `description`;
//! the rest is the instructions. The model is told only each skill's name
//! and description, and reads the instructions with `load_skill` when a task
//! matches. Skills are looked for in the user's `skills` directory under the
//! home directory and in `.odg/skills` in the workspace; where both hold a
//! skill of the same name, the workspace's is used.

use std::path::{Path, PathBuf};

use crate::events::SkillInfo;

const FILE_NAME: &str = "SKILL.md";
/// More skills than this are not listed: the list is sent with every request.
const LISTED: usize = 50;
const DESCRIPTION_CHARS: usize = 300;
/// A skill's instructions are replayed in every later request, like a file read.
const BODY_CHARS: usize = 50_000;

/// A skill found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The directory that holds `SKILL.md` and whatever else the skill refers to.
    pub directory: PathBuf,
}

/// Split a `SKILL.md` into the name and description its opening block gives, and the instructions after it.
fn parse(text: &str) -> (Option<String>, Option<String>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---") else { return (None, None, text) };
    let Some(end) = rest.find("\n---") else { return (None, None, text) };
    let body = rest[end + 4..].trim_start_matches(['-']).trim_start_matches(['\r', '\n']);
    let field = |key: &str| {
        rest[..end].lines().find_map(|line| {
            let value = line.strip_prefix(key)?.trim_start().strip_prefix(':')?.trim();
            // A value may be quoted, as YAML allows.
            let value = value.strip_prefix('"').and_then(|inner| inner.strip_suffix('"')).or_else(|| value.strip_prefix('\'').and_then(|inner| inner.strip_suffix('\''))).unwrap_or(value);
            (!value.is_empty()).then(|| value.to_string())
        })
    };
    (field("name"), field("description"), body)
}

fn read(directory: &Path) -> Option<(Skill, String)> {
    let text = std::fs::read_to_string(directory.join(FILE_NAME)).ok()?;
    let (name, description, body) = parse(&text);
    let folder = directory.file_name()?.to_string_lossy().into_owned();
    // A skill that does not describe itself is described by how its instructions open.
    let opening = || body.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().trim_start_matches('#').trim().to_string();
    let description: String = description.unwrap_or_else(opening).chars().take(DESCRIPTION_CHARS).collect();
    Some((Skill { name: name.unwrap_or(folder), description, directory: directory.to_path_buf() }, body.to_string()))
}

/// Every skill under `roots`, by name. A later root's skill replaces an earlier root's of the same name.
pub fn discover(roots: &[PathBuf]) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let Some((skill, _)) = read(&entry.path()) else { continue };
            skills.retain(|other| other.name != skill.name);
            skills.push(skill);
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills.truncate(LISTED);
    skills
}

/// What the model is told of the skills: names and descriptions.
pub fn listing(skills: &[Skill]) -> Vec<SkillInfo> {
    skills.iter().map(|skill| SkillInfo { name: skill.name.clone(), description: skill.description.clone() }).collect()
}

/// The instructions of the skill called `name`, as `load_skill` returns them.
pub fn load(roots: &[PathBuf], name: &str) -> Result<String, String> {
    let skills = discover(roots);
    let Some(skill) = skills.iter().find(|skill| skill.name == name) else {
        let known: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
        return Err(if known.is_empty() { format!("There is no skill `{name}`: no skills are installed.") } else { format!("There is no skill `{name}`. The skills are: {}.", known.join(", ")) });
    };
    let (_, body) = read(&skill.directory).ok_or_else(|| format!("The skill `{name}` can no longer be read."))?;
    let mut body: String = if body.chars().count() > BODY_CHARS {
        format!("{}\n[... the skill's instructions are cut at {BODY_CHARS} characters ...]", body.chars().take(BODY_CHARS).collect::<String>())
    } else {
        body
    };
    // Instructions often point at files beside them; the model needs to know where that is.
    body.push_str(&format!("\n\n[This skill's files are in {}. Read the ones it refers to from there.]", skill.directory.display()));
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(root: &Path, folder: &str, text: &str) {
        std::fs::create_dir_all(root.join(folder)).unwrap();
        std::fs::write(root.join(folder).join(FILE_NAME), text).unwrap();
    }

    #[test]
    fn finds_skills_and_lets_the_workspace_replace_the_users() {
        let directory = tempfile::tempdir().unwrap();
        let (user, project) = (directory.path().join("user"), directory.path().join("project"));
        skill(&user, "release", "---\nname: release\ndescription: \"How to cut a release.\"\n---\n\n# Releasing\nTag, then build.\n");
        skill(&user, "review", "---\nname: code-review\ndescription: How to review a change.\n---\nRead the diff first.\n");
        skill(&user, "plain", "# Writing changelogs\n\nKeep entries short.\n");
        skill(&project, "release", "---\nname: release\ndescription: This project's release steps.\n---\nRun `pnpm e2e` first.\n");
        std::fs::create_dir_all(user.join("empty")).unwrap();
        let roots = [user.clone(), project.clone(), directory.path().join("missing")];

        let found = discover(&roots);
        let listed: Vec<(&str, &str)> = found.iter().map(|skill| (skill.name.as_str(), skill.description.as_str())).collect();
        // By name; a skill without an opening block is named for its folder and described by its first line.
        assert_eq!(listed, [("code-review", "How to review a change."), ("plain", "Writing changelogs"), ("release", "This project's release steps.")]);
        assert_eq!(found[2].directory, project.join("release"));
        assert_eq!(listing(&found)[0], SkillInfo { name: "code-review".into(), description: "How to review a change.".into() });

        // Loading returns the instructions without the opening block, and says where the skill's files are.
        let loaded = load(&roots, "release").unwrap();
        assert!(loaded.starts_with("Run `pnpm e2e` first.\n") && loaded.contains(&project.join("release").display().to_string()), "{loaded}");
        assert!(load(&roots, "code-review").unwrap().starts_with("Read the diff first."));
        assert_eq!(load(&roots, "nope").unwrap_err(), "There is no skill `nope`. The skills are: code-review, plain, release.");
        assert_eq!(load(&[directory.path().join("missing")], "x").unwrap_err(), "There is no skill `x`: no skills are installed.");
    }
}
