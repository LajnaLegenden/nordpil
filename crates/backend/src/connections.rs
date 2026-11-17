//! Saving, deleting and opening connections.

use std::time::{Duration, Instant, SystemTime};

use aws_config::default_provider::credentials::DefaultCredentialsChain;
use aws_config::profile::ProfileFileCredentialsProvider;
use aws_credential_types::Credentials;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider, future};
use aws_runtime::env_config::file::{EnvConfigFileKind, EnvConfigFiles};
use mongodb::Client;
use mongodb::bson::doc;
use mongodb::options::{AuthMechanism, ClientOptions};
use uuid::Uuid;

use crate::store::SavedConnection;
use crate::uri::ConnectionUri;
use crate::{
    AwsProfileFiles, Backend, BackendError, DatabaseInfo, ErrorKind, Password, PasswordChange,
    Session,
};

/// Used unless the connection string sets its own timeouts. The driver's
/// 30 s default is too long to stare at a spinner.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

impl Backend {
    pub(crate) async fn save_connection(
        &self,
        mut connection: SavedConnection,
        password: PasswordChange,
    ) -> Result<SavedConnection, BackendError> {
        let uri = parse_uri(&connection.uri)?;
        let password = match (password, &uri.password) {
            (PasswordChange::Keep, Some(in_uri)) => PasswordChange::Set(Password::new(in_uri)),
            (change, _) => change,
        };
        connection.uri = uri.without_password().to_string();
        if connection.name.trim().is_empty() {
            connection.name = uri.hosts.join(",");
        }

        let id = connection.id;
        self.with_secrets(move |secrets| match password {
            _ if !secrets.available() => Ok(()),
            PasswordChange::Keep => Ok(()),
            PasswordChange::Set(password) => secrets.set(id, password.expose()),
            PasswordChange::Clear => secrets.delete(id),
        })
        .await?;

        let saved = connection.clone();
        self.with_store(move |store| {
            let mut connections = store.load()?;
            match connections.iter_mut().find(|c| c.id == saved.id) {
                Some(existing) => *existing = saved,
                None => connections.push(saved),
            }
            store.save(&connections)
        })
        .await?;

        // Turning read-only on must apply to an open session right away.
        if let Some(session) = self.lock_sessions().get_mut(&id) {
            session.read_only = connection.read_only;
        }
        Ok(connection)
    }

    /// `index` is the connection's place once it is out of its old one.
    pub(crate) async fn move_connection(&self, id: Uuid, index: usize) -> Result<(), BackendError> {
        self.with_store(move |store| {
            let mut connections = store.load()?;
            let from = connections
                .iter()
                .position(|c| c.id == id)
                .ok_or_else(|| BackendError::new("This connection no longer exists."))?;
            let moved = connections.remove(from);
            connections.insert(index.min(connections.len()), moved);
            store.save(&connections)
        })
        .await
    }

    pub(crate) async fn delete_connection(&self, id: Uuid) -> Result<(), BackendError> {
        self.lock_sessions().remove(&id);
        self.with_store(move |store| {
            let mut connections = store.load()?;
            connections.retain(|c| c.id != id);
            store.save(&connections)
        })
        .await?;

        let deleted = self
            .with_secrets(move |secrets| {
                if secrets.available() {
                    secrets.delete(id)
                } else {
                    Ok(())
                }
            })
            .await;
        if let Err(error) = deleted {
            tracing::warn!(%id, %error, detail = ?error.detail, "could not delete stored password");
        }
        if let Err(error) = self.forget_queries(id).await {
            tracing::warn!(%id, %error, detail = ?error.detail, "could not delete saved queries");
        }
        if let Err(error) = self.forget_scripts(id).await {
            tracing::warn!(%id, %error, detail = ?error.detail, "could not delete saved scripts");
        }
        if let Err(error) = self.forget_pipelines(id).await {
            tracing::warn!(%id, %error, detail = ?error.detail, "could not delete saved pipelines");
        }
        if let Err(error) = self.forget_mappings(id).await {
            tracing::warn!(%id, %error, detail = ?error.detail, "could not delete linked fields");
        }
        Ok(())
    }

