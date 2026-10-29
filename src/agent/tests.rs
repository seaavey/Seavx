#[cfg(test)]
mod test_cases {
    use crate::agent::engine::AgentMode;
    use crate::agent::security::WorkspaceSecurity;
    use crate::agent::tools::ToolRegistry;
    use serde_json::json;

    #[test]
    fn test_workspace_security_path_traversal() {
        let sec = WorkspaceSecurity::new(".");
        assert!(sec.validate_path("Cargo.toml").is_ok());
        assert!(sec.validate_path("../../../etc/passwd").is_err());
        assert!(sec.validate_path(".env").is_err());
    }

    #[tokio::test]
    async fn test_tool_registry_and_schema() {
        let registry = ToolRegistry::with_default_tools();
        assert!(registry.get("read_file").is_some());
        assert!(registry.get("write_file").is_some());
        assert!(registry.get("edit_file").is_some());
        assert!(registry.get("list_directory").is_some());
        assert!(registry.get("run_command").is_some());
        assert!(registry.get("search_files").is_some());
        assert!(registry.get("save_memory").is_some());
        assert!(registry.get("skill_view").is_some());
        assert!(registry.get("skill_write").is_some());
        assert!(registry.get("skill_patch").is_some());

        let sec = WorkspaceSecurity::new(".");
        let res = registry
            .get("read_file")
            .unwrap()
            .execute(
                json!({ "path": "Cargo.toml", "offset": 1, "limit": 5 }),
                &sec,
            )
            .await;
        assert!(res.is_ok());
        assert!(res.unwrap().contains("name = \"seavx\""));

        let search_res = registry
            .get("search_files")
            .unwrap()
            .execute(json!({ "pattern": "name = \"seavx\"" }), &sec)
            .await;
        assert!(search_res.is_ok());
        assert!(search_res.unwrap().contains("Cargo.toml"));

        let mem_res = registry
            .get("save_memory")
            .unwrap()
            .execute(json!({ "lesson": "secret_api_key_123" }), &sec)
            .await;
        assert!(mem_res.is_err());

        // Test singularity threshold rejection (< 70%)
        let low_conf_res = registry
            .get("save_memory")
            .unwrap()
            .execute(
                json!({ "lesson": "Low confidence guess", "confidence": 50 }),
                &sec,
            )
            .await;
        assert!(low_conf_res.is_err());

        // Test singularity acceptance (>= 70%)
        let high_conf_res = registry
            .get("save_memory")
            .unwrap()
            .execute(
                json!({ "lesson": "High confidence verified rule", "confidence": 95 }),
                &sec,
            )
            .await;
        assert!(high_conf_res.is_ok());
        let _ = std::fs::remove_file("MEMORY.md");
    }

    #[test]
    fn test_normalize_base_url() {
        use crate::agent::engine::normalize_base_url;
        use genai::adapter::AdapterKind;

        // Bare domain with trailing slash
        let (url, adapters) = normalize_base_url("anthropic.com/");
        assert_eq!(url, "https://anthropic.com/v1/");
        assert!(adapters.contains(&AdapterKind::Anthropic));
        assert!(adapters.contains(&AdapterKind::OpenAI));

        // Domain with http and no /v1
        let (url, _) = normalize_base_url("http://custom-proxy.internal:8080");
        assert_eq!(url, "http://custom-proxy.internal:8080/v1/");

        // Domain already having /v1
        let (url, adapters) = normalize_base_url("https://api.openai.com/v1");
        assert_eq!(url, "https://api.openai.com/v1/");
        assert!(adapters.contains(&AdapterKind::OpenAI));
    }

    #[test]
    fn test_scan_workspace_entries() {
        let entries = crate::tui::app::scan_workspace_entries("@Cargo");
        assert!(!entries.is_empty());
        assert!(entries.iter().any(|(_, p)| p == "Cargo.toml"));
    }

