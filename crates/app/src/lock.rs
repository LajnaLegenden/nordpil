//! The idle lock: after the `[lock]` timeout without input, or on
//! Ctrl+Shift+L, nordpil shows only a lock screen until the OS user proves
//! it is them again.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui::{self, Event};
use nordpil_backend::Password;
use nordpil_backend::settings::{LockSettings, LockTimeout};

/// How an [`Authenticator`] checks the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The OS asks in its own dialog, e.g. Touch ID or polkit.
    Prompt,
    /// The lock screen asks for the account password.
    Password,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "made by the OS authenticators, not wired yet")
)]
pub enum Outcome {
    Granted,
    /// A wrong password; the text says so.
    Denied(String),
    Cancelled,
    /// The check itself didn't work, e.g. PAM is missing.
    Failed(String),
}

/// What an auth attempt is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Unlock,
    /// Turns the lock on with this timeout; it was off.
    Enable(LockTimeout),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Idle(LockTimeout),
    Manual,
    /// It was locked when nordpil last closed.
    Launch,
}

/// Checks that the OS user is at the keyboard.
pub trait Authenticator: Send + Sync + 'static {
    fn method(&self) -> Method;
    /// Blocks until the user answers, so it runs off the UI thread.
    fn authenticate(&self, password: Option<&Password>) -> Outcome;
}

pub struct Lock {
    /// Wall clock, so time asleep counts as idle.
    pub last_activity: SystemTime,
    /// egui's time of the input last looked at.
    input_time: f64,
    /// Named when the OS check fails, so the lock can be turned off there.
    config_path: PathBuf,
    /// Without one, nordpil never locks.
    authenticator: Option<Arc<dyn Authenticator>>,
    /// Ctrl+Shift+L came while work ran.
    manual_pending: bool,
    /// The window had focus last frame.
    focused: bool,
    /// Unlocks run only while locked and enables only while the lock is
    /// off, so one at a time is enough.
    attempt: Option<Attempt>,
    locked: Option<Locked>,
}

struct Attempt {
    purpose: Purpose,
    answer: Receiver<Outcome>,
}

/// What the lock screen shows.
pub struct Locked {
    pub reason: Reason,
    pub at: SystemTime,
    pub method: Method,
    /// The OS account that unlocks it, when the OS says.
    pub user: Option<String>,
    pub password: String,
    /// Why the last attempt didn't unlock.
    pub error: Option<String>,
    /// How to get past a check that keeps failing.
    pub hint: Option<String>,
    /// The next focus gain opens the OS prompt.
    armed: bool,
}

impl Lock {
    pub fn new(config_path: PathBuf) -> Self {
        Self {
            last_activity: SystemTime::now(),
            input_time: f64::NEG_INFINITY,
            config_path,
            authenticator: None,
            manual_pending: false,
            // So the first frame of a launch lock counts as gaining focus.
            focused: false,
            attempt: None,
            locked: None,
        }
    }

    /// Turns the lock on, locked at once if it was at the last exit.
    pub fn enable(&mut self, authenticator: Arc<dyn Authenticator>, settings: LockSettings) {
        self.authenticator = Some(authenticator);
        if settings.locked && settings.timeout != LockTimeout::Off {
            self.lock(Reason::Launch);
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked.is_some()
    }

    /// `None` when nordpil can't lock.
    pub fn method(&self) -> Option<Method> {
        self.authenticator
            .as_ref()
            .map(|authenticator| authenticator.method())
    }

    pub fn attempting(&self) -> bool {
        self.attempt.is_some()
    }

    pub fn can_lock(&self, timeout: LockTimeout) -> bool {
        self.authenticator.is_some() && timeout != LockTimeout::Off
    }

    /// While unlocked: notes new input, and says why to lock now without
    /// regard to running work. Otherwise wakes the app when the idle time
    /// runs out.
    pub fn due(&mut self, ctx: &egui::Context, timeout: LockTimeout) -> Option<Reason> {
        if self.is_locked() {
            return None;
        }
        // While the window is hidden this runs without a pass, on the input of
        // the last shown frame; only its focus is current.
        let (time, active, focused) = ctx.input(|input| {
            let active = input.events.iter().any(is_activity);
            (input.time, active, input.raw.focused)
        });
        let now = SystemTime::now();
        self.focused = focused;
        if active && time > self.input_time {
            self.last_activity = now;
        }
        self.input_time = time;
        let limit = timeout.duration().filter(|_| self.authenticator.is_some());
        let Some(limit) = limit else {
            self.manual_pending = false;
            return None;
        };
        if self.manual_pending {
            return Some(Reason::Manual);
        }
        let idle = now.duration_since(self.last_activity).unwrap_or_default();
        if idle >= limit {
            return Some(Reason::Idle(timeout));
        }
        ctx.request_repaint_after(limit - idle);
        None
    }

    /// Locks on the first frame without running work. False when it
    /// already waits for that.
    pub fn lock_after_work(&mut self) -> bool {
        !std::mem::replace(&mut self.manual_pending, true)
    }

    pub fn lock(&mut self, reason: Reason) {
        let Some(authenticator) = &self.authenticator else {
            return;
        };
        self.manual_pending = false;
        self.locked = Some(Locked {
            reason,
            at: SystemTime::now(),
            method: authenticator.method(),
            user: whoami::username().ok(),
            password: String::new(),
            error: None,
            hint: None,
            armed: true,
        });
    }

    pub fn screen(&mut self) -> Option<&mut Locked> {
        self.locked.as_mut()
    }

    /// While locked: takes the answer of a running attempt, and opens the OS
    /// prompt when the window gains focus. True once unlocked.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let focused = ctx.input(|input| input.focused);
        let was_focused = std::mem::replace(&mut self.focused, focused);
        let Some(locked) = &mut self.locked else {
            return false;
        };
        if let Some((Purpose::Unlock, outcome)) = take_answer(&mut self.attempt) {
            match outcome {
                Outcome::Granted => {
                    self.locked = None;
                    self.last_activity = SystemTime::now();
                    return true;
                }
                Outcome::Denied(message) => locked.error = Some(message),
                Outcome::Cancelled => {}
                Outcome::Failed(message) => {
                    locked.error = Some(message);
                    locked.hint = Some(format!(
                        "To turn the lock off, set timeout = \"off\" under [lock] in {}.",
                        self.config_path.display()
                    ));
                }
            }
        }
        // A prompt that takes focus and then gives it back mustn't open again.
        if !focused && was_focused && self.attempt.is_none() {
            locked.armed = true;
        }
        if focused && !was_focused && locked.armed && locked.method == Method::Prompt {
            self.attempt(ctx);
        }
        false
    }

