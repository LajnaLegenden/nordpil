//! The lock's OS check on macOS: Touch ID, or the account password in the
//! system's own dialog.

use std::sync::mpsc;

use nordpil_backend::Password;
use robius_authentication::{
    AndroidText, BiometricStrength, Context, Error, PolicyBuilder, Text, WindowsText,
};

use super::{Authenticator, Method, Outcome};

/// Shown as "nordpil is trying to unlock nordpil".
const REASON: &str = "unlock nordpil";

pub struct MacAuthenticator;

impl Authenticator for MacAuthenticator {
    fn method(&self) -> Method {
        Method::Prompt
    }

    fn authenticate(&self, _password: Option<&Password>) -> Outcome {
        let Some(policy) = PolicyBuilder::new()
            .biometrics(Some(BiometricStrength::Strong))
            .password(true)
            .build()
        else {
            return Outcome::Failed("macOS has no way to check it is you.".to_owned());
        };
        let text = Text {
            android: AndroidText {
                title: REASON,
                subtitle: None,
                description: None,
            },
            apple: REASON,
            windows: WindowsText::new_truncated("nordpil", REASON),
        };
        // The answer comes on a framework queue; this already runs on the
        // lock's own thread, so it can wait for it.
        let (sender, answer) = mpsc::channel();
        let result = Context::new(())
            .authenticate(text, &policy, move |result| {
                let _ = sender.send(result);
            })
            .and_then(|()| answer.recv().unwrap_or(Err(Error::Unknown)));
        match result {
            Ok(()) => Outcome::Granted,
            Err(Error::UserCanceled | Error::SystemCanceled | Error::AppCanceled) => {
                Outcome::Cancelled
            }
            Err(Error::Authentication) => Outcome::Denied("Authentication failed.".to_owned()),
            Err(error) => Outcome::Failed(format!("macOS couldn't check it is you: {error}.")),
        }
    }
}
