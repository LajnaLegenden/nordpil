use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use nordpil_backend::Password;
use nordpil_backend::settings::LockTimeout;

use super::*;
use crate::lock::{Authenticator, Method, Outcome};

struct FakeAuth {
    method: Method,
    outcomes: Mutex<VecDeque<Outcome>>,
    calls: AtomicUsize,
}

impl Authenticator for FakeAuth {
    fn method(&self) -> Method {
        self.method
    }

    fn authenticate(&self, _password: Option<&Password>) -> Outcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Outcome::Cancelled)
    }
}

fn fake(method: Method, outcomes: impl IntoIterator<Item = Outcome>) -> Arc<FakeAuth> {
    Arc::new(FakeAuth {
        method,
        outcomes: Mutex::new(outcomes.into_iter().collect()),
        calls: AtomicUsize::new(0),
    })
}

fn enable(harness: &mut Harness<'_, NordpilApp>, timeout: LockTimeout, auth: &Arc<FakeAuth>) {
    harness.state_mut().settings.lock.timeout = timeout;
    harness.state_mut().enable_lock(auth.clone());
}

fn rewind(harness: &mut Harness<'_, NordpilApp>, secs: u64) {
    harness.state_mut().lock.last_activity -= Duration::from_secs(secs);
}

/// Steps instead of running: a lock screen may keep repainting.
fn wait_for(harness: &mut Harness<'_, NordpilApp>, done: impl Fn(&NordpilApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(harness.state()) {
        assert!(
            Instant::now() < deadline,
            "timed out: {}",
            describe(harness.state())
        );
        std::thread::sleep(Duration::from_millis(20));
        harness.step();
    }
    harness.step();
}

fn has_label(harness: &Harness<'_, NordpilApp>, text: &str) -> bool {
    harness.query_all_by_label_contains(text).next().is_some()
}

fn wait_for_label(harness: &mut Harness<'_, NordpilApp>, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !has_label(harness, text) {
        assert!(Instant::now() < deadline, "no label {text:?}");
        std::thread::sleep(Duration::from_millis(20));
        harness.step();
    }
}

fn is_locked(app: &NordpilApp) -> bool {
    app.lock.is_locked()
}

fn press_lock(harness: &mut Harness<'_, NordpilApp>) {
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::L);
    harness.step();
    harness.step();
}

fn type_password(harness: &mut Harness<'_, NordpilApp>, text: &str) {
    harness
        .get_by_role_and_label(Role::PasswordInput, "Password")
        .focus();
    harness.step();
    harness
        .get_by_role_and_label(Role::PasswordInput, "Password")
        .type_text(text);
    harness.step();
    harness.step();
}

fn click_unlock(harness: &mut Harness<'_, NordpilApp>) {
    harness
        .get_by_role_and_label(Role::Button, "Unlock")
        .click();
    harness.step();
}

fn lock_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("nordpil-lock-{}", Uuid::new_v4()))
}

fn owner_harness() -> Harness<'static, NordpilApp> {
    lookup_harness(&["orders"], bson::doc! { "_id": 1, "ownerRef": "a-1" })
}

/// `lookup_harness` on its own config dir: a connected "Local" and one tab.
fn tab_harness_in(dir: &Path) -> Harness<'static, NordpilApp> {
    let mut harness = harness_in(dir);
    run_until(&mut harness, |app| app.keyring_available);
    let app = harness.state_mut();
    let mut entry = ConnectionEntry::new(SavedConnection {
        id: Uuid::new_v4(),
        name: "Local".to_owned(),
        uri: "mongodb://localhost:27017".to_owned(),
        color: None,
        read_only: false,
    });
    entry.state = ConnectionState::Connected(Vec::new());
    let target = Selection {
        connection: entry.saved.id,
        database: "shop".to_owned(),
        collection: "orders".to_owned(),
    };
    app.connections = vec![entry];
    let mut tab = CollectionTab::new(1, target);
    tab.inputs.filter = "{ status: 'paid' }".to_owned();
    tab.set_documents(vec![bson::doc! { "_id": 1, "ownerRef": "a-1" }]);
    app.tabs.push(tab);
    app.active_tab = Some(0);
    app.next_tab_id = 1;
    harness.run();
    harness
}

fn config_has(dir: &Path, text: &str) -> bool {
    std::fs::read_to_string(dir.join("config.toml")).is_ok_and(|config| config.contains(text))
}