    pub(crate) async fn connect(
        &self,
        id: Uuid,
        password: Option<Password>,
        remember_password: bool,
    ) -> Result<Vec<DatabaseInfo>, BackendError> {
        let saved = self
            .with_store(move |store| {
                store
                    .load()?
                    .into_iter()
                    .find(|c| c.id == id)
                    .ok_or_else(|| BackendError::new("This connection no longer exists."))
            })
            .await?;
        let uri = parse_uri(&saved.uri)?;

        let typed = password.is_some();
        let password = match password {
            Some(password) => Some(password),
            None if uri.username.is_some() => self.stored_password(id).await,
            None => None,
        };
        if uri.username.is_some() && password.is_none() {
            return Err(BackendError::new("Enter the password for this connection.")
                .with_kind(ErrorKind::PasswordRequired));
        }

        let mut options = ClientOptions::parse(saved.uri.as_str()).await?;
        if let (Some(credential), Some(password)) = (options.credential.as_mut(), &password) {
            credential.password = Some(password.expose().to_owned());
        }
        let aws = options
            .credential
            .as_ref()
            .is_some_and(|c| c.mechanism == Some(AuthMechanism::MongoDbAws));
        let aws_profile = saved.aws_profile.as_deref().filter(|p| !p.is_empty());
        if aws && let Some(credential) = options.credential.as_mut() {
            // Keys in the connection string win over the default lookup, as
            // in the driver; a chosen profile wins over both.
            let uri_keys = credential.username.is_some();
            if aws_profile.is_some() || self.aws_profile_files.is_some() || !uri_keys {
                credential.aws_credential_provider =
                    Some(aws_credentials(aws_profile, self.aws_profile_files.as_ref()).await);
            }
        }
        options.app_name.get_or_insert_with(|| "nordpil".to_owned());
        options
            .server_selection_timeout
            .get_or_insert(DEFAULT_TIMEOUT);
        options.connect_timeout.get_or_insert(DEFAULT_TIMEOUT);
        let client = Client::with_options(options)?;
        // Forces server selection and authentication, so bad settings fail here.
        client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|error| {
                let error = BackendError::from(error);
                if aws && error.kind == ErrorKind::AuthenticationFailed {
                    aws_error(error, aws_profile)
                } else {
                    error
                }
            })?;
        let databases = crate::databases::list(&client).await?;

        if typed
            && remember_password
            && let Some(password) = password
        {
            let stored = self
                .with_secrets(move |secrets| {
                    if secrets.available() {
                        secrets.set(id, password.expose())
                    } else {
                        Ok(())
                    }
                })
                .await;
            if let Err(error) = stored {
                tracing::warn!(%id, %error, detail = ?error.detail, "could not store password");
            }
        }

        let session = Session {
            client,
            read_only: saved.read_only,
        };
        self.lock_sessions().insert(id, session);
        tracing::info!(%id, "connected");
        Ok(databases)
    }

    async fn stored_password(&self, id: Uuid) -> Option<Password> {
        let stored = self
            .with_secrets(move |secrets| {
                if secrets.available() {
                    secrets.get(id)
                } else {
                    Ok(None)
                }
            })
            .await;
        stored
            .unwrap_or_else(|error| {
                tracing::warn!(%id, %error, detail = ?error.detail, "could not read stored password");
                None
            })
            .map(Password::new)
    }
}

fn parse_uri(uri: &str) -> Result<ConnectionUri, BackendError> {
    ConnectionUri::parse(uri).map_err(|error| BackendError::new(error.to_string()))
}

/// Credentials for a `MONGODB-AWS` connection: the chosen profile, or with
/// none the AWS SDK's default lookup (environment, `AWS_PROFILE` or the
/// default profile, then ECS or EC2). Either way they are cached, see
/// [`CachedCredentials`].
async fn aws_credentials(
    profile: Option<&str>,
    files: Option<&AwsProfileFiles>,
) -> SharedCredentialsProvider {
    if profile.is_none() && files.is_none() {
        let chain = DefaultCredentialsChain::builder().build().await;
        return SharedCredentialsProvider::new(CachedCredentials::new(chain));
    }
    let mut builder = ProfileFileCredentialsProvider::builder();
    if let Some(profile) = profile {
        builder = builder.profile_name(profile);
    }
    if let Some(files) = files {
        builder = builder.profile_files(
            EnvConfigFiles::builder()
                .with_file(EnvConfigFileKind::Config, &files.config)
                .with_file(EnvConfigFileKind::Credentials, &files.credentials)
                .build(),
        );
    }
    SharedCredentialsProvider::new(CachedCredentials::new(builder.build()))
}

