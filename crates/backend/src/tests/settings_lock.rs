use uuid::Uuid;

use super::*;

#[test]
fn lock_settings_round_trip() {
    let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
    let path = dir.join("config.toml");
    let settings = Settings {
        lock: LockSettings {
            timeout: LockTimeout::H1,
            locked: true,
        },
        ..Default::default()
    };
    save(&path, settings.clone()).unwrap();

    assert_eq!(load(&path).unwrap(), settings);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[lock]\ntimeout = \"1h\""), "{text}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn missing_lock_table_loads_off_and_unlocked() {
    let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
    let path = dir.join("config.toml");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&path, "version = 1\n").unwrap();

    let settings = load(&path).unwrap();
    assert_eq!(settings.lock.timeout, LockTimeout::Off);
    assert!(!settings.lock.locked);
    let _ = std::fs::remove_dir_all(dir);
}