fn wait_for_config(dir: &Path, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !config_has(dir, text) {
        assert!(Instant::now() < deadline, "config.toml never had {text:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn idle_lock_fires_after_the_timeout_without_input() {
    let mut harness = owner_harness();
    let auth = fake(Method::Prompt, []);
    enable(&mut harness, LockTimeout::H1, &auth);
    harness.step();
    assert!(!harness.state().lock.is_locked());

    rewind(&mut harness, 3601);
    wait_for(&mut harness, is_locked);

    assert!(has_label(&harness, "nordpil is locked"));
    assert!(has_label(&harness, "Locked after 1 h idle at"));
}

#[test]
fn input_restarts_the_idle_clock() {
    let mut harness = owner_harness();
    let auth = fake(Method::Prompt, []);
    enable(&mut harness, LockTimeout::H1, &auth);
    harness.step();

    rewind(&mut harness, 3601);
    harness.key_press(Key::A);
    harness.step();
    harness.step();

    assert!(!harness.state().lock.is_locked());
}

#[test]
fn off_never_locks() {
    let mut harness = owner_harness();
    let auth = fake(Method::Prompt, []);
    enable(&mut harness, LockTimeout::Off, &auth);
    harness.step();

    rewind(&mut harness, 9 * 3600);
    harness.step();
    harness.step();
    assert!(!harness.state().lock.is_locked());

    press_lock(&mut harness);
    let ctx = harness.ctx.clone();
    let filter = harness.state().tabs[0].editors.filter.id();
    assert!(!harness.state().lock.is_locked());
    assert!(!ctx.memory(|memory| memory.has_focus(filter)));
}

#[cfg(unix)]
const SLOW_AGENT_ANSWER: &str = r#"echo '{"kind":"find","filter":"{ status: \"paid\" }","projection":"","sort":"","limit":"","pipeline":"","explanation":"Paid."}'"#;

/// An Ask AI request whose agent waits for `marker` to exist.
#[cfg(unix)]
fn waiting_agent_harness(marker: &Path) -> Harness<'static, NordpilApp> {
    let mut harness = ai_harness(&format!(
        "cat > /dev/null\nwhile [ ! -e '{}' ]; do sleep 0.05; done\n{SLOW_AGENT_ANSWER}",
        marker.display()
    ));
    harness
        .get_by_role_and_label(Role::Button, "Ask AI")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Request")
        .type_text("paid ones");
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Generate")
        .click();
    // The request is sent on the frame after Generate.
    wait_for(&mut harness, |app| app.backend.is_busy());
    harness
}

#[test]
#[cfg(unix)]
fn running_work_delays_the_idle_lock_until_it_ends() {
    let marker = std::env::temp_dir().join(format!("nordpil-go-{}", Uuid::new_v4()));
    let mut harness = waiting_agent_harness(&marker);
    let auth = fake(Method::Prompt, []);
    enable(&mut harness, LockTimeout::H1, &auth);

    rewind(&mut harness, 3601);
    harness.step();
    harness.step();
    assert!(!harness.state().lock.is_locked());

    std::fs::write(&marker, "").unwrap();
    wait_for(&mut harness, |app| !app.backend.is_busy());
    harness.step();
    assert!(harness.state().lock.is_locked());
    let _ = std::fs::remove_file(marker);
}

#[test]
#[cfg(unix)]
fn lock_now_waits_for_running_work_and_says_so() {
    let marker = std::env::temp_dir().join(format!("nordpil-go-{}", Uuid::new_v4()));
    let mut harness = waiting_agent_harness(&marker);
    let auth = fake(Method::Prompt, []);
    enable(&mut harness, LockTimeout::H1, &auth);

    press_lock(&mut harness);
    let app = harness.state();
    assert!(!app.lock.is_locked());
    assert!(app.notifications.iter().any(|note| {
        note.message
            .contains("locks when the running work finishes")
    }));

    std::fs::write(&marker, "").unwrap();
    wait_for(&mut harness, is_locked);
    assert!(has_label(&harness, "Locked manually"));
    let _ = std::fs::remove_file(marker);
}

#[test]
fn lock_closes_dialogs_and_clears_loaded_data() {
    use crate::aggregate::Preview as StagePreview;
    use crate::shell::{Body, ShellResult};

    let mut harness = owner_harness();
    let auth = fake(Method::Prompt, []);
    enable(&mut harness, LockTimeout::H1, &auth);
    {
        let app = harness.state_mut();
        let tab = &mut app.tabs[0];
        tab.inputs.filter = "{ status: 'paid' }".to_owned();
        tab.shell.loaded = true;
        tab.shell.text = "db.orders.find({})".to_owned();
        tab.shell.script = "db.orders.find({})".to_owned();
        tab.shell.results.push(ShellResult {
            title: "db.orders.find({})".to_owned(),
            elapsed: Duration::from_millis(3),
            body: Body::documents(vec![bson::doc! { "_id": 1 }], 20),
        });
        tab.aggregate.loaded = true;
        tab.aggregate
            .paste("[{ $match: { status: 'paid' } }]", &mut app.backend)
            .unwrap();
    }
    harness.step();
    wait_for(&mut harness, |app| !app.backend.is_busy());
    harness.state_mut().tabs[0].aggregate.stages[0].preview = StagePreview::TimedOut;

    let connection = harness.state().connections[0].saved.id;
    let app = harness.state_mut();
    app.settings_dialog = Some(SettingsDialog::default());
    app.goto = Some(GotoPalette::new());
    app.shortcuts_sheet = true;
    app.ask_ai = Some(AskAi::new(
        app.tabs[0].id,
        Surface::Find,
        AiAgent::Claude,
        "paid orders".to_owned(),
    ));
    app.apply(Action::NewConnection);
    app.apply(Action::Drop {
        connection,
        database: "shop".to_owned(),
        collection: Some("orders".to_owned()),
    });
    app.tab_action(0, TabAction::Edit(0));
    assert!(app.document_dialog.is_some());

    press_lock(&mut harness);

    let app = harness.state();
    assert!(app.lock.is_locked());
    assert!(app.settings_dialog.is_none());
    assert!(app.goto.is_none());
    assert!(!app.shortcuts_sheet);
    assert!(app.ask_ai.is_none());
    assert!(app.form.is_none());
    assert!(app.dialog.is_none());
    assert!(app.document_dialog.is_none());
    assert!(matches!(
        app.connections[0].state,
        ConnectionState::Disconnected
    ));
    let tab = &app.tabs[0];
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(tab.inputs.filter, "{ status: 'paid' }");
    assert!(tab.results.is_none());
    assert!(tab.shell.results.is_empty());
    assert_eq!(tab.shell.text, "db.orders.find({})");
    assert_eq!(tab.aggregate.stages[0].name, "$match");
    assert!(matches!(
        tab.aggregate.stages[0].preview,
        StagePreview::None
    ));
}

#[test]
fn nothing_of_the_app_shows_or_reacts_while_locked() {
    let mut harness = owner_harness();
    let auth = fake(Method::Prompt, []);
    enable(&mut harness, LockTimeout::H1, &auth);
    harness.step();
    assert!(has_label(&harness, "Local"));

    rewind(&mut harness, 3601);
    wait_for(&mut harness, is_locked);
    assert!(!has_label(&harness, "Local"));
    assert!(!has_label(&harness, "ownerRef:"));
    assert!(harness.query_by_label("New connection").is_none());
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME"));
    if let Ok(user) = user {
        assert!(has_label(&harness, &user));
    }

    harness.key_press_modifiers(Modifiers::COMMAND, Key::W);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::K);
    harness.key_press(Key::F1);
    harness.step();
    harness.step();
    let app = harness.state();
    assert_eq!(app.tabs.len(), 1);
    assert!(app.goto.is_none());
    assert!(!app.shortcuts_sheet);
    assert!(app.lock.is_locked());
}