    #[test]
    fn test_tui_scroll_calculation() {
        let visible_height = 20u16;
        let total_lines = 100u16;
        let max_scroll = total_lines.saturating_sub(visible_height);
        assert_eq!(max_scroll, 80);

        let auto_offset = max_scroll;
        assert_eq!(auto_offset, 80);

        let mut scroll = max_scroll;
        scroll = scroll.saturating_sub(10);
        let user_offset = scroll.min(max_scroll);
        assert_eq!(user_offset, 70);

        scroll = scroll.saturating_sub(200);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn test_agent_config_load() {
        let temp_dir = std::env::temp_dir().join("seavx_test_config");
        let seavx_dir = temp_dir.join(".seavx");
        let _ = std::fs::create_dir_all(&seavx_dir);

        let toml_content =
            "model = \"test-model-xyz\"\nmax_turns = 42\napi_key = \"secret-test-key\"\n";
        let _ = std::fs::write(seavx_dir.join("config.toml"), toml_content);

        let config = crate::agent::engine::AgentConfig::load(&temp_dir);
        assert_eq!(config.model, "test-model-xyz");
        assert_eq!(config.max_turns, 42);
        assert_eq!(config.api_key.as_deref(), Some("secret-test-key"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_parse_skill_confidence() {
        use crate::agent::engine::{SKILL_CONFIDENCE_THRESHOLD, parse_confidence};

        let pass = "---\nname: rust-release\nconfidence: 85%\n---\n";
        let quoted = "---\nname: rust-release\nconfidence: \"90%\"\n---\n";
        let fail = "---\nname: one-off\nconfidence: 60%\n---\n";
        let missing = "---\nname: one-off\n---\n";

        assert_eq!(parse_confidence(pass), Some(85));
        assert_eq!(parse_confidence(quoted), Some(90));
        assert_eq!(parse_confidence(fail), Some(60));
        assert_eq!(parse_confidence(missing), None);

        // The gate accepts only at/above the threshold.
        for (md, expected) in [
            (pass, true),
            (quoted, true),
            (fail, false),
            (missing, false),
        ] {
            let accepted =
                matches!(parse_confidence(md), Some(c) if c >= SKILL_CONFIDENCE_THRESHOLD);
            assert_eq!(accepted, expected, "{md}");
        }
    }

    #[test]
    fn test_skill_manager_workflow() {
        let mgr = crate::agent::skills::SkillManager::new(std::path::Path::new("."));
        let test_content = "---\nname: test-skill\ndescription: A test skill\n---\n# Test Content";
        let save_res = mgr.save_skill("test-skill", test_content);
        assert!(save_res.is_ok());

        let skills = mgr.list_skills();
        assert!(skills.iter().any(|s| s.name == "test-skill"));

        // Category subdirectories (shared ~/.agents/skills layout) are scanned too.
        let nested = std::path::Path::new(".seavx/skills/category/nested-skill");
        let _ = std::fs::create_dir_all(nested);
        let _ = std::fs::write(nested.join("SKILL.md"), test_content);
        let skills = mgr.list_skills();
        assert!(skills.iter().any(|s| s.name == "nested-skill"));

        let _ = std::fs::remove_dir_all(".seavx/skills/test-skill");
        let _ = std::fs::remove_dir_all(".seavx/skills/category");
    }

    #[tokio::test]
    async fn test_skill_tools_create_patch_view() {
        let root = std::env::temp_dir().join(format!("seavx_skill_tools_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let sec = WorkspaceSecurity::new(&root);
        let registry = ToolRegistry::with_default_tools();
        let name = "seavx-skill-tools-roundtrip";

        let created = registry
            .get("skill_write")
            .unwrap()
            .execute(
                json!({
                    "name": name,
                    "content": "---\nname: seavx-skill-tools-roundtrip\ndescription: \"Roundtrip test skill.\"\n---\n\nStep A\n"
                }),
                &sec,
            )
            .await;
        assert!(created.unwrap().contains("Successfully created"));

        let patched = registry
            .get("skill_patch")
            .unwrap()
            .execute(
                json!({ "name": name, "old_string": "Step A", "new_string": "Step B" }),
                &sec,
            )
            .await;
        assert!(patched.unwrap().contains("Successfully patched"));

        let viewed = registry
            .get("skill_view")
            .unwrap()
            .execute(json!({ "name": name }), &sec)
            .await
            .unwrap();
        assert!(viewed.contains("Step B"));

        let missing = registry
            .get("skill_view")
            .unwrap()
            .execute(json!({ "name": "no-such-skill-xyz" }), &sec)
            .await;
        assert!(missing.is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    fn tool_names(mode: AgentMode) -> Vec<String> {
        let registry = ToolRegistry::with_default_tools();
        let mut names: Vec<String> = registry
            .to_genai_tools(mode)
            .iter()
            .map(|tool| tool.name.as_str().to_string())
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn plan_mode_exports_only_read_only_tools() {
        assert_eq!(
            tool_names(AgentMode::Plan),
            vec![
                "list_directory".to_string(),
                "read_file".to_string(),
                "search_files".to_string(),
                "skill_view".to_string()
            ]
        );
    }

    #[test]
    fn build_mode_exports_every_tool() {
        let names = tool_names(AgentMode::Build);
        assert_eq!(names.len(), 10, "{names:?}");
        for expected in [
            "read_file",
            "write_file",
            "edit_file",
            "list_directory",
            "run_command",
            "search_files",
            "save_memory",
            "skill_view",
            "skill_write",
            "skill_patch",
        ] {
            assert!(
                names.iter().any(|name| name.as_str() == expected),
                "missing {expected} in {names:?}"
            );
        }
    }

    #[test]
    fn plan_mode_rejects_mutation_tools_at_execution_time() {
        let registry = ToolRegistry::with_default_tools();
        for name in [
            "write_file",
            "edit_file",
            "save_memory",
            "skill_write",
            "skill_patch",
            "run_command",
        ] {
            let err = registry
                .get_for_mode(name, AgentMode::Plan)
                .err()
                .unwrap_or_else(|| panic!("{name} should be blocked"));
            assert!(err.contains("disabled in Plan Mode"), "{err}");
        }

        assert!(registry.get_for_mode("read_file", AgentMode::Plan).is_ok());
        assert!(
            registry
                .get_for_mode("run_command", AgentMode::Build)
                .is_ok()
        );
    }

    #[test]
    fn auto_distill_is_disabled_in_plan_mode() {
        use crate::agent::engine::should_auto_distill;
        assert!(!should_auto_distill(AgentMode::Plan, 5));
        assert!(!should_auto_distill(AgentMode::Build, 1));
        assert!(should_auto_distill(AgentMode::Build, 2));
    }
}