/// Refetch this long before credentials expire, so a handshake never signs
/// with ones about to lapse.
const AWS_EXPIRY_MARGIN: Duration = Duration::from_secs(5 * 60);
/// How long credentials without an expiry (static keys) are kept, so edits to
/// `~/.aws/credentials` still get picked up.
const AWS_STATIC_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// The driver asks for AWS credentials on every new pooled connection and
/// doesn't cache them. Uncached, each one re-reads `~/.aws`, and SSO and
/// `credential_process` profiles call AWS or spawn a process every time.
/// The lock is held while fetching, so connections opened together share one
/// lookup. Failures are not cached.
#[derive(Debug)]
struct CachedCredentials<P> {
    inner: P,
    cached: tokio::sync::Mutex<Option<(Credentials, Instant)>>,
}

impl<P> CachedCredentials<P> {
    fn new(inner: P) -> Self {
        Self {
            inner,
            cached: tokio::sync::Mutex::new(None),
        }
    }
}

/// Whether credentials fetched at `fetched` can still be used.
fn aws_credentials_fresh(credentials: &Credentials, fetched: Instant) -> bool {
    match credentials.expiry() {
        Some(expiry) => expiry
            .duration_since(SystemTime::now())
            .is_ok_and(|left| left > AWS_EXPIRY_MARGIN),
        None => fetched.elapsed() < AWS_STATIC_LIFETIME,
    }
}

impl<P: ProvideCredentials> ProvideCredentials for CachedCredentials<P> {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        future::ProvideCredentials::new(async move {
            let mut cached = self.cached.lock().await;
            if let Some((credentials, fetched)) = cached.as_ref()
                && aws_credentials_fresh(credentials, *fetched)
            {
                return Ok(credentials.clone());
            }
            let credentials = self.inner.provide_credentials().await?;
            *cached = Some((credentials.clone(), Instant::now()));
            Ok(credentials)
        })
    }
}

/// The driver reports missing, expired and rejected AWS credentials alike as
/// an authentication failure; its text is kept as the detail.
fn aws_error(error: BackendError, profile: Option<&str>) -> BackendError {
    let message = match profile {
        Some(profile) => format!(
            "AWS sign-in with profile \"{profile}\" failed. Check that the profile exists and \
             is signed in (aws sso login --profile {profile}), and that its IAM identity has \
             a database user."
        ),
        None => "AWS sign-in failed. Check that AWS credentials are available and signed in \
                 (aws sso login), and that the IAM identity has a database user."
            .to_owned(),
    };
    BackendError {
        kind: ErrorKind::AwsCredentials,
        message,
        detail: error.detail,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Counts lookups; hands out credentials that expire `lifetime` from now.
    #[derive(Debug)]
    struct Counting {
        calls: AtomicUsize,
        lifetime: Option<Duration>,
    }

    impl ProvideCredentials for Counting {
        fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
        where
            Self: 'a,
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let expiry = self.lifetime.map(|lifetime| SystemTime::now() + lifetime);
            future::ProvideCredentials::ready(Ok(Credentials::new(
                "AKID", "secret", None, expiry, "test",
            )))
        }
    }

    fn counting(lifetime: Option<Duration>) -> CachedCredentials<Counting> {
        CachedCredentials::new(Counting {
            calls: AtomicUsize::new(0),
            lifetime,
        })
    }

    #[tokio::test]
    async fn aws_credentials_are_fetched_once_while_fresh() {
        for lifetime in [Some(Duration::from_secs(3600)), None] {
            let cached = counting(lifetime);
            for _ in 0..3 {
                cached.provide_credentials().await.unwrap();
            }
            assert_eq!(cached.inner.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn aws_credentials_close_to_expiry_are_fetched_again() {
        let cached = counting(Some(AWS_EXPIRY_MARGIN / 2));
        cached.provide_credentials().await.unwrap();
        cached.provide_credentials().await.unwrap();
        assert_eq!(cached.inner.calls.load(Ordering::SeqCst), 2);
    }
}
