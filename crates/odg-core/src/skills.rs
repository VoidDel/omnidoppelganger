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

/// The skills that come with the app, by name, as their `SKILL.md`. None is
/// in use until the user switches it on, which writes it into the user's
/// skills directory; switched off, it is taken out again.
pub const BUILTINS: &[(&str, &str)] = &[
    ("commit-message", include_str!("../skills/commit-message/SKILL.md")),
    ("code-review", include_str!("../skills/code-review/SKILL.md")),
    ("debugging", include_str!("../skills/debugging/SKILL.md")),
    ("writing-tests", include_str!("../skills/writing-tests/SKILL.md")),
    ("refactoring", include_str!("../skills/refactoring/SKILL.md")),
    ("writing-docs", include_str!("../skills/writing-docs/SKILL.md")),
    ("frontend-design", include_str!("../skills/frontend-design/SKILL.md")),
    ("release-notes", include_str!("../skills/release-notes/SKILL.md")),
    ("security-review", include_str!("../skills/security-review/SKILL.md")),
];

/// A skill as the 秘籍 page lists it: one of the app's, on or off, or one of the user's own.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Listed {
    pub name: String,
    pub description: String,
    /// Where it is, or would be once switched on.
    pub directory: String,
    pub builtin: bool,
    /// In the user's skills directory, and so offered to every session.
    pub installed: bool,
    /// One of the app's whose instructions the user has changed: switching it off would lose that.
    pub edited: bool,
}

/// The same text, however its lines end.
fn same_text(one: &str, other: &str) -> bool {
    one.replace("\r\n", "\n").trim_end() == other.replace("\r\n", "\n").trim_end()
}

/// The skills of the user's skills directory `root`: the app's own, on or
/// off, then the others found there, by name.
pub fn catalog(root: &Path) -> Vec<Listed> {
    let mut listed: Vec<Listed> = BUILTINS
        .iter()
        .map(|(name, text)| {
            let directory = root.join(name);
            let held = std::fs::read_to_string(directory.join(FILE_NAME)).ok();
            let (_, description, _) = parse(text);
            Listed {
                name: name.to_string(),
                description: description.unwrap_or_default(),
                directory: directory.display().to_string(),
                builtin: true,
                installed: held.is_some(),
                edited: held.is_some_and(|held| !same_text(&held, text)),
            }
        })
        .collect();
    let own = discover(&[root.to_path_buf()])
        .into_iter()
        .filter(|skill| !BUILTINS.iter().any(|(name, _)| skill.directory.file_name().is_some_and(|folder| folder == *name)))
        .map(|skill| Listed { name: skill.name, description: skill.description, directory: skill.directory.display().to_string(), builtin: false, installed: true, edited: false });
    listed.extend(own);
    listed
}

/// Switch one of the app's skills on, which writes it into `root`, or off,
/// which takes it out. One already there is not written over; one the user
/// changed is taken out only with `force`.
pub fn switch(root: &Path, name: &str, on: bool, force: bool) -> Result<(), String> {
    let Some((_, text)) = BUILTINS.iter().find(|(builtin, _)| *builtin == name) else {
        return Err(format!("`{name}` is not one of the app's skills"));
    };
    let directory = root.join(name);
    let file = directory.join(FILE_NAME);
    if on {
        if file.is_file() {
            return Ok(());
        }
        std::fs::create_dir_all(&directory).map_err(|error| format!("Cannot make {}: {error}", directory.display()))?;
        return std::fs::write(&file, text).map_err(|error| format!("Cannot write {}: {error}", file.display()));
    }
    if !directory.exists() {
        return Ok(());
    }
    let edited = std::fs::read_to_string(&file).is_ok_and(|held| !same_text(&held, text)) || std::fs::read_dir(&directory).map(|entries| entries.count() > 1).unwrap_or(false);
    if edited && !force {
        return Err("edited".to_string());
    }
    std::fs::remove_dir_all(&directory).map_err(|error| format!("Cannot take away {}: {error}", directory.display()))
}

/// Take one of the user's own skills out of `root`, by its name: only a folder directly in `root` is ever taken.
pub fn remove(root: &Path, name: &str) -> Result<(), String> {
    let skill = discover(&[root.to_path_buf()]).into_iter().find(|skill| skill.name == name).ok_or_else(|| format!("There is no skill `{name}` in {}.", root.display()))?;
    if skill.directory.parent() != Some(root) {
        return Err(format!("{} is not in {}.", skill.directory.display(), root.display()));
    }
    std::fs::remove_dir_all(&skill.directory).map_err(|error| format!("Cannot take away {}: {error}", skill.directory.display()))
}

/// The most a skill installed may hold: instructions and the scripts and references beside them, not a project.
const INSTALL_FILES: usize = 2_000;
const INSTALL_BYTES: u64 = 50 * 1024 * 1024;

