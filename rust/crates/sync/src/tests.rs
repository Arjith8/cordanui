#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn open_local_database() {
        let dir = std::env::temp_dir().join("cordanui-sync-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir);

        let db_path = dir.join("test.db");
        let config = SyncConfig {
            db_path,
            ..Default::default()
        };
        let db = Database::open(&config).unwrap();
        assert!(!db.is_sync_enabled());
    }

    #[test]
    fn clone_shares_handles() {
        let dir = std::env::temp_dir().join("cordanui-sync-clone");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir);

        let config = SyncConfig {
            db_path: dir.join("test.db"),
            ..Default::default()
        };
        let db = Database::open(&config).unwrap();
        let clone = db.clone();

        // Write via one handle, read through the other.
        clone
            .execute(
                "INSERT INTO goals (id, title, description, status, parent_id, sort_order, created_at, updated_at) \
                 VALUES ('clone-1', 'via clone', NULL, 'pending', NULL, 0, 'x', 'x')",
                vec![],
            )
            .unwrap();
        let found = db
            .query_first(
                "SELECT title FROM goals WHERE id = 'clone-1'",
                vec![],
            )
            .unwrap();
        assert!(found.is_some());
    }

    #[test]
    fn round_trip_goal() {
        let dir = std::env::temp_dir().join("cordanui-sync-test2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir);

        let db_path = dir.join("test.db");
        let config = SyncConfig {
            db_path,
            ..Default::default()
        };
        let db = Database::open(&config).unwrap();

        // Insert a goal
        let id = cordanui_schema::new_id();
        let ts = cordanui_schema::now_iso();
        db.execute(
            "INSERT INTO goals (id, title, description, status, parent_id, sort_order, created_at, updated_at) \
             VALUES (?, ?, NULL, 'pending', NULL, 0, ?, ?)",
            vec![
                Value::from(id.clone()),
                Value::from("Test goal"),
                Value::from(ts.clone()),
                Value::from(ts),
            ],
        )
        .unwrap();

        // Read it back
        let result = db
            .query_first(
                "SELECT title FROM goals WHERE id = ?",
                vec![Value::from(id)],
            )
            .unwrap();
        assert!(result.is_some());
        let row = result.unwrap();
        match &row[0] {
            Value::Text(s) => assert_eq!(s, "Test goal"),
            v => panic!("expected text, got {v:?}"),
        }
    }

    #[test]
    fn config_loads_without_file() {
        // When no config file exists, should return local-only config
        // (we can't easily test the real path, but we can test the logic)
        let config = SyncConfig::default();
        assert!(!config.is_sync_enabled());
    }

    fn temp_db(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cordanui-sync-mig-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("test.db")
    }

    #[test]
    fn fresh_db_records_migrations_without_running_them() {
        let db = Database::open(&SyncConfig {
            db_path: temp_db("fresh"),
            ..Default::default()
        })
        .unwrap();

        // Every migration is recorded as applied…
        let result = db
            .query_simple("SELECT version FROM _migrations ORDER BY version")
            .unwrap();
        let versions: Vec<i64> = result
            .rows()
            .iter()
            .map(|r| match &r[0] {
                Value::Integer(n) => *n,
                v => panic!("expected integer version, got {v:?}"),
            })
            .collect();
        let expected: Vec<i64> = cordanui_schema::MIGRATIONS
            .iter()
            .map(|m| m.version)
            .collect();
        assert_eq!(versions, expected);

        // …and the final schema already reflects them (no `is_dark`).
        assert!(db
            .query_simple("SELECT is_dark FROM themes LIMIT 1")
            .is_err());
    }

    #[test]
    fn turso_credentials_round_trip_preserves_other_sections() {
        let dir = std::env::temp_dir().join(format!("cordanui-sync-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[keybinds]\nleader = \"ctrl+a\"\n\n[turso]\nurl = \"old\"\ntoken = \"oldtok\"\n",
        )
        .unwrap();

        write_turso_credentials_at(&path, "libsql://new.turso.io", "tok2").unwrap();
        let (url, token) = read_turso_credentials_at(&path);
        assert_eq!(url.as_deref(), Some("libsql://new.turso.io"));
        assert_eq!(token.as_deref(), Some("tok2"));

        // [keybinds] survived the rewrite.
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("ctrl+a"),
            "other sections must survive: {contents}"
        );

        // Fresh file creation works too.
        let fresh = dir.join("fresh.toml");
        write_turso_credentials_at(&fresh, "u", "t").unwrap();
        assert_eq!(read_turso_credentials_at(&fresh).0.as_deref(), Some("u"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_settings_mirror_round_trip_preserves_other_sections() {
        let dir = std::env::temp_dir().join(format!("cordanui-sync-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[turso]\nurl = \"libsql://x.turso.io\"\n\n[keybinds]\nleader = \"ctrl+a\"\n",
        )
        .unwrap();

        write_plugin_setting_at(&path, "my-plugin", "api_key", "sk-test").unwrap();
        write_plugin_setting_at(&path, "my-plugin", "model", "glm-5.2").unwrap();
        // Second plugin doesn't clobber the first.
        write_plugin_setting_at(&path, "other", "variant", "moon").unwrap();

        let mine = read_plugin_settings_at(&path, "my-plugin");
        assert_eq!(mine.get("api_key").map(String::as_str), Some("sk-test"));
        assert_eq!(mine.get("model").map(String::as_str), Some("glm-5.2"));
        assert_eq!(
            read_plugin_settings_at(&path, "other")
                .get("variant")
                .map(String::as_str),
            Some("moon")
        );

        // Existing sections survived.
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("libsql://x.turso.io"), "{contents}");
        assert!(contents.contains("ctrl+a"), "{contents}");
    }
}
