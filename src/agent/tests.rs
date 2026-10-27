#[cfg(test)]
mod tests {
    use crate::agent::protocol::Message;
    use crate::agent::security::WorkspaceSecurity;
    use crate::agent::tools::ToolRegistry;
    use serde_json::json;

    #[test]
    fn test_message_protocol_serialization() {
        let msg = Message::user("test prompt");
        let json_val = serde_json::to_value(&msg).expect("Serialize message");
        assert_eq!(json_val["role"], "User");
        assert_eq!(json_val["content"]["Text"], "test prompt");
    }

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
        assert!(registry.get("read_skill").is_some());

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
    fn test_skill_manager_workflow() {
        let mgr = crate::agent::skills::SkillManager::new(std::path::Path::new("."));
        let test_content = "---\nname: test-skill\ndescription: A test skill\n---\n# Test Content";
        let save_res = mgr.save_skill("test-skill", test_content);
        assert!(save_res.is_ok());

        let skills = mgr.list_skills();
        assert!(skills.iter().any(|s| s.name == "test-skill"));

        let _ = std::fs::remove_dir_all(".seavx/skills/test-skill");
    }
}
