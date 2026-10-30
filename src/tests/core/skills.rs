use super::*;

fn write_skill(root: &Path, tier: &str, name: &str) {
    let dir = root.join(tier).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} from {tier}\n---\n\n# {name}\n"),
    )
    .unwrap();
}

fn find(manager: &SkillManager, name: &str) -> Option<SkillMetadata> {
    manager
        .list_skills()
        .into_iter()
        .find(|skill| skill.name == name)
}

#[test]
fn a_repo_skills_tree_is_a_load_root() {
    let temp = tempfile::tempdir().unwrap();
    write_skill(temp.path(), "skills/autonomous-agents", "seavx-dev-probe");

    let manager = SkillManager::new(temp.path());
    let skill = find(&manager, "seavx-dev-probe").expect("skills/<category>/<name>/SKILL.md");
    assert_eq!(
        skill.description,
        "seavx-dev-probe from skills/autonomous-agents"
    );
    assert_eq!(
        skill.path,
        temp.path()
            .join("skills/autonomous-agents/seavx-dev-probe/SKILL.md")
    );
}

#[test]
fn the_hidden_project_dir_shadows_the_open_one() {
    let temp = tempfile::tempdir().unwrap();
    write_skill(temp.path(), ".seavx/skills", "seavx-shadow-probe");
    write_skill(
        temp.path(),
        "skills/autonomous-agents",
        "seavx-shadow-probe",
    );

    let manager = SkillManager::new(temp.path());
    let skill = find(&manager, "seavx-shadow-probe").unwrap();
    assert_eq!(
        skill.path,
        temp.path()
            .join(".seavx/skills/seavx-shadow-probe/SKILL.md"),
        "the hidden project dir wins, and the open one is not listed twice"
    );
    let listed: Vec<_> = manager
        .list_skills()
        .into_iter()
        .filter(|skill| skill.name == "seavx-shadow-probe")
        .collect();
    assert_eq!(listed.len(), 1, "the open copy is not listed twice");
    assert_eq!(
        listed[0].path,
        temp.path()
            .join(".seavx/skills/seavx-shadow-probe/SKILL.md")
    );
}

#[test]
fn a_new_skill_is_still_written_into_the_hidden_project_dir() {
    let temp = tempfile::tempdir().unwrap();
    let manager = SkillManager::new(temp.path());

    let path = manager
        .save_skill(
            "probe-written",
            "---\nname: probe-written\ndescription: x\n---\n",
        )
        .unwrap();
    assert_eq!(
        path,
        temp.path().join(".seavx/skills/probe-written/SKILL.md")
    );
}

#[test]
fn a_skill_dir_without_a_skill_file_is_walked_one_level_deeper() {
    let temp = tempfile::tempdir().unwrap();
    write_skill(temp.path(), "skills/category/nested", "seavx-nested-probe");

    let manager = SkillManager::new(temp.path());
    assert!(find(&manager, "seavx-nested-probe").is_some());
}
