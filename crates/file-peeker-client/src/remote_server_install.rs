use std::{ffi::OsString, fmt::Write as _, path::PathBuf, process::Stdio, time::Duration};

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
install_root="$HOME/.file-peeker/servers/$version"
installed_bin="$install_root/bin/file-peeker-server"

verify_server() {
    test -x "$installed_bin" || return 1
    actual=$("$installed_bin" version --format json 2>/dev/null) || return 1
    expected=$(printf '{"server_version":"%s","protocol_versions":[%s]}' "$version" "$protocol_version")
    test "$actual" = "$expected"
}

if verify_server; then
    printf '%s\n' 'FILE_PEEKER_INSTALL_OUTCOME=already_installed'
    "$installed_bin" version --format json
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

"$cargo_bin" install \
    --locked \
    --force \
    --root "$install_root" \
    --version "$version" \
    --bin file-peeker-server \
    file-peeker-server

if ! verify_server; then
    printf '%s\n' 'installed server failed version verification' >&2
    exit 1
fi

printf '%s\n' 'FILE_PEEKER_INSTALL_OUTCOME=installed'
"$installed_bin" version --format json
"#;

#[derive(Debug)]
struct RemoteInstallConfig {
    destination: String,
    server_version: String,
    ssh_executable: PathBuf,
    ssh_arguments: Vec<OsString>,
    remote_cargo: Option<String>,
    timeout: Duration,
    output_limit: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteInstallOutcome {
    Installed,
    AlreadyInstalled,
}

#[derive(Debug, Error)]
enum RemoteInstallError {
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

async fn install_remote_server(
    config: &RemoteInstallConfig,
) -> Result<RemoteInstallOutcome, RemoteInstallError> {
    validate_config(config)?;

    let remote_command = build_remote_command(config);
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

    parse_success_response(&stdout.bytes, stdout.truncated, &config.server_version)
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

fn build_remote_command(config: &RemoteInstallConfig) -> String {
    let cargo = config.remote_cargo.as_deref().unwrap_or("");
    ["sh", "-s", "--", &config.server_version, cargo]
        .into_iter()
        .map(shell_quote)
        .chain(std::iter::once(PROTOCOL_VERSION.to_string()))
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
        RemoteInstallOutcome, install_remote_server, is_exact_stable_version,
        parse_success_response, shell_quote,
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
    fn production_script_uses_only_crates_io_package_installation() {
        assert!(REMOTE_SCRIPT.contains("$HOME/.file-peeker/servers/$version"));
        assert!(REMOTE_SCRIPT.contains("--version \"$version\""));
        assert!(REMOTE_SCRIPT.contains("file-peeker-server"));
        for prohibited in ["--path", "--git", "--registry", "--index"] {
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