#[test]
fn a_wrong_password_keeps_it_locked_and_the_right_one_unlocks() {
    let dir = lock_dir();
    let mut harness = tab_harness_in(&dir);
    let auth = fake(
        Method::Password,
        [
            Outcome::Denied("Wrong password".to_owned()),
            Outcome::Granted,
        ],
    );
    enable(&mut harness, LockTimeout::H1, &auth);
    harness.step();
    rewind(&mut harness, 3601);
    wait_for(&mut harness, is_locked);
    wait_for_config(&dir, "locked = true");

    type_password(&mut harness, "nope");
    click_unlock(&mut harness);
    wait_for_label(&mut harness, "Wrong password");
    assert!(harness.state().lock.is_locked());
    assert_eq!(auth.calls.load(Ordering::SeqCst), 1);

    type_password(&mut harness, "right");
    click_unlock(&mut harness);
    wait_for(&mut harness, |app| !app.lock.is_locked());
    let app = harness.state();
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].inputs.filter, "{ status: 'paid' }");
    assert!(app.lock.last_activity.elapsed().unwrap() < Duration::from_secs(60));
    wait_for_config(&dir, "locked = false");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_prompt_opens_when_the_window_gains_focus_not_when_it_locks() {
    let mut harness = owner_harness();
    let auth = fake(Method::Prompt, [Outcome::Cancelled, Outcome::Granted]);
    enable(&mut harness, LockTimeout::H1, &auth);
    harness.input_mut().focused = false;
    harness.step();

    rewind(&mut harness, 3601);
    wait_for(&mut harness, is_locked);
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(20));
        harness.step();
    }
    assert_eq!(auth.calls.load(Ordering::SeqCst), 0);

    harness.input_mut().focused = true;
    wait_for(&mut harness, |_| auth.calls.load(Ordering::SeqCst) == 1);
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(20));
        harness.step();
    }
    assert!(harness.state().lock.is_locked());
    assert_eq!(auth.calls.load(Ordering::SeqCst), 1);

    click_unlock(&mut harness);
    wait_for(&mut harness, |app| !app.lock.is_locked());
    assert_eq!(auth.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn starts_locked_when_the_last_exit_was_locked() {
    let locked_at_launch = |timeout: &str| {
        let dir = lock_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!("version = 1\n\n[lock]\ntimeout = \"{timeout}\"\nlocked = true\n"),
        )
        .unwrap();
        let mut harness = harness_in(&dir);
        harness.state_mut().enable_lock(fake(Method::Prompt, []));
        harness.step();
        harness.step();
        let locked = harness.state().lock.is_locked();
        let reason = has_label(&harness, "Locked at launch");
        let _ = std::fs::remove_dir_all(dir);
        (locked, reason)
    };

    assert_eq!(locked_at_launch("1h"), (true, true));
    assert_eq!(locked_at_launch("off"), (false, false));
}