/// Where a skill is to come from when it is fetched with Git: the address to
/// clone, the branch, and the folder in the repository. A GitHub address to a
/// folder (`https://github.com/<owner>/<repo>/tree/<branch>/<folder>`) is
/// taken apart; any other is cloned as it is. `None` for what is not a Git
/// address, which is a folder on this computer.
pub fn git_source(source: &str) -> Option<(String, Option<String>, Option<String>)> {
    let remote = ["https://", "http://", "git@", "ssh://"].iter().any(|start| source.starts_with(start)) || source.ends_with(".git");
    if !remote {
        return None;
    }
    if let Some(rest) = source.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = rest.trim_end_matches('/').splitn(5, '/').collect();
        if let [owner, repository, "tree", branch, rest @ ..] = parts.as_slice() {
            let folder = rest.first().map(|folder| folder.to_string()).filter(|folder| !folder.is_empty());
            return Some((format!("https://github.com/{owner}/{}.git", repository.trim_end_matches(".git")), Some(branch.to_string()), folder));
        }
    }
    Some((source.to_string(), None, None))
}

/// The folders below `root`, at most three down, that hold a `SKILL.md`, relative to it: to name when `root` does not hold one itself.
fn skills_within(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut walk = vec![(root.to_path_buf(), 0)];
    while let Some((directory, depth)) = walk.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else { continue };
        for entry in entries.filter_map(Result::ok).filter(|entry| entry.path().is_dir() && entry.file_name() != ".git") {
            if entry.path().join(FILE_NAME).is_file() {
                found.push(entry.path().strip_prefix(root).unwrap_or(&entry.path()).to_string_lossy().replace('\\', "/"));
            } else if depth < 2 {
                walk.push((entry.path(), depth + 1));
            }
        }
    }
    found.sort();
    found
}

