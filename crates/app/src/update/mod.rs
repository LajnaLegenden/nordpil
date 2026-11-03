//! Updates from GitHub releases: checks at startup and every few hours, and
//! depending on the `updates` setting installs a newer release by itself or
//! says one is available. Network and file work runs on its own thread; the
//! UI only polls [`Updater`] for its state.

mod install;
mod release;

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use nordpil_backend::settings::Updates;
use semver::Version;

pub use install::Install;
pub use release::Release;

const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// After a failed check, e.g. while offline.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    Checking,
    Available(Release),
    Installing(Release),
    /// Installed; runs after a restart.
    Installed(Release),
    Failed(Release, String),
}

enum Event {
    Checked(Result<Option<Release>, String>),
    Installed(Release, Result<(), String>),
}

pub struct Updater {
    install: Install,
    current: Version,
    state: State,
    next_check: Instant,
    /// The banner stays hidden for this version.
    dismissed: Option<Version>,
    ctx: egui::Context,
    sender: Sender<Event>,
    receiver: Receiver<Event>,
}

impl Updater {
    pub fn new(ctx: egui::Context) -> Self {
        let install = Install::detect();
        tracing::info!(?install, "updates enabled");
        let (sender, receiver) = crossbeam_channel::unbounded();
        Self {
            install,
            current: Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
            state: State::Idle,
            next_check: Instant::now(),
            dismissed: None,
            ctx,
            sender,
            receiver,
        }
    }

    /// Applies finished work and starts a check when one is due. Call once
    /// per frame.
    pub fn poll(&mut self, mode: Updates) {
        for event in self.receiver.try_iter().collect::<Vec<_>>() {
            self.handle(event, mode);
        }
        if mode == Updates::Off {
            return;
        }
        let now = Instant::now();
        if now < self.next_check {
            self.ctx.request_repaint_after(self.next_check - now);
            return;
        }
        if matches!(self.state, State::Idle | State::Available(_)) {
            self.check();
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn can_install(&self) -> bool {
        self.install.can_update()
    }

    /// Whether the banner should show now.
    pub fn visible(&self) -> bool {
        let version = match &self.state {
            State::Idle | State::Checking => return false,
            State::Available(release)
            | State::Installing(release)
            | State::Installed(release)
            | State::Failed(release, _) => &release.version,
        };
        self.dismissed.as_ref() != Some(version)
    }

    pub fn dismiss(&mut self) {
        if let State::Available(release) | State::Installed(release) | State::Failed(release, _) =
            &self.state
        {
            self.dismissed = Some(release.version.clone());
        }
    }

    /// Downloads and installs the available release.
    pub fn install(&mut self) {
        let release = match &self.state {
            State::Available(release) | State::Failed(release, _) => release.clone(),
            _ => return,
        };
        let Some(download) = release
            .download
            .clone()
            .filter(|_| self.install.can_update())
        else {
            return;
        };
        tracing::info!(version = %release.version, file = download.name, "installing update");
        self.state = State::Installing(release.clone());
        let install = self.install.clone();
        let sender = self.sender.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = install.update(&release::agent(), &download);
            let _ = sender.send(Event::Installed(release, result));
            ctx.request_repaint();
        });
    }

    /// Starts the new version and closes this window.
    pub fn restart(&mut self) {
        match self.install.relaunch() {
            Ok(()) => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(error) => {
                if let State::Installed(release) = &self.state {
                    self.state = State::Failed(release.clone(), error);
                }
            }
        }
    }

    fn check(&mut self) {
        self.state = State::Checking;
        self.next_check = Instant::now() + CHECK_EVERY;
        let current = self.current.clone();
        let install = self.install.clone();
        let sender = self.sender.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = release::check(&release::agent(), &current, |version| {
                install.asset(version)
            });
            let _ = sender.send(Event::Checked(result));
            ctx.request_repaint();
        });
    }

    fn handle(&mut self, event: Event, mode: Updates) {
        match event {
            Event::Checked(Ok(None)) => self.state = State::Idle,
            Event::Checked(Ok(Some(release))) => {
                tracing::info!(version = %release.version, "update available");
                self.state = State::Available(release);
                if mode == Updates::Automatic {
                    self.install();
                }
            }
            Event::Checked(Err(error)) => {
                tracing::warn!(%error, "update check failed");
                self.state = State::Idle;
                self.next_check = Instant::now() + RETRY_AFTER;
            }
            Event::Installed(release, Ok(())) => {
                tracing::info!(version = %release.version, "update installed");
                self.state = State::Installed(release);
            }
            Event::Installed(release, Err(error)) => {
                tracing::warn!(%error, version = %release.version, "update failed");
                self.state = State::Failed(release, error);
            }
        }
    }
}
