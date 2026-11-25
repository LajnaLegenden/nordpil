//! The lock's OS check on Linux: polkit's own prompt when nordpil's policy
//! is installed, else the account password through PAM.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use nonstick::{AuthnFlags, ConversationAdapter, ErrorCode, Transaction, TransactionBuilder};
use nordpil_backend::Password;
use zbus::blocking::Connection;
use zbus::zvariant::Value;

use super::{Authenticator, Method, Outcome};

/// Put there by `just install` and the .deb.
const POLICY_PATH: &str = "/usr/share/polkit-1/actions/io.github.lajnalegenden.nordpil.policy";
const UNLOCK_ACTION: &str = "io.github.lajnalegenden.nordpil.unlock";

pub struct LinuxAuthenticator {
    /// Off when the policy isn't installed, and once polkit couldn't ask:
    /// the lock then asks for the password for the rest of the run.
    polkit: AtomicBool,
}

impl LinuxAuthenticator {
    pub fn new() -> Self {
        Self {
            polkit: AtomicBool::new(Path::new(POLICY_PATH).exists()),
        }
    }
}

impl Authenticator for LinuxAuthenticator {
    fn method(&self) -> Method {
        if self.polkit.load(Ordering::Relaxed) {
            Method::Prompt
        } else {
            Method::Password
        }
    }

    fn authenticate(&self, password: Option<&Password>) -> Outcome {
        if let Some(password) = password {
            return check_password(password);
        }
        match check_authorization() {
            Ok((true, _, _)) => return Outcome::Granted,
            Ok((false, _, details)) if details.contains_key("polkit.dismissed") => {
                return Outcome::Cancelled;
            }
            Ok((false, false, _)) => return Outcome::Denied("Authentication failed.".to_owned()),
            // A challenge means no agent could ask, e.g. none runs in this session.
            Ok((false, true, _)) => tracing::warn!("no polkit agent; asking for the password"),
            Err(error) => tracing::warn!(%error, "polkit failed; asking for the password"),
        }
        self.polkit.store(false, Ordering::Relaxed);
        Outcome::Cancelled
    }
}

/// Asks polkit whether nordpil may unlock, which opens its agent's password
/// dialog. Blocks until the user answers it.
fn check_authorization() -> zbus::Result<(bool, bool, HashMap<String, String>)> {
    let connection = Connection::system()?;
    let name = connection
        .unique_name()
        .ok_or_else(|| zbus::Error::Failure("No name on the system bus.".to_owned()))?;
    // Our bus name rather than our pid: polkit looks up who owns it, and
    // unlike a pid it can't be reused by another process meanwhile.
    let subject = (
        "system-bus-name",
        HashMap::from([("name", Value::from(name.as_str()))]),
    );
    let details: HashMap<&str, &str> = HashMap::new();
    let allow_user_interaction = 1u32;
    let reply = connection.call_method(
        Some("org.freedesktop.PolicyKit1"),
        "/org/freedesktop/PolicyKit1/Authority",
        Some("org.freedesktop.PolicyKit1.Authority"),
        "CheckAuthorization",
        &(subject, UNLOCK_ACTION, details, allow_user_interaction, ""),
    )?;
    reply.body().deserialize()
}

/// Checks the account password through PAM's `login` service.
fn check_password(password: &Password) -> Outcome {
    let user = match whoami::username_os() {
        Ok(user) => user,
        Err(error) => {
            return Outcome::Failed(format!("Couldn't tell whose password to check: {error}."));
        }
    };
    let pam_error = OnceCell::new();
    let conversation = PasswordConversation {
        password: password.expose(),
        error: &pam_error,
    };
    let result = TransactionBuilder::new_with_service("login")
        .username(&user)
        .build(conversation.into_conversation())
        .and_then(|mut transaction| transaction.authenticate(AuthnFlags::empty()));
    let message = match result {
        Ok(()) => return Outcome::Granted,
        Err(ErrorCode::AuthenticationError) => "Wrong password.",
        Err(ErrorCode::MaxTries) => "Too many wrong passwords. Try again later.",
        // No password gets past these, so the lock screen says how to turn it off.
        Err(ErrorCode::UserUnknown) => {
            return Outcome::Failed("PAM doesn't know this account.".to_owned());
        }
        Err(ErrorCode::AccountExpired | ErrorCode::PermissionDenied) => {
            return Outcome::Failed("This account may not sign in now.".to_owned());
        }
        Err(code) => return Outcome::Failed(format!("PAM couldn't check the password: {code}.")),
    };
    // PAM's own words say more, e.g. that too many tries locked the account.
    Outcome::Denied(pam_error.into_inner().unwrap_or_else(|| message.to_owned()))
}

/// Answers PAM's password prompt, and keeps the first error it shows.
struct PasswordConversation<'a> {
    password: &'a str,
    error: &'a OnceCell<String>,
}

impl ConversationAdapter for PasswordConversation<'_> {
    fn prompt(&self, _request: impl AsRef<OsStr>) -> nonstick::Result<OsString> {
        Err(ErrorCode::ConversationError)
    }

    fn masked_prompt(&self, _request: impl AsRef<OsStr>) -> nonstick::Result<OsString> {
        Ok(self.password.into())
    }

    fn error_msg(&self, message: impl AsRef<OsStr>) {
        let _ = self
            .error
            .set(message.as_ref().to_string_lossy().into_owned());
    }

    fn info_msg(&self, _message: impl AsRef<OsStr>) {}
}