/// Copy the skill in `source` (a folder holding `SKILL.md`) into the skills
/// directory `into`, in a folder named for the skill. A skill of that name
/// already there is put in its place only with `replace`. What is copied
/// goes in whole or not at all.
pub fn install(source: &Path, into: &Path, replace: bool) -> Result<Skill, String> {
    let Some((skill, _)) = read(source) else {
        let within = skills_within(source);
        return Err(if within.is_empty() {
            format!("There is no {FILE_NAME} in {}, nor in the folders below it: it is not a skill.", source.display())
        } else {
            format!("There is no {FILE_NAME} at the top of {}. Skills in the folders below it: {}. Pass the one wanted as `path`.", source.display(), within.join(", "))
        });
    };
    // The folder takes the skill's name, in the characters any file system allows.
    let folder: String = skill.name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect::<String>().trim_matches('-').to_string();
    if folder.is_empty() {
        return Err(format!("The skill's name `{}` gives no folder name.", skill.name));
    }
    let destination = into.join(&folder);
    if destination.exists() && !replace {
        return Err(format!("A skill is already installed at {}. Pass `replace` to put this one in its place.", destination.display()));
    }
    // What to copy, counted before anything is written.
    let mut files: Vec<PathBuf> = Vec::new();
    let mut bytes = 0u64;
    let mut walk = vec![source.to_path_buf()];
    while let Some(directory) = walk.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|error| format!("Cannot read {}: {error}", directory.display()))?.filter_map(Result::ok) {
            let (path, kind) = (entry.path(), entry.file_type().map_err(|error| error.to_string())?);
            if entry.file_name() == ".git" || kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                walk.push(path);
            } else {
                bytes += entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                files.push(path);
            }
            if files.len() > INSTALL_FILES || bytes > INSTALL_BYTES {
                return Err(format!("{} holds more than a skill does ({INSTALL_FILES} files or {} MB): it is not installed.", source.display(), INSTALL_BYTES / 1024 / 1024));
            }
        }
    }
    std::fs::create_dir_all(into).map_err(|error| format!("Cannot make {}: {error}", into.display()))?;
    let staging = into.join(format!(".installing-{folder}"));
    let _ = std::fs::remove_dir_all(&staging);
    let copied = files.iter().try_for_each(|file| {
        let target = staging.join(file.strip_prefix(source).map_err(|error| error.to_string())?);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::copy(file, &target).map(|_| ()).map_err(|error| format!("Cannot copy {}: {error}", file.display()))
    });
    let placed = copied.and_then(|()| {
        if destination.exists() {
            std::fs::remove_dir_all(&destination).map_err(|error| format!("Cannot take away the skill at {}: {error}", destination.display()))?;
        }
        std::fs::rename(&staging, &destination).map_err(|error| format!("Cannot put the skill in {}: {error}", destination.display()))
    });
    if let Err(error) = placed {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok(Skill { directory: destination, ..skill })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_a_skill_whole_by_its_name_and_says_where_one_is_in_a_bigger_folder() {
        let directory = tempfile::tempdir().unwrap();
        let (source, into) = (directory.path().join("downloaded"), directory.path().join("skills"));
        skill(&source.join("skills"), "pdf", "---\nname: pdf tools\ndescription: Work with PDFs.\n---\nUse the script.\n");
        std::fs::create_dir_all(source.join("skills/pdf/scripts")).unwrap();
        std::fs::write(source.join("skills/pdf/scripts/split.py"), "print('split')\n").unwrap();
        std::fs::create_dir_all(source.join("skills/pdf/.git")).unwrap();
        std::fs::write(source.join("skills/pdf/.git/HEAD"), "ref").unwrap();

        // A folder that holds skills but is not one names them.
        let missed = install(&source, &into, false).unwrap_err();
        assert!(missed.contains("skills/pdf") && missed.contains("`path`"), "{missed}");
        // The skill goes in a folder named for it, scripts and all, its Git folder left behind.
        let installed = install(&source.join("skills/pdf"), &into, false).unwrap();
        assert_eq!((installed.name.as_str(), installed.directory.clone()), ("pdf tools", into.join("pdf-tools")));
        assert!(into.join("pdf-tools/scripts/split.py").is_file() && !into.join("pdf-tools/.git").exists());
        assert_eq!(discover(std::slice::from_ref(&into))[0].name, "pdf tools");
        // Not over one already there, unless asked to.
        assert!(install(&source.join("skills/pdf"), &into, false).unwrap_err().contains("replace"));
        std::fs::write(source.join("skills/pdf/SKILL.md"), "---\nname: pdf tools\ndescription: Better.\n---\nNew.\n").unwrap();
        install(&source.join("skills/pdf"), &into, true).unwrap();
        assert_eq!(discover(std::slice::from_ref(&into))[0].description, "Better.");
        assert!(std::fs::read_dir(&into).unwrap().count() == 1, "nothing is left half installed");
        assert!(install(&directory.path().join("empty"), &into, false).unwrap_err().contains("not a skill"));
    }

    #[test]
    fn switches_the_apps_skills_on_and_off_and_keeps_what_the_user_changed() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        // Each that comes with the app reads as a skill: named as listed, and described.
        for (name, text) in BUILTINS {
            let (named, described, body) = parse(text);
            assert_eq!(named.as_deref(), Some(*name));
            assert!(described.is_some_and(|text| !text.is_empty()) && !body.trim().is_empty(), "{name}");
        }
        // None is on until switched on.
        let listed = catalog(&root);
        assert_eq!(listed.len(), BUILTINS.len());
        assert!(listed.iter().all(|skill| skill.builtin && !skill.installed));
        switch(&root, "debugging", true, false).unwrap();
        assert_eq!(discover(std::slice::from_ref(&root)).iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>(), ["debugging"]);
        assert!(catalog(&root).iter().any(|skill| skill.name == "debugging" && skill.installed && !skill.edited));
        // Off takes it out; one the user changed, only when told to.
        switch(&root, "debugging", false, false).unwrap();
        assert!(!root.join("debugging").exists());
        switch(&root, "code-review", true, false).unwrap();
        std::fs::write(root.join("code-review").join(FILE_NAME), "---\nname: code-review\ndescription: Mine now.\n---\nMy way.\n").unwrap();
        assert!(catalog(&root).iter().any(|skill| skill.name == "code-review" && skill.edited));
        // Switching it on again keeps the user's text.
        switch(&root, "code-review", true, false).unwrap();
        assert!(std::fs::read_to_string(root.join("code-review").join(FILE_NAME)).unwrap().contains("My way."));
        assert_eq!(switch(&root, "code-review", false, false), Err("edited".to_string()));
        switch(&root, "code-review", false, true).unwrap();
        assert!(!root.join("code-review").exists());
        assert!(switch(&root, "nope", true, false).is_err());
        // The user's own are listed after, and can be taken away; nothing outside the directory can.
        skill(&root, "mine", "---\nname: mine\ndescription: My own.\n---\nDo it my way.\n");
        let own = catalog(&root).into_iter().find(|skill| !skill.builtin).unwrap();
        assert_eq!((own.name.as_str(), own.installed), ("mine", true));
        remove(&root, "mine").unwrap();
        assert!(!root.join("mine").exists() && remove(&root, "mine").is_err());
    }

    #[test]
    fn takes_a_github_address_to_a_folder_apart() {
        assert_eq!(
            git_source("https://github.com/anthropics/skills/tree/main/skills/pdf"),
            Some(("https://github.com/anthropics/skills.git".into(), Some("main".into()), Some("skills/pdf".into())))
        );
        assert_eq!(git_source("https://gitee.com/me/skill.git"), Some(("https://gitee.com/me/skill.git".into(), None, None)));
        assert_eq!(git_source("git@github.com:me/skill.git"), Some(("git@github.com:me/skill.git".into(), None, None)));
        assert_eq!(git_source("D:\\skills\\pdf"), None);
        assert_eq!(git_source("../skills/pdf"), None);
    }

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