#[test]
fn the_off_escape_clears_the_stale_locked_flag() {
    let dir = lock_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "version = 1\n\n[lock]\ntimeout = \"off\"\nlocked = true\n",
    )
    .unwrap();
    let mut harness = harness_in(&dir);
    harness.state_mut().enable_lock(fake(Method::Prompt, []));
    harness.step();

    assert!(!harness.state().lock.is_locked());
    assert!(!harness.state().settings.lock.locked);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_os_auth_error_names_the_config_key() {
    let dir = lock_dir();
    let mut harness = tab_harness_in(&dir);
    let auth = fake(
        Method::Password,
        [Outcome::Failed("PAM is unavailable".to_owned())],
    );
    enable(&mut harness, LockTimeout::H1, &auth);
    harness.step();
    rewind(&mut harness, 3601);
    wait_for(&mut harness, is_locked);

    type_password(&mut harness, "secret");
    click_unlock(&mut harness);
    wait_for_label(&mut harness, "PAM is unavailable");

    assert!(harness.state().lock.is_locked());
    assert!(has_label(&harness, "[lock]"));
    assert!(has_label(&harness, "timeout = \"off\""));
    assert!(has_label(
        &harness,
        &dir.join("config.toml").display().to_string()
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "needs Docker"]
fn a_pending_write_confirmation_does_not_block_the_idle_lock() {
    let mongo = Mongo::default().with_tag("8.0").start().unwrap();
    let port = mongo.get_host_port_ipv4(27017).unwrap();
    let mut harness = connected_shell_harness(port);
    let auth = fake(Method::Prompt, []);
    enable(&mut harness, LockTimeout::H1, &auth);

    harness
        .state_mut()
        .run_script(0, "db.items.insertOne({ a: 1 })".to_owned());
    wait_for(&mut harness, |app| {
        app.tabs[0]
            .shell
            .running
            .as_ref()
            .is_some_and(|running| running.run.pending_write().is_some())
    });

    rewind(&mut harness, 3601);
    wait_for(&mut harness, is_locked);
    wait_for(&mut harness, |app| app.tabs[0].shell.running.is_none());

    assert_eq!(
        mongosh(&mongo, "db.getSiblingDB('test').items.countDocuments({})"),
        "0"
    );
}
