//! `MONGODB-AWS` (IAM) authentication against MongoDB Enterprise in Docker.
//! Run with `just test-int`.
//!
//! mongod checks AWS credentials by forwarding the client's signed STS request
//! to `awsSTSUrl`. Here that is `tests/fixtures/mock_sts.py`, which answers by
//! access key ID, so no AWS account is involved. mongod insists on HTTPS, so
//! each run makes a throwaway CA that mongod trusts.

use nordpil_backend::secrets::MemorySecrets;
use nordpil_backend::store::{ConnectionStore, SavedConnection};
use nordpil_backend::{
    Backend, BackendError, CancellationToken, Command, DatabaseInfo, ErrorKind, Handler,
    PasswordChange, Response,
};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use testcontainers_modules::testcontainers::core::{
    CmdWaitFor, ExecCommand, IntoContainerPort, WaitFor,
};
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, GenericImage, ImageExt};
use uuid::Uuid;

const ENTERPRISE_IMAGE: &str = "mongodb/mongodb-enterprise-server";
const ENTERPRISE_TAG: &str = "8.0-ubuntu2204";

/// Run inside the mongod container, where the localhost exception allows
/// creating the first user. The ARNs match the users `mock_sts.py` returns.
const SETUP: &str = r#"
const admin = db.getSiblingDB("admin");
admin.createUser({ user: "root", pwd: "root", roles: ["root"] });
admin.auth("root", "root");
for (const name of ["alpha", "beta", "token"]) {
    db.getSiblingDB(name).items.insertOne({ name });
    db.getSiblingDB("$external").runCommand({
        createUser: `arn:aws:iam::123456789012:user/${name}`,
        roles: [{ role: "readWrite", db: name }],
    });
}
"#;

/// Profiles as `~/.aws/credentials` would hold them. Keys are made up; the
/// fake STS only looks at the access key ID and session token.
const CREDENTIALS: &str = "\
[default]
aws_access_key_id = AKIDBETA
aws_secret_access_key = secret-beta

[alpha]
aws_access_key_id = AKIDALPHA
aws_secret_access_key = secret-alpha

[token]
aws_access_key_id = AKIDTOKEN
aws_secret_access_key = secret-token
aws_session_token = session-token-1

[rejected]
aws_access_key_id = AKIDREJECTED
aws_secret_access_key = secret-rejected
";

struct AwsCluster {
    _sts: ContainerAsync<GenericImage>,
    _mongo: ContainerAsync<GenericImage>,
    uri: String,
}

/// Starts the fake STS and an Enterprise mongod that sends AWS logins to it.
async fn start_cluster() -> AwsCluster {
    let run = Uuid::new_v4().simple().to_string();
    let network = format!("nordpil-aws-{run}");
    let sts_host = format!("nordpil-sts-{run}");

    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "nordpil test CA");
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let sts_key = KeyPair::generate().unwrap();
    let sts_cert = CertificateParams::new(vec![sts_host.clone()])
        .unwrap()
        .signed_by(&sts_key, &ca, &ca_key)
        .unwrap();

    let sts = GenericImage::new("python", "3.12-alpine")
        .with_wait_for(WaitFor::message_on_stdout("mock-sts ready"))
        .with_cmd(["python", "-u", "/app/mock_sts.py"])
        .with_network(&network)
        .with_container_name(&sts_host)
        .with_copy_to(
            "/app/mock_sts.py",
            include_bytes!("fixtures/mock_sts.py").to_vec(),
        )
        .with_copy_to("/app/sts.pem", sts_cert.pem().into_bytes())
        .with_copy_to("/app/sts.key", sts_key.serialize_pem().into_bytes())
        .start()
        .await
        .unwrap();

    let mongo = GenericImage::new(ENTERPRISE_IMAGE, ENTERPRISE_TAG)
        .with_exposed_port(27017.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Waiting for connections"))
        .with_network(&network)
        // Only the test CA: mongod has no reason to reach anything else.
        .with_copy_to("/etc/ssl/certs/ca-certificates.crt", ca.pem().into_bytes())
        .with_cmd([
            "--auth".to_owned(),
            "--bind_ip_all".to_owned(),
            "--setParameter".to_owned(),
            "authenticationMechanisms=SCRAM-SHA-256,MONGODB-AWS".to_owned(),
            "--setParameter".to_owned(),
            format!("awsSTSUrl=https://{sts_host}:8443"),
        ])
        .start()
        .await
        .unwrap();
    mongo
        .exec(
            ExecCommand::new(["mongosh", "--quiet", "--eval", SETUP])
                .with_cmd_ready_condition(CmdWaitFor::exit_code(0)),
        )
        .await
        .unwrap();

    let port = mongo.get_host_port_ipv4(27017).await.unwrap();
    AwsCluster {
        _sts: sts,
        _mongo: mongo,
        uri: format!(
            "mongodb://127.0.0.1:{port}/?directConnection=true\
             &authMechanism=MONGODB-AWS&authSource=$external"
        ),
    }
}

