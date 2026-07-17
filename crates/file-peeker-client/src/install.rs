use std::{
    ffi::OsString,
    fmt::Write as _,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use file_peeker_protocol::PROTOCOL_VERSION;
use serde_json::Value;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};

const REMOTE_SCRIPT: &str = r#"set -eu

version=$1
cargo_hint=$2
protocol_version=$3
source=$4
source_root=$5
policy=$6
install_root="$HOME/.file-peeker/servers/$version"
installed_bin="$install_root/bin/file-peeker-server"

verify_server() {
    test -x "$installed_bin" || return 1
    actual=$("$installed_bin" version --format json 2>/dev/null) || return 1
    expected=$(printf '{"server_version":"%s","protocol_versions":[%s]}' "$version" "$protocol_version")
    test "$actual" = "$expected"
}

case "$policy" in
    reuse|overwrite) ;;
    *)
        printf '%s\n' "unknown installation policy: $policy" >&2
        exit 2
        ;;
esac

if test "$policy" = reuse && verify_server; then
    printf '%s\n' 'FILE_PEEKER_INSTALL_OUTCOME=already_installed'
    "$installed_bin" version --format json
    exit 0
fi

if test "$source" = check; then
    printf '%s\n' 'FILE_PEEKER_INSTALL_REQUIRED'
    exit 0
fi

if test -n "$cargo_hint"; then
    cargo_bin=$cargo_hint
else
    cargo_bin=$(command -v cargo) || {
        printf '%s\n' 'cargo was not found on the remote server' >&2
        exit 127
    }
fi

case "$source" in
    crates_io)
        "$cargo_bin" install \
            --locked \
            --force \
            --root "$install_root" \
            --version "$version" \
            --bin file-peeker-server \
            file-peeker-server
        ;;
    workspace)
        protocol_package="$source_root/file-peeker-protocol-$version"
        server_package="$source_root/file-peeker-server-$version"
        tar -xzf "$source_root/protocol.crate" -C "$source_root"
        tar -xzf "$source_root/server.crate" -C "$source_root"
        "$cargo_bin" install \
            --locked \
            --force \
            --root "$install_root" \
            --path "$server_package" \
            --bin file-peeker-server \
            --config "patch.crates-io.file-peeker-protocol.path='$protocol_package'"
        ;;
    *)
        printf '%s\n' "unknown installation source: $source" >&2
        exit 2
        ;;
esac

if ! verify_server; then
    printf '%s\n' 'installed server failed version verification' >&2
    exit 1
fi

printf '%s\n' 'FILE_PEEKER_INSTALL_OUTCOME=installed'
"$installed_bin" version --format json
"#;

#[derive(Debug)]
pub(super) struct RemoteInstallConfig {
    pub(super) destination: String,
    pub(super) server_version: String,
    pub(super) ssh_executable: PathBuf,
    pub(super) ssh_arguments: Vec<OsString>,
    pub(super) remote_cargo: Option<String>,
    pub(super) timeout: Duration,
    pub(super) output_limit: usize,
    pub(super) source: RemoteInstallSource,
    pub(super) policy: RemoteInstallPolicy,
}