    /// Asks the authenticator to unlock, unless an attempt runs.
    pub fn attempt(&mut self, ctx: &egui::Context) {
        let Some(locked) = &mut self.locked else {
            return;
        };
        if self.attempt.is_some() {
            return;
        }
        let password = (locked.method == Method::Password)
            .then(|| Password::new(std::mem::take(&mut locked.password)));
        locked.armed = false;
        locked.error = None;
        locked.hint = None;
        self.ask(ctx, Purpose::Unlock, password);
    }

    /// Asks the authenticator before the lock turns on with `timeout`,
    /// unless an attempt runs.
    pub fn attempt_enable(
        &mut self,
        ctx: &egui::Context,
        timeout: LockTimeout,
        password: Option<Password>,
    ) {
        if self.attempt.is_none() {
            self.ask(ctx, Purpose::Enable(timeout), password);
        }
    }

    /// While unlocked: takes the answer of an attempt to turn the lock on,
    /// with the timeout it was for.
    pub fn poll_enable(&mut self) -> Option<(LockTimeout, Outcome)> {
        let Some((Purpose::Enable(timeout), outcome)) = take_answer(&mut self.attempt) else {
            return None;
        };
        if outcome == Outcome::Granted {
            self.last_activity = SystemTime::now();
        }
        Some((timeout, outcome))
    }

    /// Runs the authenticator on its own thread, which wakes the UI when it
    /// answers.
    fn ask(&mut self, ctx: &egui::Context, purpose: Purpose, password: Option<Password>) {
        let Some(authenticator) = &self.authenticator else {
            return;
        };
        let authenticator = Arc::clone(authenticator);
        let (sender, answer) = crossbeam_channel::bounded(1);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(authenticator.authenticate(password.as_ref()));
            ctx.request_repaint();
        });
        self.attempt = Some(Attempt { purpose, answer });
    }
}

/// The answer of the running attempt, once it came, and what it was for.
fn take_answer(attempt: &mut Option<Attempt>) -> Option<(Purpose, Outcome)> {
    let Attempt { purpose, answer } = attempt.as_ref()?;
    let outcome = match answer.try_recv() {
        Ok(outcome) => outcome,
        Err(TryRecvError::Empty) => return None,
        Err(TryRecvError::Disconnected) => Outcome::Failed(match purpose {
            Purpose::Unlock => "The unlock check stopped without an answer.".to_owned(),
            Purpose::Enable(_) => "The check stopped without an answer.".to_owned(),
        }),
    };
    let purpose = *purpose;
    *attempt = None;
    Some((purpose, outcome))
}

/// Input from the user; focus changes don't count.
fn is_activity(event: &Event) -> bool {
    matches!(
        event,
        Event::Key { .. }
            | Event::Text(_)
            | Event::Paste(_)
            | Event::Copy
            | Event::Cut
            | Event::Ime(_)
            | Event::PointerMoved(_)
            | Event::MouseMoved(_)
            | Event::PointerButton { .. }
            | Event::MouseWheel { .. }
            | Event::Touch { .. }
            | Event::Zoom(_)
    )
}