/// A backend that reads AWS profiles from a temporary directory, not `~/.aws`.
fn aws_backend() -> Backend {
    let dir = std::env::temp_dir().join(format!("nordpil-aws-it-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("aws-config");
    let credentials = dir.join("aws-credentials");
    std::fs::write(&config, "").unwrap();
    std::fs::write(&credentials, CREDENTIALS).unwrap();
    Backend::new(
        ConnectionStore::new(dir.join("connections.toml")),
        MemorySecrets::default(),
    )
    .with_aws_profile_files(config, credentials)
}

/// Saves a connection and connects to it without a password, as the app does
/// when the user clicks a saved connection.
async fn connect(
    backend: &Backend,
    uri: &str,
    aws_profile: Option<&str>,
) -> Result<Vec<DatabaseInfo>, BackendError> {
    let id = Uuid::new_v4();
    let connection = SavedConnection {
        id,
        name: "aws".to_owned(),
        uri: uri.to_owned(),
        color: None,
        read_only: false,
        aws_profile: aws_profile.map(str::to_owned),
    };
    let save = Command::SaveConnection {
        connection,
        password: PasswordChange::Keep,
    };
    let Response::ConnectionSaved(saved) = backend.handle(save, CancellationToken::new()).await
    else {
        panic!("unexpected response");
    };
    saved.unwrap();

    let connect = Command::Connect {
        id,
        password: None,
        remember_password: false,
    };
    let Response::Connected { result, .. } =
        backend.handle(connect, CancellationToken::new()).await
    else {
        panic!("unexpected response");
    };
    result
}

/// Each IAM user may only see its own database, so the list shows who we are.
fn names(databases: &[DatabaseInfo]) -> Vec<&str> {
    databases.iter().map(|db| db.name.as_str()).collect()
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn connects_as_the_chosen_profile_without_a_password() {
    let cluster = start_cluster().await;
    let backend = aws_backend();

    let alpha = connect(&backend, &cluster.uri, Some("alpha"))
        .await
        .unwrap();
    assert_eq!(names(&alpha), ["alpha"]);

    let beta = connect(&backend, &cluster.uri, Some("default"))
        .await
        .unwrap();
    assert_eq!(names(&beta), ["beta"]);
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn uses_the_default_profile_when_none_is_chosen() {
    let cluster = start_cluster().await;
    let backend = aws_backend();

    let databases = connect(&backend, &cluster.uri, None).await.unwrap();
    assert_eq!(names(&databases), ["beta"]);
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn sends_the_session_token_of_temporary_credentials() {
    let cluster = start_cluster().await;
    let backend = aws_backend();

    // The fake STS rejects AKIDTOKEN unless its session token comes along.
    let databases = connect(&backend, &cluster.uri, Some("token"))
        .await
        .unwrap();
    assert_eq!(names(&databases), ["token"]);
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn bad_or_missing_credentials_are_aws_errors() {
    let cluster = start_cluster().await;
    let backend = aws_backend();

    let rejected = connect(&backend, &cluster.uri, Some("rejected"))
        .await
        .unwrap_err();
    assert_eq!(rejected.kind, ErrorKind::AwsCredentials, "{rejected:?}");

    let missing = connect(&backend, &cluster.uri, Some("nordpil-missing"))
        .await
        .unwrap_err();
    assert_eq!(missing.kind, ErrorKind::AwsCredentials, "{missing:?}");
    assert!(
        missing.message.contains("nordpil-missing"),
        "the message should name the profile: {missing:?}"
    );
}
