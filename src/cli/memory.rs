//! `seavx memory`: print the project memory, user preferences and installed skills.

use std::path::PathBuf;

pub fn run() -> anyhow::Result<()> {
    let memory_candidates = [
        PathBuf::from(".seavx/MEMORY.md"),
        PathBuf::from("MEMORY.md"),
    ];
    let mut found_mem = false;
    for memory_path in &memory_candidates {
        if memory_path.exists() {
            println!("--- Project Memory ({}) ---", memory_path.display());
            match std::fs::read_to_string(memory_path) {
                Ok(c) => println!("{c}"),
                Err(e) => eprintln!("Error reading {}: {e}", memory_path.display()),
            }
            found_mem = true;
            break;
        }
    }
    if !found_mem {
        println!("No MEMORY.md found in current workspace.");
    }

    if let Some(home) = crate::home::dir() {
        let user_candidates = [
            PathBuf::from(".seavx/USER.md"),
            PathBuf::from("USER.md"),
            home.join(".seavx/USER.md"),
        ];
        for u in &user_candidates {
            if u.exists() {
                println!("\n--- User Preferences ({}) ---", u.display());
                match std::fs::read_to_string(u) {
                    Ok(c) => println!("{c}"),
                    Err(e) => eprintln!("Error reading {}: {e}", u.display()),
                }
                break;
            }
        }
    }

    let skill_mgr = crate::agent::skills::SkillManager::new(&PathBuf::from("."));
    let skills = skill_mgr.list_skills();
    if !skills.is_empty() {
        println!("\n--- Installed Skills (.seavx/skills, ~/.seavx/skills, ~/.agents/skills) ---");
        for s in skills {
            println!("- {}: {}", s.name, s.description);
        }
    }

    Ok(())
}