impl RemoteInstallConfig {
    pub(super) fn for_current_build(destination: String, policy: RemoteInstallPolicy) -> Self {
        Self {
            destination,
            server_version: env!("CARGO_PKG_VERSION").into(),
            ssh_executable: PathBuf::from("ssh"),
            ssh_arguments: Vec::new(),
            remote_cargo: None,
            timeout: Duration::from_secs(300),
            output_limit: 64 * 1024,
            source: if cfg!(debug_assertions) {
                RemoteInstallSource::Workspace
            } else {
                RemoteInstallSource::CratesIo
            },
            policy,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteInstallSource {
    Workspace,
    CratesIo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteInstallPolicy {
    ReuseExisting,
    #[allow(dead_code)]
    Overwrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteInstallOutcome {
    Installed,
    AlreadyInstalled,
}

#[derive(Debug, Error)]
pub(super) enum RemoteInstallError {
    #[error("invalid remote installation configuration: {0}")]
    InvalidConfig(String),
    #[error("failed to start SSH: {0}")]
    Spawn(String),
    #[error("remote installation timed out after {0} ms")]
    Timeout(u128),
    #[error("SSH exited unsuccessfully: {diagnostics}")]
    Failed { diagnostics: String },
    #[error("invalid remote installation response: {0}")]
    InvalidResponse(String),
    #[error("failed to communicate with SSH: {0}")]
    Io(String),
}

pub(super) async fn install_remote_server(
    config: &RemoteInstallConfig,
) -> Result<RemoteInstallOutcome, RemoteInstallError> {
    validate_config(config)?;

    if config.policy == RemoteInstallPolicy::ReuseExisting
        && remote_server_is_compatible(config).await?
    {
        return Ok(RemoteInstallOutcome::AlreadyInstalled);
    }

    match config.source {
        RemoteInstallSource::Workspace => install_workspace_server(config).await,
        RemoteInstallSource::CratesIo => run_remote_install_script(config, "").await,
    }
}

async fn run_remote_install_script(
    config: &RemoteInstallConfig,
    workspace_root: &str,
) -> Result<RemoteInstallOutcome, RemoteInstallError> {
    let remote_command = build_remote_command(config, workspace_root, None);
    let stdout = run_remote_script(config, remote_command).await?;
    parse_success_response(&stdout.bytes, stdout.truncated, &config.server_version)
}

async fn remote_server_is_compatible(
    config: &RemoteInstallConfig,
) -> Result<bool, RemoteInstallError> {
    let remote_command = build_remote_command(config, "", Some("check"));
    let stdout = run_remote_script(config, remote_command).await?;
    if stdout.truncated {
        return Err(RemoteInstallError::InvalidResponse(
            "SSH stdout exceeded the configured limit".into(),
        ));
    }
    let response = std::str::from_utf8(&stdout.bytes)
        .map_err(|error| RemoteInstallError::InvalidResponse(error.to_string()))?;
    if response
        .lines()
        .any(|line| line == "FILE_PEEKER_INSTALL_OUTCOME=already_installed")
    {
        verify_version_json(
            response.lines().last().unwrap_or_default(),
            &config.server_version,
        )?;
        Ok(true)
    } else if response.trim() == "FILE_PEEKER_INSTALL_REQUIRED" {
        Ok(false)
    } else {
        Err(RemoteInstallError::InvalidResponse(
            "unknown installation check response".into(),
        ))
    }
}

async fn run_remote_script(
    config: &RemoteInstallConfig,
    remote_command: String,
) -> Result<BoundedOutput, RemoteInstallError> {
    let mut command = Command::new(&config.ssh_executable);
    command
        .args(&config.ssh_arguments)
        .arg(&config.destination)
        .arg(remote_command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|error| RemoteInstallError::Spawn(error.to_string()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| RemoteInstallError::Io("SSH stdin was not available".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RemoteInstallError::Io("SSH stdout was not available".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| RemoteInstallError::Io("SSH stderr was not available".into()))?;

    let stdout_task = tokio::spawn(read_bounded(stdout, config.output_limit));
    let stderr_task = tokio::spawn(read_bounded(stderr, config.output_limit));

    stdin
        .write_all(REMOTE_SCRIPT.as_bytes())
        .await
        .map_err(|error| RemoteInstallError::Io(error.to_string()))?;
    drop(stdin);

    let status = if let Ok(result) = timeout(config.timeout, child.wait()).await {
        result.map_err(|error| RemoteInstallError::Io(error.to_string()))?
    } else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        let _ = stdout_task.await;
        let _ = stderr_task.await;
        return Err(RemoteInstallError::Timeout(config.timeout.as_millis()));
    };

    let stdout = join_reader(stdout_task).await?;
    let stderr = join_reader(stderr_task).await?;

    if !status.success() {
        return Err(RemoteInstallError::Failed {
            diagnostics: format_diagnostics(status.code(), &stdout, &stderr),
        });
    }

    Ok(stdout)
}

async fn install_workspace_server(
    config: &RemoteInstallConfig,
) -> Result<RemoteInstallOutcome, RemoteInstallError> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| RemoteInstallError::InvalidConfig("cannot locate workspace root".into()))?;
    package_workspace_crates(workspace).await?;

    let package_dir = workspace.join("target/package");
    let protocol = package_dir.join(format!(
        "file-peeker-protocol-{}.crate",
        config.server_version
    ));
    let server = package_dir.join(format!(
        "file-peeker-server-{}.crate",
        config.server_version
    ));
    let remote_root = format!(
        "/tmp/file-peeker-dev-install-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| RemoteInstallError::Io(error.to_string()))?
            .as_nanos()
    );

    run_workspace_ssh(config, &format!("mkdir -m 700 '{remote_root}'"), None).await?;
    let result = async {
        transfer_workspace_package(config, &protocol, &format!("{remote_root}/protocol.crate"))
            .await?;
        transfer_workspace_package(config, &server, &format!("{remote_root}/server.crate")).await?;

        run_remote_install_script(config, &remote_root).await
    }
    .await;
    let cleanup = run_workspace_ssh(config, &format!("rm -rf '{remote_root}'"), None).await;
    match result {
        Ok(outcome) => {
            cleanup?;
            Ok(outcome)
        }
        Err(error) => {
            let _ = cleanup;
            Err(error)
        }
    }
}

async fn package_workspace_crates(workspace: &Path) -> Result<(), RemoteInstallError> {
    run_local_cargo(
        Command::new("cargo")
            .arg("package")
            .arg("--manifest-path")
            .arg(workspace.join("crates/file-peeker-protocol/Cargo.toml"))
            .arg("--allow-dirty")
            .arg("--no-verify"),
        "package protocol crate",
    )
    .await?;
    run_local_cargo(
        Command::new("cargo")
            .arg("package")
            .arg("--manifest-path")
            .arg(workspace.join("crates/file-peeker-server/Cargo.toml"))
            .arg("--allow-dirty")
            .arg("--no-verify")
            .arg("--config")
            .arg(format!(
                "patch.crates-io.file-peeker-protocol.path='{}'",
                workspace.join("crates/file-peeker-protocol").display()
            )),
        "package server crate",
    )
    .await
}

async fn run_local_cargo(command: &mut Command, action: &str) -> Result<(), RemoteInstallError> {
    let status = command
        .status()
        .await
        .map_err(|error| RemoteInstallError::Spawn(format!("cannot {action}: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(RemoteInstallError::Failed {
            diagnostics: format!("failed to {action}: {status}"),
        })
    }
}

async fn transfer_workspace_package(
    config: &RemoteInstallConfig,
    local: &Path,
    remote: &str,
) -> Result<(), RemoteInstallError> {
    let bytes = std::fs::read(local).map_err(|error| {
        RemoteInstallError::Io(format!("cannot read {}: {error}", local.display()))
    })?;
    run_workspace_ssh(config, &format!("cat > '{remote}'"), Some(&bytes)).await
}

async fn run_workspace_ssh(
    config: &RemoteInstallConfig,
    remote_command: &str,
    input: Option<&[u8]>,
) -> Result<(), RemoteInstallError> {
    let mut child = Command::new(&config.ssh_executable)
        .args(&config.ssh_arguments)
        .arg(&config.destination)
        .arg(remote_command)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| RemoteInstallError::Spawn(error.to_string()))?;
    if let Some(bytes) = input {
        child
            .stdin
            .take()
            .ok_or_else(|| RemoteInstallError::Io("SSH stdin was not available".into()))?
            .write_all(bytes)
            .await
            .map_err(|error| RemoteInstallError::Io(error.to_string()))?;
    }
    let status = timeout(config.timeout, child.wait())
        .await
        .map_err(|_| RemoteInstallError::Timeout(config.timeout.as_millis()))?
        .map_err(|error| RemoteInstallError::Io(error.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(RemoteInstallError::Failed {
            diagnostics: format!("SSH exited with {status}"),
        })
    }
}

fn validate_config(config: &RemoteInstallConfig) -> Result<(), RemoteInstallError> {
    if config.destination.is_empty() || config.destination.starts_with('-') {
        return Err(RemoteInstallError::InvalidConfig(
            "SSH destination is required and must not begin with `-`".into(),
        ));
    }
    if config.ssh_executable.as_os_str().is_empty() {
        return Err(RemoteInstallError::InvalidConfig(
            "SSH executable is required".into(),
        ));
    }
    if !is_exact_stable_version(&config.server_version) {
        return Err(RemoteInstallError::InvalidConfig(
            "server version must use exact MAJOR.MINOR.PATCH form".into(),
        ));
    }
    if config.timeout.is_zero() {
        return Err(RemoteInstallError::InvalidConfig(
            "installation timeout must be positive".into(),
        ));
    }
    if config.output_limit == 0 {
        return Err(RemoteInstallError::InvalidConfig(
            "output limit must be positive".into(),
        ));
    }
    if config.remote_cargo.as_deref() == Some("") {
        return Err(RemoteInstallError::InvalidConfig(
            "remote Cargo executable must not be empty".into(),
        ));
    }

    Ok(())
}

fn is_exact_stable_version(version: &str) -> bool {
    let components = version.split('.').collect::<Vec<_>>();
    components.len() == 3
        && components.iter().all(|component| {
            !component.is_empty()
                && component.bytes().all(|byte| byte.is_ascii_digit())
                && (component == &"0" || !component.starts_with('0'))
        })
}

fn build_remote_command(
    config: &RemoteInstallConfig,
    workspace_root: &str,
    source_override: Option<&str>,
) -> String {
    let cargo = config.remote_cargo.as_deref().unwrap_or("");
    let protocol_version = PROTOCOL_VERSION.to_string();
    let source = source_override.unwrap_or(match config.source {
        RemoteInstallSource::Workspace => "workspace",
        RemoteInstallSource::CratesIo => "crates_io",
    });
    let policy = match config.policy {
        RemoteInstallPolicy::ReuseExisting => "reuse",
        RemoteInstallPolicy::Overwrite => "overwrite",
    };
    [
        "sh",
        "-s",
        "--",
        &config.server_version,
        cargo,
        &protocol_version,
        source,
        workspace_root,
        policy,
    ]
    .into_iter()
    .map(shell_quote)
    .collect::<Vec<_>>()
    .join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[derive(Debug)]
struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<BoundedOutput, std::io::Error> {
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut truncated = false;

    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }

        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < count;
    }

    Ok(BoundedOutput { bytes, truncated })
}

async fn join_reader(
    task: tokio::task::JoinHandle<Result<BoundedOutput, std::io::Error>>,
) -> Result<BoundedOutput, RemoteInstallError> {
    task.await
        .map_err(|error| RemoteInstallError::Io(error.to_string()))?
        .map_err(|error| RemoteInstallError::Io(error.to_string()))
}

fn format_diagnostics(
    exit_code: Option<i32>,
    stdout: &BoundedOutput,
    stderr: &BoundedOutput,
) -> String {
    let mut diagnostics = format!(
        "exit code {}",
        exit_code.map_or_else(|| "unknown".into(), |code| code.to_string())
    );
    append_output(&mut diagnostics, "stdout", stdout);
    append_output(&mut diagnostics, "stderr", stderr);
    diagnostics
}

fn append_output(diagnostics: &mut String, label: &str, output: &BoundedOutput) {
    if !output.bytes.is_empty() {
        let _ = write!(
            diagnostics,
            "; {label}: {}",
            String::from_utf8_lossy(&output.bytes).trim()
        );
        if output.truncated {
            diagnostics.push_str(" [truncated]");
        }
    }
}

fn parse_success_response(
    stdout: &[u8],
    truncated: bool,
    expected_server_version: &str,
) -> Result<RemoteInstallOutcome, RemoteInstallError> {
    if truncated {
        return Err(RemoteInstallError::InvalidResponse(
            "SSH stdout exceeded the configured limit".into(),
        ));
    }

    let text = std::str::from_utf8(stdout)
        .map_err(|error| RemoteInstallError::InvalidResponse(error.to_string()))?;
    let lines = text.lines().collect::<Vec<_>>();
    let [.., marker, version_json] = lines.as_slice() else {
        return Err(RemoteInstallError::InvalidResponse(
            "missing completion marker or version JSON".into(),
        ));
    };

    verify_version_json(version_json, expected_server_version)?;

    match *marker {
        "FILE_PEEKER_INSTALL_OUTCOME=installed" => Ok(RemoteInstallOutcome::Installed),
        "FILE_PEEKER_INSTALL_OUTCOME=already_installed" => {
            Ok(RemoteInstallOutcome::AlreadyInstalled)
        }
        _ => Err(RemoteInstallError::InvalidResponse(
            "unknown completion marker".into(),
        )),
    }
}

fn verify_version_json(
    version_json: &str,
    expected_server_version: &str,
) -> Result<(), RemoteInstallError> {
    let value: Value = serde_json::from_str(version_json)
        .map_err(|error| RemoteInstallError::InvalidResponse(error.to_string()))?;
    let protocol_versions = value["protocol_versions"].as_array().ok_or_else(|| {
        RemoteInstallError::InvalidResponse("missing protocol_versions array".into())
    })?;

    if value["server_version"].as_str() != Some(expected_server_version)
        || !protocol_versions
            .iter()
            .any(|version| version.as_u64() == Some(u64::from(PROTOCOL_VERSION)))
    {
        return Err(RemoteInstallError::InvalidResponse(
            "server version response is incompatible".into(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        time::Duration,
    };

    use tempfile::TempDir;

    use super::{
        PROTOCOL_VERSION, REMOTE_SCRIPT, RemoteInstallConfig, RemoteInstallError,
        RemoteInstallOutcome, RemoteInstallPolicy, RemoteInstallSource, install_remote_server,
        is_exact_stable_version, parse_success_response, shell_quote,
    };

    #[test]
    fn validates_exact_stable_versions() {
        assert!(is_exact_stable_version("0.1.0"));
        assert!(is_exact_stable_version("12.34.56"));
        assert!(!is_exact_stable_version("1.0"));
        assert!(!is_exact_stable_version("1.0.0-beta.1"));
        assert!(!is_exact_stable_version("01.0.0"));
    }

    #[test]
    fn quotes_remote_shell_arguments() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a b'c"), "'a b'\"'\"'c'");
    }

    #[test]
    fn unified_script_supports_workspace_and_crates_io_sources() {
        assert!(REMOTE_SCRIPT.contains("$HOME/.file-peeker/servers/$version"));
        assert!(REMOTE_SCRIPT.contains("--version \"$version\""));
        assert!(REMOTE_SCRIPT.contains("--path \"$server_package\""));
        assert!(REMOTE_SCRIPT.contains("crates_io)"));
        assert!(REMOTE_SCRIPT.contains("workspace)"));
        assert!(REMOTE_SCRIPT.contains("FILE_PEEKER_INSTALL_REQUIRED"));
        assert!(REMOTE_SCRIPT.contains("file-peeker-server"));
        for prohibited in ["--git", "--registry", "--index"] {
            assert!(!REMOTE_SCRIPT.contains(prohibited));
        }
    }

    #[tokio::test]
    async fn installs_then_recognizes_existing_server() {
        let fixture = Fixture::new(false);
        let config = fixture.config(Duration::from_secs(5));

        assert_eq!(
            install_remote_server(&config)
                .await
                .expect("installation should succeed"),
            RemoteInstallOutcome::Installed
        );
        assert_eq!(
            install_remote_server(&config)
                .await
                .expect("verified installation should be reused"),
            RemoteInstallOutcome::AlreadyInstalled
        );
        assert!(
            fixture
                .install_root()
                .join("bin/file-peeker-server")
                .is_file()
        );
    }

    #[tokio::test]
    async fn overwrite_policy_reinstalls_an_existing_server() {
        let fixture = Fixture::new(false);
        let mut config = fixture.config(Duration::from_secs(5));
        config.policy = RemoteInstallPolicy::Overwrite;

        for _ in 0..2 {
            assert_eq!(
                install_remote_server(&config)
                    .await
                    .expect("forced installation should succeed"),
                RemoteInstallOutcome::Installed
            );
        }
    }

    #[tokio::test]
    async fn workspace_reuse_checks_before_packaging() {
        let fixture = Fixture::new(false);
        let mut config = fixture.config(Duration::from_secs(5));
        install_remote_server(&config)
            .await
            .expect("initial installation should succeed");

        config.source = RemoteInstallSource::Workspace;
        assert_eq!(
            install_remote_server(&config)
                .await
                .expect("compatible workspace installation should be reused"),
            RemoteInstallOutcome::AlreadyInstalled
        );
    }

    #[tokio::test]
    async fn requires_an_explicit_destination() {
        let fixture = Fixture::new(false);
        let mut config = fixture.config(Duration::from_secs(5));
        config.destination.clear();

        let error = install_remote_server(&config)
            .await
            .expect_err("missing destinations must fail");
        assert!(matches!(error, RemoteInstallError::InvalidConfig(_)));
    }

    #[test]
    fn rejects_an_incompatible_success_response() {
        let response = br#"FILE_PEEKER_INSTALL_OUTCOME=installed
{"server_version":"9.9.9","protocol_versions":[1]}
"#;

        let error = parse_success_response(response, false, "0.1.0")
            .expect_err("a different server version must not be accepted");
        assert!(matches!(error, RemoteInstallError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn reports_remote_cargo_failure() {
        let fixture = Fixture::new(false);
        let mut config = fixture.config(Duration::from_secs(5));
        config.remote_cargo = Some("/missing/file-peeker-test-cargo".into());

        let error = install_remote_server(&config)
            .await
            .expect_err("a missing remote Cargo executable must fail");
        assert!(matches!(error, RemoteInstallError::Failed { .. }));
    }

    #[tokio::test]
    async fn times_out_and_reaps_ssh() {
        let fixture = Fixture::new(true);
        let config = fixture.config(Duration::from_millis(50));

        let error = install_remote_server(&config)
            .await
            .expect_err("slow SSH should time out");
        assert!(matches!(error, RemoteInstallError::Timeout(_)));
    }

    struct Fixture {
        _directory: TempDir,
        home: PathBuf,
        ssh: PathBuf,
        cargo: PathBuf,
    }

    impl Fixture {
        fn new(slow_ssh: bool) -> Self {
            let directory = tempfile::tempdir().expect("fixture directory should be created");
            let home = directory.path().join("remote-home");
            fs::create_dir(&home).expect("remote home should be created");

            let ssh = directory.path().join("ssh");
            let delay = if slow_ssh { "sleep 1" } else { ":" };
            write_executable(
                &ssh,
                &format!(
                    "#!/bin/sh\n{delay}\nexport HOME={}\nfor last do :; done\nexec sh -c \"$last\"\n",
                    shell_quote(home.to_str().expect("fixture path should be UTF-8"))
                ),
            );

            let cargo = directory.path().join("cargo");
            let protocol_version = PROTOCOL_VERSION;
            write_executable(
                &cargo,
                &format!(
                    r#"#!/bin/sh
root=
version=
while test "$#" -gt 0; do
    case "$1" in
        --root) root=$2; shift 2 ;;
        --version) version=$2; shift 2 ;;
        *) shift ;;
    esac
done
test -n "$root"
test -n "$version"
mkdir -p "$root/bin"
cat > "$root/bin/file-peeker-server" <<EOF
#!/bin/sh
printf '%s\n' '{{"server_version":"$version","protocol_versions":[{protocol_version}]}}'
EOF
chmod +x "$root/bin/file-peeker-server"
"#
                ),
            );

            Self {
                _directory: directory,
                home,
                ssh,
                cargo,
            }
        }

        fn config(&self, timeout: Duration) -> RemoteInstallConfig {
            RemoteInstallConfig {
                destination: "test-remote".into(),
                server_version: "0.1.0".into(),
                ssh_executable: self.ssh.clone(),
                ssh_arguments: Vec::new(),
                remote_cargo: Some(
                    self.cargo
                        .to_str()
                        .expect("fixture path should be UTF-8")
                        .into(),
                ),
                timeout,
                output_limit: 64 * 1024,
                source: RemoteInstallSource::CratesIo,
                policy: RemoteInstallPolicy::ReuseExisting,
            }
        }

        fn install_root(&self) -> PathBuf {
            self.home.join(".file-peeker/servers/0.1.0")
        }
    }

    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).expect("fixture executable should be written");
        let mut permissions = fs::metadata(path)
            .expect("fixture executable metadata should exist")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions)
            .expect("fixture executable should become executable");
    }
}
