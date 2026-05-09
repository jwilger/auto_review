use crate::config::{compose_state_path, resolve_db_backing, DbBacking};
use crate::dedup::{DeliveryDedup, RecentDeliveries, SqliteDeliveries};
use crate::metrics::{Metrics, MetricsObserver};
use crate::poller::{ChatPoller, SharedCommentCursors, DEFAULT_POLL_INTERVAL};
use crate::ratelimit::TokenBucket;
use crate::{
    build_router, AppState, ChatDeps, GatewayInfo, ReadinessProbe, RuntimeIsolationPostureInfo,
};
use anyhow::{Context, Result};
use ar_forgejo::Client as ForgejoClient;
use ar_index::{
    InMemoryLearningsStore, InMemoryVectorStore, SqliteLearningsStore, SqliteVectorStore,
    VectorStore,
};
use ar_llm::{ModelTier, OpenAiProvider, Router as LlmRouter};
use ar_orchestrator::review_history::{InMemoryReviewHistory, ReviewHistory};
use ar_orchestrator::sqlite_history::SqliteReviewHistory;
use ar_orchestrator::SpawningDispatcher;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Clone, Copy)]
struct GatewayStartupEnvValues<'a> {
    bind: Option<&'a str>,
    webhook_secret: Option<&'a str>,
    forgejo_base_url: Option<&'a str>,
    ar_forgejo_token: Option<&'a str>,
    llm_base_url: Option<&'a str>,
    llm_reasoning_model: Option<&'a str>,
}

#[derive(Debug)]
struct GatewayStartupConfig {
    bind: String,
    webhook_secret: String,
    forgejo_base_url: String,
    ar_forgejo_token: String,
    llm_base_url: String,
    llm_reasoning_model: String,
}

#[derive(Debug)]
pub struct StartupOptions {
    pub bare: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GatewayLaunchOutcome {
    ContinueInProcess,
    #[allow(dead_code)]
    OuterLauncherFinished,
}

#[derive(Clone, Copy)]
struct GatewayLauncherEnvValues<'a> {
    bare: Option<&'a str>,
    external_isolation: Option<&'a str>,
}

#[derive(Debug)]
struct OciSetupDiagnostic {
    _detail: String,
}

#[derive(Debug)]
struct RuntimeIsolationPostureInput<'a> {
    bare: Option<&'a str>,
    external_isolation: Option<&'a str>,
    oci_setup_diagnostic: Option<OciSetupDiagnostic>,
    target_os: &'a str,
}

#[derive(Debug, Eq, PartialEq)]
enum RuntimeIsolationPostureKind {
    OciDefault,
    ExternalContainer,
    ExplicitBare,
    OciSetupFailure,
    UnsupportedPlatform,
}

#[derive(Debug)]
struct RuntimeIsolationPosture {
    kind: RuntimeIsolationPostureKind,
    operator_label: String,
    operator_detail: String,
}

impl From<RuntimeIsolationPosture> for RuntimeIsolationPostureInfo {
    fn from(posture: RuntimeIsolationPosture) -> Self {
        Self {
            kind: match posture.kind {
                RuntimeIsolationPostureKind::OciDefault => "oci_default",
                RuntimeIsolationPostureKind::ExternalContainer => "external_container",
                RuntimeIsolationPostureKind::ExplicitBare => "explicit_bare",
                RuntimeIsolationPostureKind::OciSetupFailure => "oci_setup_failed",
                RuntimeIsolationPostureKind::UnsupportedPlatform => "unsupported_platform",
            },
            label: posture.operator_label,
            detail: posture.operator_detail,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct EmbeddedOciGatewayInputs<'a> {
    bundle_path: &'a Path,
    runtime_path: &'a Path,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
struct EmbeddedOciGatewayEnvValues<'a> {
    bundle_path: Option<&'a str>,
    runtime_path: Option<&'a str>,
}

#[derive(Debug)]
struct PackagedOciRuntimeCommand {
    program: PathBuf,
    args: Vec<PathBuf>,
    clear_ambient_env: bool,
    env: Vec<(String, String)>,
}

const PACKAGED_NIX_STORE_PREFIX: &str = "/nix/store";

const STATIC_INNER_GATEWAY_ENV: &[(&str, &str)] = &[
    ("SSL_CERT_FILE", "/etc/ssl/certs/ca-bundle.crt"),
    ("PATH", "/bin"),
    ("AR_GATEWAY_BIND", "0.0.0.0:8080"),
    ("AR_GATEWAY_EXTERNAL_ISOLATION", "container"),
    ("RUST_LOG", "info,ar_gateway=debug"),
];

const REQUIRED_INNER_GATEWAY_ENV: &[&str] = &[
    "WEBHOOK_SECRET",
    "FORGEJO_BASE_URL",
    "AR_FORGEJO_TOKEN",
    "LLM_BASE_URL",
];

const INNER_GATEWAY_ENV_ALLOWLIST: &[&str] = &[
    "AR_GATEWAY_BIND",
    "WEBHOOK_SECRET",
    "FORGEJO_BASE_URL",
    "AR_FORGEJO_TOKEN",
    "LLM_BASE_URL",
    "LLM_API_KEY",
    "LLM_REASONING_MODEL",
    "LLM_CHEAP_MODEL",
    "LLM_CHEAP_BASE_URL",
    "LLM_CHEAP_API_KEY",
    "LLM_EMBEDDING_MODEL",
    "LLM_EMBEDDING_BASE_URL",
    "LLM_EMBEDDING_API_KEY",
    "AR_EMBED_INPUT_CAP_BYTES",
    "AR_EMBED_BATCH_SIZE",
    "AR_EMBED_NUM_CTX",
    "AR_BOT_LOGIN",
    "AR_BOT_NAME",
    "AR_CI_REVIEW_TOKEN",
    "AR_LEARNINGS_DB",
    "AR_HISTORY_DB",
    "AR_VECTOR_DB",
    "AR_DEDUP_DB",
    "AR_DEDUP_CAPACITY",
    "AR_POLL_INTERVAL_SECS",
    "AR_READINESS_TTL_SECS",
    "AR_REVIEW_CONCURRENCY",
    "AR_WEBHOOK_RATE_PER_SEC",
    "AR_WEBHOOK_BURST",
    "AR_SEVERITY_FLOOR",
    "RUST_LOG",
];

impl OciSetupDiagnostic {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            _detail: detail.into(),
        }
    }

    fn public_detail(&self) -> &str {
        if self._detail.contains("/run/secrets")
            || self._detail.contains("secret-bearing")
            || self._detail.contains("ar-token")
        {
            "embedded OCI setup failed before inner gateway startup"
        } else {
            &self._detail
        }
    }
}

impl fmt::Display for OciSetupDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.public_detail())
    }
}

fn select_gateway_launcher(
    values: GatewayLauncherEnvValues<'_>,
    prepare_oci: impl FnOnce() -> std::result::Result<GatewayLaunchOutcome, OciSetupDiagnostic>,
) -> Result<GatewayLaunchOutcome> {
    if values.external_isolation == Some("container") {
        tracing::info!(
            "external container isolation marker detected; embedded OCI launcher skipped because this process is already expected to run inside the packaged container boundary"
        );
        return Ok(GatewayLaunchOutcome::ContinueInProcess);
    }

    let use_oci = match values.bare.map(str::trim).map(str::to_ascii_lowercase) {
        None => true,
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on") => false,
        Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off") => true,
        Some(_) => anyhow::bail!(
            "AR_GATEWAY_BARE has an unrecognized value; use true/false, yes/no, on/off, or 1/0"
        ),
    };

    if use_oci {
        prepare_oci().map_err(|diagnostic| {
            anyhow::anyhow!(
                "OCI gateway launcher setup failed: {diagnostic}; set AR_GATEWAY_BARE (or pass --bare) to opt out"
            )
        })
    } else {
        tracing::warn!("{}", explicit_bare_gateway_mode_warning());
        Ok(GatewayLaunchOutcome::ContinueInProcess)
    }
}

fn explicit_bare_gateway_mode_warning() -> &'static str {
    "Warning: bare gateway mode selected; only application-level controls are active, not container-equivalent isolation."
}

fn classify_runtime_isolation_posture(
    input: RuntimeIsolationPostureInput<'_>,
) -> Result<RuntimeIsolationPosture> {
    if input.target_os != "linux" {
        return Ok(RuntimeIsolationPosture {
            kind: RuntimeIsolationPostureKind::UnsupportedPlatform,
            operator_label: "unsupported platform".to_string(),
            operator_detail:
                "Embedded OCI isolation is unavailable on this platform; run in bare mode or provide external isolation."
                    .to_string(),
        });
    }

    if input.external_isolation == Some("container") {
        return Ok(RuntimeIsolationPosture {
            kind: RuntimeIsolationPostureKind::ExternalContainer,
            operator_label: "external container isolation".to_string(),
            operator_detail: "Gateway is already inside an externally provided container boundary."
                .to_string(),
        });
    }

    let use_oci = match input.bare.map(str::trim).map(str::to_ascii_lowercase) {
        None => true,
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on") => false,
        Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off") => true,
        Some(_) => anyhow::bail!(
            "AR_GATEWAY_BARE has an unrecognized value; use true/false, yes/no, on/off, or 1/0"
        ),
    };

    if !use_oci {
        return Ok(RuntimeIsolationPosture {
            kind: RuntimeIsolationPostureKind::ExplicitBare,
            operator_label: "bare gateway mode".to_string(),
            operator_detail: explicit_bare_gateway_mode_warning().to_string(),
        });
    }

    if let Some(diagnostic) = input.oci_setup_diagnostic {
        return Ok(RuntimeIsolationPosture {
            kind: RuntimeIsolationPostureKind::OciSetupFailure,
            operator_label: "OCI setup failed".to_string(),
            operator_detail: format!(
                "OCI setup failed: {diagnostic}; set AR_GATEWAY_BARE to opt out"
            ),
        });
    }

    Ok(RuntimeIsolationPosture {
        kind: RuntimeIsolationPostureKind::OciDefault,
        operator_label: "packaged OCI container isolation".to_string(),
        operator_detail: "Gateway uses embedded OCI container-equivalent isolation by default."
            .to_string(),
    })
}

fn select_gateway_launcher_for_startup_options(
    options: StartupOptions,
    values: GatewayLauncherEnvValues<'_>,
    prepare_oci: impl FnOnce() -> std::result::Result<GatewayLaunchOutcome, OciSetupDiagnostic>,
) -> Result<GatewayLaunchOutcome> {
    let bare = if options.bare {
        Some("true")
    } else {
        values.bare
    };

    select_gateway_launcher(
        GatewayLauncherEnvValues {
            bare,
            external_isolation: values.external_isolation,
        },
        prepare_oci,
    )
}

fn prepare_embedded_oci_gateway_with_inputs(
    inputs: EmbeddedOciGatewayInputs<'_>,
    launch: impl FnOnce(
        EmbeddedOciGatewayInputs<'_>,
    ) -> std::result::Result<GatewayLaunchOutcome, OciSetupDiagnostic>,
) -> std::result::Result<GatewayLaunchOutcome, OciSetupDiagnostic> {
    validate_packaged_oci_input_paths(inputs)?;

    launch(inputs)
}

fn validate_packaged_oci_input_paths(
    inputs: EmbeddedOciGatewayInputs<'_>,
) -> std::result::Result<(), OciSetupDiagnostic> {
    validate_packaged_path_shape("bundle", inputs.bundle_path)?;
    validate_packaged_path_shape("runtime", inputs.runtime_path)?;

    if inputs.bundle_path.starts_with(PACKAGED_NIX_STORE_PREFIX)
        && inputs.runtime_path.starts_with(PACKAGED_NIX_STORE_PREFIX)
    {
        return Ok(());
    }

    if let Some(release_root) = portable_release_root_for_bundle(inputs.bundle_path) {
        let runtime_launcher = release_root.join("bin").join("youki");
        if inputs.runtime_path == runtime_launcher {
            return Ok(());
        }
    }

    Err(OciSetupDiagnostic::new(
        "packaged OCI bundle and runtime paths must be package-resolved under /nix/store or a matching portable release root",
    ))
}

fn portable_release_root_for_bundle(bundle_path: &Path) -> Option<&Path> {
    let nix_dir = bundle_path.ancestors().find_map(|ancestor| {
        let nix_dir = ancestor.parent()?;
        (ancestor.file_name().is_some_and(|name| name == "store")
            && nix_dir.file_name().is_some_and(|name| name == "nix"))
        .then_some(nix_dir)
    })?;

    let release_root = nix_dir.parent()?;
    (release_root != Path::new("/")).then_some(release_root)
}

fn validate_packaged_path_shape(
    kind: &str,
    path: &Path,
) -> std::result::Result<(), OciSetupDiagnostic> {
    if path.as_os_str().is_empty() {
        return Err(OciSetupDiagnostic::new(format!(
            "packaged OCI {kind} path is missing or empty"
        )));
    }

    if !path.is_absolute() {
        return Err(OciSetupDiagnostic::new(format!(
            "packaged OCI {kind} path must be absolute and package-resolved under {PACKAGED_NIX_STORE_PREFIX}"
        )));
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(OciSetupDiagnostic::new(format!(
            "packaged OCI {kind} path must not contain traversal components"
        )));
    }

    Ok(())
}

fn prepare_embedded_oci_gateway_from_env_values(
    values: EmbeddedOciGatewayEnvValues<'_>,
    launch: impl FnOnce(
        EmbeddedOciGatewayInputs<'_>,
    ) -> std::result::Result<GatewayLaunchOutcome, OciSetupDiagnostic>,
) -> std::result::Result<GatewayLaunchOutcome, OciSetupDiagnostic> {
    let bundle_path = values.bundle_path.ok_or_else(|| {
        OciSetupDiagnostic::new("AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH is required")
    })?;
    let runtime_path = values.runtime_path.ok_or_else(|| {
        OciSetupDiagnostic::new("AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH is required")
    })?;

    prepare_embedded_oci_gateway_with_inputs(
        EmbeddedOciGatewayInputs {
            bundle_path: Path::new(bundle_path),
            runtime_path: Path::new(runtime_path),
        },
        launch,
    )
}

fn inner_gateway_process_env_from_lookup(
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> std::result::Result<Vec<(String, String)>, OciSetupDiagnostic> {
    let mut process_env = STATIC_INNER_GATEWAY_ENV
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect::<Vec<_>>();

    for name in INNER_GATEWAY_ENV_ALLOWLIST {
        let value = lookup(name).filter(|value| !value.trim().is_empty());
        if REQUIRED_INNER_GATEWAY_ENV.contains(name) && value.is_none() {
            return Err(OciSetupDiagnostic::new(format!(
                "{name} is required for staged OCI inner gateway config"
            )));
        }
        if let Some(value) = value {
            set_process_env_entry(&mut process_env, name, value);
        }
    }

    set_process_env_entry(
        &mut process_env,
        "AR_GATEWAY_EXTERNAL_ISOLATION",
        "container".to_string(),
    );

    Ok(process_env)
}

fn set_process_env_entry(process_env: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some((_, existing_value)) = process_env
        .iter_mut()
        .find(|(existing_name, _)| existing_name == name)
    {
        *existing_value = value;
    } else {
        process_env.push((name.to_string(), value));
    }
}

fn staged_oci_config_with_process_env(
    mut config: serde_json::Value,
    process_env: &[(String, String)],
) -> std::result::Result<serde_json::Value, OciSetupDiagnostic> {
    let Some(process) = config
        .get_mut("process")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Err(OciSetupDiagnostic::new(
            "packaged OCI config is missing a process object",
        ));
    };

    process.insert(
        "env".to_string(),
        serde_json::Value::Array(
            process_env
                .iter()
                .map(|(name, value)| serde_json::Value::String(format!("{name}={value}")))
                .collect(),
        ),
    );

    Ok(config)
}

fn stage_embedded_oci_gateway_bundle_at_path(
    packaged_bundle: &Path,
    staged_bundle: &Path,
    process_env: &[(String, String)],
) -> std::result::Result<PathBuf, OciSetupDiagnostic> {
    let packaged_rootfs = packaged_bundle.join("rootfs");
    if !packaged_rootfs.is_dir() {
        return Err(OciSetupDiagnostic::new(
            "packaged OCI bundle rootfs is missing",
        ));
    }

    fs::create_dir(staged_bundle).map_err(|error| {
        OciSetupDiagnostic::new(format!(
            "staged OCI bundle directory could not be created: {}",
            error.kind()
        ))
    })?;
    restrict_stage_directory(staged_bundle)?;
    link_packaged_rootfs(&packaged_rootfs, &staged_bundle.join("rootfs"))?;

    let packaged_config =
        fs::read_to_string(packaged_bundle.join("config.json")).map_err(|error| {
            OciSetupDiagnostic::new(format!(
                "packaged OCI config could not be read: {}",
                error.kind()
            ))
        })?;
    let config = serde_json::from_str::<serde_json::Value>(&packaged_config)
        .map_err(|_error| OciSetupDiagnostic::new("packaged OCI config could not be parsed"))?;
    let staged_config = staged_oci_config_with_process_env(config, process_env)?;
    let staged_config = serde_json::to_vec_pretty(&staged_config)
        .map_err(|_error| OciSetupDiagnostic::new("staged OCI config could not be serialized"))?;

    fs::write(staged_bundle.join("config.json"), staged_config).map_err(|error| {
        OciSetupDiagnostic::new(format!(
            "staged OCI config could not be written: {}",
            error.kind()
        ))
    })?;

    Ok(staged_bundle.to_path_buf())
}

#[cfg(unix)]
fn restrict_stage_directory(path: &Path) -> std::result::Result<(), OciSetupDiagnostic> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        OciSetupDiagnostic::new(format!(
            "staged OCI bundle permissions could not be restricted: {}",
            error.kind()
        ))
    })
}

#[cfg(not(unix))]
fn restrict_stage_directory(_path: &Path) -> std::result::Result<(), OciSetupDiagnostic> {
    Err(OciSetupDiagnostic::new(
        "embedded OCI gateway launcher requires Unix staging permissions",
    ))
}

#[cfg(unix)]
fn link_packaged_rootfs(
    source: &Path,
    destination: &Path,
) -> std::result::Result<(), OciSetupDiagnostic> {
    std::os::unix::fs::symlink(source, destination).map_err(|error| {
        OciSetupDiagnostic::new(format!(
            "packaged OCI rootfs could not be linked into staged bundle: {}",
            error.kind()
        ))
    })
}

#[cfg(not(unix))]
fn link_packaged_rootfs(
    _source: &Path,
    _destination: &Path,
) -> std::result::Result<(), OciSetupDiagnostic> {
    Err(OciSetupDiagnostic::new(
        "embedded OCI gateway launcher requires Unix rootfs staging",
    ))
}

fn unique_oci_stage_bundle_path() -> PathBuf {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);

    env::temp_dir().join(format!(
        "auto-review-oci-bundle-{}-{timestamp}",
        std::process::id()
    ))
}

fn stage_embedded_oci_gateway_bundle(
    packaged_bundle: &Path,
    process_env: &[(String, String)],
) -> std::result::Result<PathBuf, OciSetupDiagnostic> {
    stage_embedded_oci_gateway_bundle_at_path(
        packaged_bundle,
        &unique_oci_stage_bundle_path(),
        process_env,
    )
}

fn build_packaged_oci_runtime_command(
    runtime_path: &Path,
    bundle_path: &Path,
) -> PackagedOciRuntimeCommand {
    PackagedOciRuntimeCommand {
        program: runtime_path.to_path_buf(),
        args: vec![
            PathBuf::from("run"),
            PathBuf::from("--bundle"),
            bundle_path.to_path_buf(),
            PathBuf::from("auto-review-gateway"),
        ],
        clear_ambient_env: true,
        env: Vec::new(),
    }
}

fn execute_packaged_oci_runtime_with_executor(
    inputs: EmbeddedOciGatewayInputs<'_>,
    executor: impl FnOnce(PackagedOciRuntimeCommand) -> std::result::Result<(), OciSetupDiagnostic>,
) -> std::result::Result<GatewayLaunchOutcome, OciSetupDiagnostic> {
    executor(build_packaged_oci_runtime_command(
        inputs.runtime_path,
        inputs.bundle_path,
    ))
    .map_err(|_diagnostic| {
        OciSetupDiagnostic::new("packaged OCI runtime failed while starting the inner gateway")
    })?;

    Ok(GatewayLaunchOutcome::OuterLauncherFinished)
}

fn execute_packaged_oci_runtime_with_staged_bundle(
    inputs: EmbeddedOciGatewayInputs<'_>,
    process_env: &[(String, String)],
    executor: impl FnOnce(PackagedOciRuntimeCommand) -> std::result::Result<(), OciSetupDiagnostic>,
) -> std::result::Result<GatewayLaunchOutcome, OciSetupDiagnostic> {
    let staged_bundle = stage_embedded_oci_gateway_bundle(inputs.bundle_path, process_env)?;
    let outcome = execute_packaged_oci_runtime_with_executor(
        EmbeddedOciGatewayInputs {
            bundle_path: &staged_bundle,
            runtime_path: inputs.runtime_path,
        },
        executor,
    );

    if let Err(error) = fs::remove_dir_all(&staged_bundle) {
        tracing::warn!(
            error = %error.kind(),
            "staged OCI bundle cleanup failed after runtime exit"
        );
    }

    outcome
}

fn run_packaged_oci_runtime_command(
    command: PackagedOciRuntimeCommand,
) -> std::result::Result<(), OciSetupDiagnostic> {
    let mut process = ProcessCommand::new(&command.program);
    process.args(&command.args);

    if command.clear_ambient_env {
        process.env_clear();
    }
    process.envs(command.env);

    let status = process.status().map_err(|error| {
        OciSetupDiagnostic::new(format!(
            "packaged OCI runtime could not be started: {}",
            error.kind()
        ))
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(OciSetupDiagnostic::new(format!(
            "packaged OCI runtime exited before starting the inner gateway: {status}"
        )))
    }
}

fn prepare_embedded_oci_gateway() -> std::result::Result<GatewayLaunchOutcome, OciSetupDiagnostic> {
    let bundle_path = env::var("AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH").ok();
    let runtime_path = env::var("AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH").ok();
    let process_env = inner_gateway_process_env_from_lookup(|name| env::var(name).ok())?;

    prepare_embedded_oci_gateway_from_env_values(
        EmbeddedOciGatewayEnvValues {
            bundle_path: bundle_path.as_deref(),
            runtime_path: runtime_path.as_deref(),
        },
        |inputs| {
            execute_packaged_oci_runtime_with_staged_bundle(
                inputs,
                &process_env,
                run_packaged_oci_runtime_command,
            )
        },
    )
}

impl GatewayStartupConfig {
    fn from_env_values(values: GatewayStartupEnvValues<'_>) -> Result<Self> {
        let llm_reasoning_model = values
            .llm_reasoning_model
            .unwrap_or("qwen2.5-coder:32b")
            .to_string();

        if llm_reasoning_model.trim().is_empty() {
            anyhow::bail!(
                "LLM_REASONING_MODEL is set to an empty/whitespace value; \
                 unset it to take the default (qwen2.5-coder:32b) or set \
                 a real model name"
            );
        }

        Ok(Self {
            bind: values.bind.unwrap_or("0.0.0.0:8080").to_string(),
            webhook_secret: values
                .webhook_secret
                .context("WEBHOOK_SECRET is required")?
                .to_string(),
            forgejo_base_url: values
                .forgejo_base_url
                .context("FORGEJO_BASE_URL is required")?
                .to_string(),
            ar_forgejo_token: values
                .ar_forgejo_token
                .context("AR_FORGEJO_TOKEN is required")?
                .to_string(),
            llm_base_url: values
                .llm_base_url
                .context("LLM_BASE_URL is required")?
                .to_string(),
            llm_reasoning_model,
        })
    }
}

pub async fn run_from_env(options: StartupOptions) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ar_gateway=debug")),
        )
        .try_init()
        .ok();

    let bare_env = env::var("AR_GATEWAY_BARE").ok();
    let external_isolation_env = env::var("AR_GATEWAY_EXTERNAL_ISOLATION").ok();
    let launch_outcome = select_gateway_launcher_for_startup_options(
        options,
        GatewayLauncherEnvValues {
            bare: bare_env.as_deref(),
            external_isolation: external_isolation_env.as_deref(),
        },
        prepare_embedded_oci_gateway,
    )?;

    if matches!(launch_outcome, GatewayLaunchOutcome::OuterLauncherFinished) {
        return Ok(());
    }

    let bind_env = env::var("AR_GATEWAY_BIND").ok();
    let webhook_secret_env = env::var("WEBHOOK_SECRET").ok();
    let forgejo_base_env = env::var("FORGEJO_BASE_URL").ok();
    let forgejo_token_env = read_non_empty_env("AR_FORGEJO_TOKEN");
    let llm_base_env = env::var("LLM_BASE_URL").ok();
    let reasoning_model_env = env::var("LLM_REASONING_MODEL").ok();
    let startup_config = GatewayStartupConfig::from_env_values(GatewayStartupEnvValues {
        bind: bind_env.as_deref(),
        webhook_secret: webhook_secret_env.as_deref(),
        forgejo_base_url: forgejo_base_env.as_deref(),
        ar_forgejo_token: forgejo_token_env.as_deref(),
        llm_base_url: llm_base_env.as_deref(),
        llm_reasoning_model: reasoning_model_env.as_deref(),
    })?;

    // git is required for the workspace clone phase. Probe up
    // front so a missing-git deploy surfaces in the first log
    // scrape rather than the first failed review's opaque
    // "No such file or directory" io error. Don't bail — the
    // gateway should still serve /healthz and /metrics for
    // operators investigating; reviews just fail loudly per-PR.
    match tokio::process::Command::new("git")
        .arg("--version")
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            tracing::info!(
                version = %String::from_utf8_lossy(&out.stdout).trim(),
                "git OK"
            );
        }
        Ok(out) => {
            tracing::warn!(
                status = %out.status,
                "git --version exited non-zero; reviews will fail at the clone phase"
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "git not found in PATH; every review will fail at prepare_workspace. \
                 Install git or add it to PATH."
            );
        }
    }

    let secret = startup_config.webhook_secret;
    // Forgejo's webhook docs recommend a strong random secret; HMAC-
    // SHA256 with a short shared key is brute-forceable. Warn at
    // startup rather than at first verify so operators see this in
    // their first log scrape, not a production failure window.
    if secret.len() < 16 {
        tracing::warn!(
            length = secret.len(),
            "WEBHOOK_SECRET is shorter than 16 bytes; HMAC verification \
             will work but the secret is weakly resistant to brute-force \
             attack. Recommend 32+ random bytes (e.g. `openssl rand -hex 32`)"
        );
    }
    let forgejo_base = startup_config.forgejo_base_url;
    let forgejo_token = startup_config.ar_forgejo_token;
    let llm_base = startup_config.llm_base_url;
    let llm_api_key = env::var("LLM_API_KEY").ok();
    let reasoning_model = startup_config.llm_reasoning_model;

    // Bot identity: read once and validate up-front so the poller
    // and the chat handler see the same values. AR_BOT_LOGIN gates
    // self-loop detection (`is_bot_self`); an empty value would
    // never match any Forgejo sender and the bot would reply to
    // its own comments — a real loop bomb. AR_BOT_NAME is the
    // mention parser's `@<name>` token; an empty value would match
    // every `@` and fire on every PR thread mention, also bad.
    let bot_login = match env::var("AR_BOT_LOGIN") {
        Ok(v) if v.trim().is_empty() => {
            anyhow::bail!(
                "AR_BOT_LOGIN is set to an empty/whitespace value; \
                 unset it to take the default (`auto_review`) or set \
                 the bot's actual Forgejo login"
            );
        }
        Ok(v) => v,
        Err(_) => "auto_review".to_string(),
    };
    let bot_name = match env::var("AR_BOT_NAME") {
        Ok(v) if v.trim().is_empty() => {
            anyhow::bail!(
                "AR_BOT_NAME is set to an empty/whitespace value; \
                 unset it to inherit AR_BOT_LOGIN or set the @-handle \
                 users mention"
            );
        }
        Ok(v) => v,
        Err(_) => bot_login.clone(),
    };

    let forgejo =
        Arc::new(ForgejoClient::new(&forgejo_base, &forgejo_token).context("forgejo client")?);

    let reasoning_provider = Arc::new(
        OpenAiProvider::new(&llm_base, llm_api_key.as_deref(), &reasoning_model)
            .context("reasoning LLM provider")?,
    );
    let mut router = LlmRouter::new().with(ModelTier::Reasoning, reasoning_provider);

    // Optional Embedding tier — when configured, the orchestrator
    // builds a RAG context from the cloned workspace and injects
    // it into the LLM prompt. Reuses LLM_BASE_URL + LLM_API_KEY by
    // default; override with LLM_EMBEDDING_BASE_URL / _API_KEY when
    // your embedder lives on a different endpoint.
    if let Some(embedding_model) = read_non_empty_env("LLM_EMBEDDING_MODEL") {
        let embed_base =
            read_non_empty_env("LLM_EMBEDDING_BASE_URL").unwrap_or_else(|| llm_base.clone());
        let embed_key = read_non_empty_env("LLM_EMBEDDING_API_KEY").or_else(|| llm_api_key.clone());
        let mut provider = OpenAiProvider::new(&embed_base, embed_key.as_deref(), &embedding_model)
            .context("embedding LLM provider")?;
        provider = provider.with_embedding_model(&embedding_model);
        // For Ollama-backed embedders, explicitly send options.num_ctx
        // so a bigger byte cap doesn't get silently truncated by the
        // server's default 2048. Ignored by hosted OpenAI.
        if let Some(num_ctx) = parse_env::<u32>("AR_EMBED_NUM_CTX") {
            provider = provider.with_embed_num_ctx(num_ctx);
            tracing::info!(num_ctx, "embedding num_ctx override enabled");
        }
        let provider = Arc::new(provider);
        router = router.with(ModelTier::Embedding, provider);
        tracing::info!(model = %embedding_model, "embedding tier configured; RAG enabled");
    } else {
        tracing::info!("LLM_EMBEDDING_MODEL not set; RAG disabled");
    }

    // Optional Cheap tier — used by the LLM-driven file triage step.
    if let Some(cheap_model) = read_non_empty_env("LLM_CHEAP_MODEL") {
        let cheap_base =
            read_non_empty_env("LLM_CHEAP_BASE_URL").unwrap_or_else(|| llm_base.clone());
        let cheap_key = read_non_empty_env("LLM_CHEAP_API_KEY").or_else(|| llm_api_key.clone());
        let provider = Arc::new(
            OpenAiProvider::new(&cheap_base, cheap_key.as_deref(), &cheap_model)
                .context("cheap LLM provider")?,
        );
        router = router.with(ModelTier::Cheap, provider);
        tracing::info!(model = %cheap_model, "cheap tier configured; LLM triage enabled");
    } else {
        tracing::info!("LLM_CHEAP_MODEL not set; LLM triage disabled (heuristic only)");
    }

    let llm_router = Arc::new(router);

    // Single shared learnings store: writes from the chat handler
    // (remember/forget) become visible to RAG retrieval in subsequent
    // reviews. Persistent SQLite by default (at the per-store XDG path);
    // operators opt out with `AR_LEARNINGS_DB=:memory:` or override
    // the path with `AR_LEARNINGS_DB=/path/to/learnings.db`.
    let learnings_backing = resolve_db_backing(
        env::var("AR_LEARNINGS_DB").ok().as_deref(),
        &default_state_path("learnings.db"),
    );
    let (learnings, learnings_info) = match &learnings_backing {
        DbBacking::Sqlite(path) => {
            ensure_parent_dir(path).with_context(|| {
                format!("create parent dir for learnings db at {}", path.display())
            })?;
            let store = SqliteLearningsStore::open(path)
                .await
                .with_context(|| format!("open learnings db at {}", path.display()))?;
            tracing::info!(path = %path.display(), "learnings store: SQLite (persistent)");
            (
                Arc::new(store) as Arc<dyn ar_index::LearningsStore>,
                format!("sqlite:{}", path.display()),
            )
        }
        DbBacking::InMemory => {
            tracing::info!("learnings store: in-memory (AR_LEARNINGS_DB=:memory: opt-out)");
            (
                Arc::new(InMemoryLearningsStore::new()) as Arc<dyn ar_index::LearningsStore>,
                "in-memory".to_string(),
            )
        }
    };

    // Shared review history. Both the orchestrator's incremental-
    // review dedup AND the chat poller need to enumerate the PRs
    // we've reviewed; constructing one Arc and threading it through
    // both keeps them consistent. Persistent SQLite by default;
    // `AR_HISTORY_DB=:memory:` opts out (every restart triggers a
    // fresh full review on the next webhook for any open PR).
    let history_backing = resolve_db_backing(
        env::var("AR_HISTORY_DB").ok().as_deref(),
        &default_state_path("history.db"),
    );
    let (history, history_info) = match &history_backing {
        DbBacking::Sqlite(path) => {
            ensure_parent_dir(path).with_context(|| {
                format!("create parent dir for history db at {}", path.display())
            })?;
            let store = SqliteReviewHistory::open(path)
                .await
                .with_context(|| format!("open history db at {}", path.display()))?;
            tracing::info!(path = %path.display(), "review history: SQLite (persistent)");
            (
                Arc::new(store) as Arc<dyn ReviewHistory>,
                format!("sqlite:{}", path.display()),
            )
        }
        DbBacking::InMemory => {
            tracing::info!("review history: in-memory (AR_HISTORY_DB=:memory: opt-out)");
            (
                Arc::new(InMemoryReviewHistory::new()) as Arc<dyn ReviewHistory>,
                "in-memory".to_string(),
            )
        }
    };

    // Shared symbol-embedding store. Persistent SQLite by default
    // so symbol embeddings survive across reviews (and across gateway
    // restarts). `AR_VECTOR_DB=:memory:` opts out — useful for tests
    // and ephemeral previews where re-embedding on each review is
    // acceptable. The wins matter most for the slow local Ollama
    // embedder; hosted OpenAI is fast enough that operators may not
    // bother with persistence.
    let vector_backing = resolve_db_backing(
        env::var("AR_VECTOR_DB").ok().as_deref(),
        &default_state_path("vector.db"),
    );
    let (vector_store, vector_info) = match &vector_backing {
        DbBacking::Sqlite(path) => {
            ensure_parent_dir(path).with_context(|| {
                format!("create parent dir for vector db at {}", path.display())
            })?;
            let store = SqliteVectorStore::open(path)
                .await
                .with_context(|| format!("open vector db at {}", path.display()))?;
            tracing::info!(path = %path.display(), "vector store: SQLite (persistent)");
            (
                Arc::new(store) as Arc<dyn VectorStore>,
                format!("sqlite:{}", path.display()),
            )
        }
        DbBacking::InMemory => {
            tracing::info!("vector store: in-memory (AR_VECTOR_DB=:memory: opt-out)");
            (
                Arc::new(InMemoryVectorStore::new()) as Arc<dyn VectorStore>,
                "in-memory".to_string(),
            )
        }
    };

    // Webhook delivery dedup. Persistent SQLite by default; operators
    // opt out of persistence with `AR_DEDUP_DB=:memory:` (in-memory
    // LRU bounded by `AR_DEDUP_CAPACITY`, default 256), or disable
    // dedup entirely with `AR_DEDUP_CAPACITY=0` (mostly for tests
    // that want every well-signed delivery dispatched). Computed
    // upfront so the chosen backing lands in /info alongside the
    // others, even though the actual `with_webhook_dedup` call
    // happens further down once `state` exists.
    let dedup_capacity = parse_env::<usize>("AR_DEDUP_CAPACITY").unwrap_or(256);
    let dedup_backing = resolve_db_backing(
        env::var("AR_DEDUP_DB").ok().as_deref(),
        &default_state_path("dedup.db"),
    );
    let (dedup_store, dedup_info): (Option<Arc<dyn DeliveryDedup>>, String) = if dedup_capacity == 0
    {
        tracing::info!("webhook delivery dedup: disabled (AR_DEDUP_CAPACITY=0)");
        (None, "disabled".into())
    } else {
        match &dedup_backing {
            DbBacking::Sqlite(path) => {
                ensure_parent_dir(path).with_context(|| {
                    format!("create parent dir for dedup db at {}", path.display())
                })?;
                let store = SqliteDeliveries::open(path)
                    .await
                    .with_context(|| format!("open dedup db at {}", path.display()))?;
                tracing::info!(path = %path.display(), "webhook delivery dedup: SQLite (persistent)");
                (
                    Some(Arc::new(store) as Arc<dyn DeliveryDedup>),
                    format!("sqlite:{}", path.display()),
                )
            }
            DbBacking::InMemory => {
                let store = RecentDeliveries::new(dedup_capacity);
                tracing::info!(
                    capacity = dedup_capacity,
                    "webhook delivery dedup: in-memory LRU (AR_DEDUP_DB=:memory: opt-out)"
                );
                (
                    Some(Arc::new(store) as Arc<dyn DeliveryDedup>),
                    format!("in-memory(capacity={dedup_capacity})"),
                )
            }
        }
    };

    let metrics = Arc::new(Metrics::new());
    let observer: Arc<dyn ar_orchestrator::ReviewObserver> =
        Arc::new(MetricsObserver::new(metrics.clone()));

    let mut dispatcher_builder = SpawningDispatcher::new(
        forgejo.clone(),
        llm_router.clone(),
        forgejo_base.clone(),
        forgejo_token.clone(),
    )
    .with_history(history.clone())
    .with_learnings(learnings.clone())
    .with_vector_store(vector_store.clone())
    .with_observer(observer);

    // Optional concurrency cap on in-flight reviews. Without this,
    // a burst of N PRs spawns N tmpdirs + N in-flight LLM calls.
    // For high-traffic instances or expensive cloud LLMs the
    // operator wants a cap; small deployments leave it unset.
    if let Some(max) = parse_env::<usize>("AR_REVIEW_CONCURRENCY") {
        dispatcher_builder = dispatcher_builder.with_concurrency_limit(max);
        tracing::info!(max, "review concurrency cap enabled");
    }

    let dispatcher = Arc::new(dispatcher_builder);

    // Background poller for inline review-thread `@auto_review`
    // mentions. Forgejo doesn't fire pull_request_review_comment
    // webhooks reliably for thread replies (gitea#26023), so we
    // poll. Disabled when AR_POLL_INTERVAL_SECS=0.
    let poll_interval_secs =
        parse_env::<u64>("AR_POLL_INTERVAL_SECS").unwrap_or(DEFAULT_POLL_INTERVAL.as_secs());
    let chat_comment_cursors: SharedCommentCursors = Arc::new(Mutex::new(HashMap::new()));
    if poll_interval_secs > 0 {
        let dispatcher_dyn: Arc<dyn ar_orchestrator::JobDispatcher> = dispatcher.clone();
        ChatPoller::new(
            forgejo.clone(),
            llm_router.clone(),
            learnings.clone(),
            history.clone(),
            dispatcher_dyn,
            bot_login.clone(),
            bot_name.clone(),
        )
        .with_cursors(chat_comment_cursors.clone())
        .with_metrics(metrics.clone())
        .spawn(Duration::from_secs(poll_interval_secs));
        tracing::info!(
            interval_secs = poll_interval_secs,
            bot_login = %bot_login,
            bot_name = %bot_name,
            "chat poller running"
        );
    } else {
        tracing::info!("AR_POLL_INTERVAL_SECS=0; chat poller disabled");
    }

    let chat_deps = ChatDeps {
        forgejo: forgejo.clone(),
        llm: llm_router.clone(),
        learnings,
    };

    // Wire the readiness probe to the same Forgejo client the chat
    // handler uses. The TTL (default 10s) is tuneable via env so
    // operators with aggressive k8s probe schedules can lengthen it
    // to avoid hammering Forgejo.
    let readiness_ttl_secs = parse_env::<u64>("AR_READINESS_TTL_SECS").unwrap_or(10);
    let readiness = Arc::new(ReadinessProbe::with_ttl(
        forgejo.clone(),
        Duration::from_secs(readiness_ttl_secs),
    ));
    let runtime_isolation = RuntimeIsolationPostureInfo::from(classify_runtime_isolation_posture(
        RuntimeIsolationPostureInput {
            bare: bare_env.as_deref(),
            external_isolation: external_isolation_env.as_deref(),
            oci_setup_diagnostic: None,
            target_os: env::consts::OS,
        },
    )?);
    tracing::info!(
        kind = runtime_isolation.kind,
        label = %runtime_isolation.label,
        detail = %runtime_isolation.detail,
        "runtime isolation posture classified"
    );

    // Snapshot the runtime config for /info. Read env-var-driven
    // booleans here once rather than threading them through every
    // builder call.
    let info = Arc::new(GatewayInfo {
        name: "auto_review",
        version: env!("CARGO_PKG_VERSION"),
        bot_login: bot_login.clone(),
        bot_name: bot_name.clone(),
        learnings: learnings_info.clone(),
        history: history_info.clone(),
        vector: vector_info.clone(),
        dedup: dedup_info.clone(),
        llm_tiers: {
            let mut tiers = vec!["reasoning"]; // always present (required)
            if read_non_empty_env("LLM_CHEAP_MODEL").is_some() {
                tiers.push("cheap");
            }
            if read_non_empty_env("LLM_EMBEDDING_MODEL").is_some() {
                tiers.push("embedding");
            }
            tiers
        },
        reasoning_model: reasoning_model.clone(),
        poller_enabled: poll_interval_secs > 0,
        readiness_enabled: true,
        runtime_isolation,
    });

    let mut state = AppState::new(secret, dispatcher)
        .with_chat(chat_deps)
        .with_bot_identity(bot_login, bot_name)
        .with_metrics(metrics)
        .with_readiness(readiness)
        .with_chat_comment_cursors(chat_comment_cursors)
        .with_info(info);

    if let Some(action_token) = validate_ci_review_token(read_non_empty_env("AR_CI_REVIEW_TOKEN"))?
    {
        state = state.with_ci_review_endpoint(action_token, forgejo.clone());
        tracing::info!("CI review endpoint enabled at POST /reviews/ci");
    } else {
        tracing::info!("CI review endpoint disabled (AR_CI_REVIEW_TOKEN unset)");
    }

    if let Some(dedup) = dedup_store {
        state = state.with_webhook_dedup(dedup);
    }

    // Single-line summary of the four persistence backings, so an
    // operator can confirm at startup which file the bot opened
    // (or that everything's volatile) without diffing four
    // separate lines above.
    tracing::info!(
        learnings = %learnings_info,
        history = %history_info,
        vector = %vector_info,
        dedup = %dedup_info,
        "persistence backings selected",
    );

    // Optional global webhook throttle (T7 mitigation). Off by
    // default so existing deployments don't suddenly start
    // shedding traffic; operators opt in by setting both env
    // vars. The intended values for a self-host fronting a single
    // Forgejo instance are tens of req/s and a burst around 30 —
    // legitimate Forgejo traffic is well under that.
    let rate_per_sec = parse_env::<u32>("AR_WEBHOOK_RATE_PER_SEC");
    let burst = parse_env::<u32>("AR_WEBHOOK_BURST");
    match (rate_per_sec, burst) {
        (Some(rate), Some(burst)) => {
            let bucket = Arc::new(TokenBucket::new(burst, rate));
            state = state.with_webhook_rate_limit(bucket);
            tracing::info!(rate, burst, "webhook rate limiter enabled");
        }
        // Only one half set — operator probably meant to enable
        // the limiter but missed the partner var. Without this
        // warning the rate limit is silently off and the operator
        // discovers it during an incident.
        (Some(_), None) => {
            tracing::warn!(
                "AR_WEBHOOK_RATE_PER_SEC is set but AR_WEBHOOK_BURST is not; \
                 rate limiter requires both — DISABLED. Set both or unset both."
            );
        }
        (None, Some(_)) => {
            tracing::warn!(
                "AR_WEBHOOK_BURST is set but AR_WEBHOOK_RATE_PER_SEC is not; \
                 rate limiter requires both — DISABLED. Set both or unset both."
            );
        }
        (None, None) => {} // intentional: no rate limit
    }

    let app = build_router(state);

    let listener = TcpListener::bind(&startup_config.bind)
        .await
        .with_context(|| format!("bind {}", startup_config.bind))?;
    let bind = startup_config.bind;
    tracing::info!(%bind, "ar-gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("graceful shutdown complete");
    Ok(())
}

/// Shutdown signal handler. Returns when SIGTERM (Unix) or
/// SIGINT (Ctrl-C, cross-platform) arrives. Used as the
/// `with_graceful_shutdown` argument on `axum::serve` so:
/// - in-flight HTTP responses finish cleanly,
/// - the listener stops accepting new connections immediately,
/// - the process exits 0 once the listener drains.
///
/// Note: review tasks the dispatcher has already `tokio::spawn`-ed
/// continue running after the listener drains, since they're not
/// joined on. The tokio runtime drops them when `main` returns.
/// This is best-effort by design — adding a join set across the
/// dispatcher boundary would mean threading a CancellationToken
/// through every spawned activity, which is more machinery than
/// the single-tenant deploy needs. Operators wanting zero data
/// loss should drain via the systemd `ExecStop=` hook with a
/// short pre-stop sleep before SIGTERM, so in-flight reviews
/// reach their commit-status post.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "ctrl_c handler failed; shutdown trigger disabled");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "SIGTERM handler init failed");
                    return;
                }
            };
        term.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("received SIGINT; draining listener");
        }
        _ = terminate => {
            tracing::info!("received SIGTERM; draining listener");
        }
    }
}

/// Read an env var, treating both "unset" and "empty / whitespace-only"
/// as `None`. Most operator-facing env vars take a meaningful default
/// when unset; an explicit empty assignment (`FOO=`) is almost always
/// a misconfiguration that should fall through to the same default
/// rather than silently producing a broken empty string.
/// Compute the per-store XDG default sqlite path for `filename`. Thin
/// wrapper around the pure [`compose_state_path`] so unit tests don't
/// have to mutate the process env.
fn default_state_path(filename: &str) -> PathBuf {
    let xdg = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let home = env::var_os("HOME").map(PathBuf::from);
    compose_state_path(xdg.as_deref(), home.as_deref(), filename)
}

/// Create the parent directory of `path` if it doesn't exist. The
/// SQLite stores' `open()` would otherwise fail with a confusing
/// "unable to open database file" on first run when the XDG state
/// dir is missing. `create_dir_all` is idempotent.
fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn read_non_empty_env(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(v) if v.trim().is_empty() => {
            tracing::warn!(
                env = name,
                "env var set to an empty/whitespace value; treating as unset"
            );
            None
        }
        Ok(v) => Some(v),
        Err(_) => None,
    }
}

fn validate_ci_review_token(raw: Option<String>) -> Result<Option<String>> {
    let Some(token) = raw else {
        return Ok(None);
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        return Ok(None);
    }
    if token.len() < 32 {
        anyhow::bail!(
            "AR_CI_REVIEW_TOKEN is too short; configure a strong token of at least 32 characters"
        );
    }
    Ok(Some(token))
}

/// Parse an env var as an integer, distinguishing "unset" from
/// "set but unparseable". The previous `.parse::<X>().ok()` pattern
/// silently swallowed garbage values like `AR_REVIEW_CONCURRENCY=ten`,
/// leaving the operator with no signal that their config didn't take
/// effect. This warn-and-fall-through variant surfaces the typo.
fn parse_env<T>(name: &str) -> Option<T>
where
    T: std::str::FromStr,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    let raw = read_non_empty_env(name)?;
    match raw.parse::<T>() {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                env = name,
                value = %raw,
                error = %e,
                "env var set to an unparseable value; using the built-in default"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn run_from_env_source() -> &'static str {
        let source = include_str!("startup.rs");
        let start = source
            .find("pub async fn run_from_env(options: StartupOptions)")
            .unwrap_or_else(|| panic!("startup.rs should define run_from_env with StartupOptions"));
        let end = source[start..]
            .find("fn validate_ci_review_token")
            .map(|offset| start + offset)
            .unwrap_or_else(|| {
                panic!("run_from_env source should precede validate_ci_review_token")
            });

        &source[start..end]
    }

    struct CapturingSubscriber {
        messages: Arc<Mutex<Vec<String>>>,
    }

    impl tracing::Subscriber for CapturingSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = MessageVisitor::default();
            event.record(&mut visitor);
            if let Some(message) = visitor.message {
                self.messages.lock().unwrap().push(message);
            }
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    #[derive(Default)]
    struct MessageVisitor {
        message: Option<String>,
    }

    impl tracing::field::Visit for MessageVisitor {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "message" {
                self.message = Some(value.to_string());
            }
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = Some(format!("{value:?}").trim_matches('"').to_string());
            }
        }
    }

    #[test]
    fn run_from_env_wires_startup_options_and_bare_env_to_launcher_before_gateway_config() {
        let source = run_from_env_source();
        let selector = source
            .find("select_gateway_launcher_for_startup_options")
            .unwrap_or_else(|| {
                panic!(
                    "run_from_env must call select_gateway_launcher_for_startup_options before normal startup"
                )
            });
        let config = source
            .find("GatewayStartupConfig::from_env_values")
            .unwrap_or_else(|| panic!("run_from_env should continue through GatewayStartupConfig"));

        assert!(
            selector < config,
            "run_from_env must select the launcher before normal gateway config/startup"
        );

        let launcher_wiring = &source[..config];

        assert!(
            launcher_wiring.contains("AR_GATEWAY_BARE"),
            "run_from_env must read AR_GATEWAY_BARE before selecting the launcher"
        );
        assert!(
            launcher_wiring.contains("AR_GATEWAY_EXTERNAL_ISOLATION"),
            "run_from_env must read AR_GATEWAY_EXTERNAL_ISOLATION before selecting the launcher"
        );
        assert!(
            launcher_wiring[selector..].contains("options"),
            "run_from_env must pass its StartupOptions into the launcher selector"
        );
        assert!(
            launcher_wiring[selector..].contains("GatewayLauncherEnvValues"),
            "run_from_env must pass GatewayLauncherEnvValues into the launcher selector"
        );
        assert!(
            launcher_wiring[selector..].contains("bare:"),
            "run_from_env must wire the AR_GATEWAY_BARE value into GatewayLauncherEnvValues::bare"
        );
        assert!(
            launcher_wiring[selector..].contains("external_isolation:"),
            "run_from_env must wire AR_GATEWAY_EXTERNAL_ISOLATION into GatewayLauncherEnvValues::external_isolation"
        );
    }

    #[test]
    fn ci_review_token_unset_empty_or_whitespace_disables_endpoint() {
        for raw in [None, Some(""), Some("   \t\n  ")] {
            let validated = validate_ci_review_token(raw.map(str::to_string));

            assert!(
                matches!(validated, Ok(None)),
                "expected {raw:?} to disable the CI review endpoint, got {validated:?}"
            );
        }
    }

    #[test]
    fn ci_review_token_accepts_strong_random_value() {
        let token = "0123456789abcdef0123456789abcdef".to_string();

        let validated = validate_ci_review_token(Some(token.clone()));

        assert_eq!(validated.unwrap(), Some(token));
    }

    #[test]
    fn ci_review_token_rejects_short_non_empty_value() {
        let rejected_token = "abc123-token-value";
        let err = validate_ci_review_token(Some(rejected_token.to_string())).unwrap_err();
        let message = err.to_string();

        assert!(
            message.contains("AR_CI_REVIEW_TOKEN"),
            "error should name AR_CI_REVIEW_TOKEN, got: {message}"
        );
        assert!(
            message.contains("too short") || message.contains("strong token"),
            "error should explain the token is too short or needs a strong token, got: {message}"
        );
        assert!(
            !message.contains(rejected_token),
            "error must not echo the rejected token value, got: {message}"
        );
    }

    #[test]
    fn startup_config_from_explicit_env_values_applies_defaults_and_safe_validation() {
        let secret = "super-secret-webhook-value";
        let forgejo_token = "forgejo-token-that-must-not-leak";
        let values = GatewayStartupEnvValues {
            bind: None,
            webhook_secret: Some(secret),
            forgejo_base_url: Some("https://forgejo.example.test"),
            ar_forgejo_token: Some(forgejo_token),
            llm_base_url: Some("https://llm.example.test/v1"),
            llm_reasoning_model: None,
        };

        let config = GatewayStartupConfig::from_env_values(values).unwrap();

        assert_eq!(config.bind, "0.0.0.0:8080");
        assert_eq!(config.webhook_secret, secret);
        assert_eq!(config.forgejo_base_url, "https://forgejo.example.test");
        assert_eq!(config.ar_forgejo_token, forgejo_token);
        assert_eq!(config.llm_base_url, "https://llm.example.test/v1");
        assert_eq!(config.llm_reasoning_model, "qwen2.5-coder:32b");

        for (missing_name, missing_values) in [
            (
                "WEBHOOK_SECRET",
                GatewayStartupEnvValues {
                    webhook_secret: None,
                    ..values
                },
            ),
            (
                "FORGEJO_BASE_URL",
                GatewayStartupEnvValues {
                    forgejo_base_url: None,
                    ..values
                },
            ),
            (
                "AR_FORGEJO_TOKEN",
                GatewayStartupEnvValues {
                    ar_forgejo_token: None,
                    ..values
                },
            ),
            (
                "LLM_BASE_URL",
                GatewayStartupEnvValues {
                    llm_base_url: None,
                    ..values
                },
            ),
        ] {
            let err = GatewayStartupConfig::from_env_values(missing_values).unwrap_err();
            let message = err.to_string();

            assert!(
                message.contains(missing_name),
                "missing {missing_name} error should name the env var, got: {message}"
            );
            assert!(
                !message.contains(secret) && !message.contains(forgejo_token),
                "missing {missing_name} error must not leak secrets, got: {message}"
            );
        }

        let whitespace_model_err = GatewayStartupConfig::from_env_values(GatewayStartupEnvValues {
            llm_reasoning_model: Some(" \t\n "),
            ..values
        })
        .unwrap_err();
        let message = whitespace_model_err.to_string();

        assert!(
            message.contains("LLM_REASONING_MODEL"),
            "whitespace reasoning model error should name LLM_REASONING_MODEL, got: {message}"
        );
        assert!(
            message.contains("empty") || message.contains("whitespace"),
            "whitespace reasoning model error should explain the value is empty/whitespace, got: {message}"
        );
        assert!(
            !message.contains(secret) && !message.contains(forgejo_token),
            "whitespace reasoning model error must not leak secrets, got: {message}"
        );
    }

    #[test]
    fn launcher_decision_defaults_to_oci_and_fails_closed_when_setup_unavailable() {
        let err = select_gateway_launcher(
            GatewayLauncherEnvValues {
                bare: None,
                external_isolation: None,
            },
            || {
                Err(OciSetupDiagnostic::new(
                    "rootless OCI setup failed while preparing /run/secrets/ar-token",
                ))
            },
        )
        .unwrap_err();
        let message = err.to_string();

        assert!(
            message.contains("OCI"),
            "default gateway launcher failure should identify the OCI launcher path, got: {message}"
        );
        assert!(
            message.contains("setup failed") || message.contains("inner gateway startup"),
            "default gateway launcher failure should report sanitized OCI setup failure context, got: {message}"
        );
        assert!(
            message.contains("AR_GATEWAY_BARE") || message.contains("--bare"),
            "default gateway launcher failure should name the explicit bare opt-out, got: {message}"
        );
        assert!(
            !message.contains("ar-token") && !message.contains("/run/secrets"),
            "launcher diagnostics must not echo secret-bearing paths, got: {message}"
        );
    }

    #[test]
    fn embedded_oci_gateway_with_packaged_inputs_returns_finished_after_fake_launcher_success() {
        let launched = std::cell::Cell::new(false);
        let packaged_inputs = EmbeddedOciGatewayInputs {
            bundle_path: Path::new("/nix/store/test-ar-gateway-embedded-oci-rootfs"),
            runtime_path: Path::new("/nix/store/test-embedded-youki-runtime"),
        };

        let outcome = prepare_embedded_oci_gateway_with_inputs(packaged_inputs, |inputs| {
            launched.set(true);
            assert_eq!(inputs.bundle_path, packaged_inputs.bundle_path);
            assert_eq!(inputs.runtime_path, packaged_inputs.runtime_path);
            Ok(GatewayLaunchOutcome::OuterLauncherFinished)
        })
        .unwrap_or_else(|diagnostic| {
            panic!(
                "valid packaged OCI inputs plus fake launcher success should return OuterLauncherFinished, got setup diagnostic: {diagnostic:?}"
            )
        });

        assert!(launched.get(), "fake OCI launcher should be invoked");
        assert_eq!(outcome, GatewayLaunchOutcome::OuterLauncherFinished);
    }

    #[test]
    fn embedded_oci_gateway_accepts_portable_release_root_relocated_bundle_and_launcher() {
        let launched = std::cell::Cell::new(false);
        let portable_inputs = EmbeddedOciGatewayInputs {
            bundle_path: Path::new("/tmp/extracted/nix/store/test-ar-gateway-embedded-oci-rootfs"),
            runtime_path: Path::new("/tmp/extracted/bin/youki"),
        };

        let outcome = prepare_embedded_oci_gateway_with_inputs(portable_inputs, |inputs| {
            launched.set(true);
            assert_eq!(inputs.bundle_path, portable_inputs.bundle_path);
            assert_eq!(inputs.runtime_path, portable_inputs.runtime_path);
            Ok(GatewayLaunchOutcome::OuterLauncherFinished)
        })
        .unwrap_or_else(|diagnostic| {
            panic!(
                "portable release-root relocated OCI inputs plus fake launcher success should return OuterLauncherFinished, got setup diagnostic: {diagnostic:?}"
            )
        });

        assert!(
            launched.get(),
            "portable release-root youki launcher should be invoked"
        );
        assert_eq!(outcome, GatewayLaunchOutcome::OuterLauncherFinished);
    }

    #[test]
    fn embedded_oci_gateway_consumes_wrapper_packaged_paths_from_explicit_env_values() {
        let bundle_path = Path::new("/nix/store/wrapper-provided-ar-gateway-embedded-oci-rootfs");
        let runtime_path = Path::new("/nix/store/wrapper-provided-embedded-youki-runtime");
        let env_values = EmbeddedOciGatewayEnvValues {
            bundle_path: Some(bundle_path.to_str().unwrap()),
            runtime_path: Some(runtime_path.to_str().unwrap()),
        };
        let launched = std::cell::Cell::new(false);

        let outcome = prepare_embedded_oci_gateway_from_env_values(env_values, |inputs| {
            launched.set(true);
            assert_eq!(
                inputs.bundle_path, bundle_path,
                "OCI preparation must use AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH from the wrapper"
            );
            assert_eq!(
                inputs.runtime_path, runtime_path,
                "OCI preparation must use AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH from the wrapper"
            );
            Ok(GatewayLaunchOutcome::OuterLauncherFinished)
        })
        .unwrap_or_else(|diagnostic| {
            panic!(
                "wrapper-provided packaged OCI env values plus fake launcher success should return OuterLauncherFinished, got setup diagnostic: {diagnostic:?}"
            )
        });

        assert!(launched.get(), "fake OCI launcher should be invoked");
        assert_eq!(outcome, GatewayLaunchOutcome::OuterLauncherFinished);
    }

    #[test]
    fn embedded_oci_gateway_rejects_relative_packaged_paths_before_runtime_lookup() {
        for (case, packaged_inputs) in [
            (
                "relative runtime",
                EmbeddedOciGatewayInputs {
                    bundle_path: Path::new("/nix/store/test-ar-gateway-embedded-oci-rootfs"),
                    runtime_path: Path::new(
                        "secret-bearing-relative-runtime-/run/secrets/ar-token",
                    ),
                },
            ),
            (
                "relative bundle",
                EmbeddedOciGatewayInputs {
                    bundle_path: Path::new("secret-bearing-relative-bundle-/run/secrets/ar-token"),
                    runtime_path: Path::new("/nix/store/test-embedded-youki-runtime"),
                },
            ),
        ] {
            let diagnostic = prepare_embedded_oci_gateway_with_inputs(packaged_inputs, |_inputs| {
                panic!("{case} must be rejected before PATH/runtime lookup")
            })
            .unwrap_err();
            let message = format!("{diagnostic:?}");

            assert!(
                message.contains("absolute") || message.contains("package"),
                "{case} diagnostic should explain packaged OCI paths must be absolute/package-resolved, got: {message}"
            );
            assert!(
                !message.contains("secret-bearing")
                    && !message.contains("ar-token")
                    && !message.contains("/run/secrets"),
                "{case} diagnostic must not leak raw secret-bearing paths, got: {message}"
            );
        }
    }

    #[test]
    fn embedded_oci_gateway_rejects_unpackaged_absolute_paths_before_runtime_lookup() {
        for (case, packaged_inputs) in [
            (
                "tmp runtime",
                EmbeddedOciGatewayInputs {
                    bundle_path: Path::new("/nix/store/test-ar-gateway-embedded-oci-rootfs"),
                    runtime_path: Path::new("/tmp/secret-bearing-youki-/run/secrets/ar-token"),
                },
            ),
            (
                "home bundle",
                EmbeddedOciGatewayInputs {
                    bundle_path: Path::new(
                        "/home/alice/secret-bearing-rootfs-/run/secrets/ar-token",
                    ),
                    runtime_path: Path::new("/nix/store/test-embedded-youki-runtime"),
                },
            ),
        ] {
            let diagnostic = prepare_embedded_oci_gateway_with_inputs(packaged_inputs, |_inputs| {
                panic!("{case} must be rejected before PATH/runtime lookup")
            })
            .unwrap_err();
            let message = format!("{diagnostic:?}");

            assert!(
                message.contains("/nix/store") || message.contains("package-resolved"),
                "{case} diagnostic should explain default packaged OCI paths must resolve through the Nix store, got: {message}"
            );
            assert!(
                !message.contains("secret-bearing")
                    && !message.contains("ar-token")
                    && !message.contains("/run/secrets")
                    && !message.contains("/tmp/")
                    && !message.contains("/home/alice"),
                "{case} diagnostic must redact rejected absolute paths, got: {message}"
            );
        }
    }

    #[test]
    fn embedded_oci_gateway_rejects_nix_store_paths_with_traversal_components() {
        for (case, packaged_inputs) in [
            (
                "traversing runtime",
                EmbeddedOciGatewayInputs {
                    bundle_path: Path::new("/nix/store/test-ar-gateway-embedded-oci-rootfs"),
                    runtime_path: Path::new("/nix/store/../../tmp/secret-bearing-youki"),
                },
            ),
            (
                "traversing bundle",
                EmbeddedOciGatewayInputs {
                    bundle_path: Path::new("/nix/store/../secret-bearing-rootfs"),
                    runtime_path: Path::new("/nix/store/test-embedded-youki-runtime"),
                },
            ),
        ] {
            let diagnostic = prepare_embedded_oci_gateway_with_inputs(packaged_inputs, |_inputs| {
                panic!("{case} must be rejected before runtime lookup")
            })
            .unwrap_err();
            let message = format!("{diagnostic:?}");

            assert!(
                message.contains("traversal"),
                "{case} diagnostic should reject traversal components, got: {message}"
            );
            assert!(
                !message.contains("secret-bearing") && !message.contains("/tmp/"),
                "{case} diagnostic must redact rejected traversal path details, got: {message}"
            );
        }
    }

    #[test]
    fn runtime_isolation_posture_classifies_oci_default_as_container_equivalent() {
        let posture = classify_runtime_isolation_posture(RuntimeIsolationPostureInput {
            bare: None,
            external_isolation: None,
            oci_setup_diagnostic: None,
            target_os: "linux",
        })
        .unwrap();

        assert_eq!(posture.kind, RuntimeIsolationPostureKind::OciDefault);
        assert!(
            posture.operator_label.contains("container") || posture.operator_label.contains("OCI")
        );
        assert!(posture
            .operator_detail
            .contains("container-equivalent isolation"));
    }

    #[test]
    fn runtime_isolation_posture_classifies_external_container_marker() {
        let posture = classify_runtime_isolation_posture(RuntimeIsolationPostureInput {
            bare: None,
            external_isolation: Some("container"),
            oci_setup_diagnostic: None,
            target_os: "linux",
        })
        .unwrap();

        assert_eq!(posture.kind, RuntimeIsolationPostureKind::ExternalContainer);
        assert!(posture.operator_label.contains("external container"));
        assert!(posture.operator_detail.contains("already inside"));
    }

    #[test]
    fn runtime_isolation_posture_classifies_explicit_bare_without_isolation_claim() {
        let posture = classify_runtime_isolation_posture(RuntimeIsolationPostureInput {
            bare: Some("true"),
            external_isolation: None,
            oci_setup_diagnostic: None,
            target_os: "linux",
        })
        .unwrap();

        assert_eq!(posture.kind, RuntimeIsolationPostureKind::ExplicitBare);
        assert!(posture.operator_label.contains("bare"));
        assert!(posture
            .operator_detail
            .contains("only application-level controls"));
        assert!(!posture
            .operator_detail
            .contains("container-equivalent isolation is active"));
    }

    #[test]
    fn runtime_isolation_posture_redacts_secret_bearing_oci_setup_failure() {
        let posture = classify_runtime_isolation_posture(RuntimeIsolationPostureInput {
            bare: None,
            external_isolation: None,
            oci_setup_diagnostic: Some(OciSetupDiagnostic::new(
                "youki failed while opening /run/secrets/ar-token from secret-bearing bundle",
            )),
            target_os: "linux",
        })
        .unwrap();

        assert_eq!(posture.kind, RuntimeIsolationPostureKind::OciSetupFailure);
        assert!(posture.operator_detail.contains("OCI setup failed"));
        assert!(posture.operator_detail.contains("AR_GATEWAY_BARE"));
        assert!(
            !posture.operator_detail.contains("/run/secrets")
                && !posture.operator_detail.contains("ar-token")
                && !posture.operator_detail.contains("secret-bearing"),
            "operator-visible posture details must redact secret-bearing diagnostics: {}",
            posture.operator_detail
        );
    }

    #[test]
    fn runtime_isolation_posture_classifies_unsupported_platform_when_target_seam_reports_it() {
        let posture = classify_runtime_isolation_posture(RuntimeIsolationPostureInput {
            bare: None,
            external_isolation: None,
            oci_setup_diagnostic: None,
            target_os: "windows",
        })
        .unwrap();

        assert_eq!(
            posture.kind,
            RuntimeIsolationPostureKind::UnsupportedPlatform
        );
        assert!(posture.operator_label.contains("unsupported platform"));
        assert!(
            posture.operator_detail.contains("bare") || posture.operator_detail.contains("OCI")
        );
    }

    fn required_inner_gateway_env_source() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("WEBHOOK_SECRET", "webhook-secret-value-that-must-not-leak"),
            ("FORGEJO_BASE_URL", "https://forgejo.example.test"),
            ("AR_FORGEJO_TOKEN", "forgejo-token-value-that-must-not-leak"),
            ("LLM_BASE_URL", "https://llm.example.test/v1"),
            ("AR_GATEWAY_BIND", "127.0.0.1:9090"),
            ("LLM_REASONING_MODEL", "qwen-test-model"),
        ])
    }

    fn process_env_map(entries: &[(String, String)]) -> HashMap<&str, &str> {
        entries
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<HashMap<_, _>>()
    }

    #[test]
    fn inner_gateway_oci_env_allowlist_includes_required_config_and_excludes_unrelated_secret_names(
    ) {
        let mut source = required_inner_gateway_env_source();
        source.insert("LLM_API_KEY", "llm-api-key-value-that-must-not-leak");
        source.insert(
            "AWS_SECRET_ACCESS_KEY",
            "ambient-aws-secret-must-not-propagate",
        );
        source.insert("UNRELATED_TOKEN", "ambient-token-must-not-propagate");
        source.insert(
            "AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH",
            "/tmp/unpackaged-youki",
        );
        source.insert("AR_GATEWAY_BARE", "true");

        let process_env = inner_gateway_process_env_from_lookup(|name| {
            source.get(name).map(|value| (*value).to_string())
        })
        .unwrap_or_else(|diagnostic| {
            panic!("complete required env source should stage inner gateway env: {diagnostic:?}")
        });
        let env = process_env_map(&process_env);

        for required_name in [
            "WEBHOOK_SECRET",
            "FORGEJO_BASE_URL",
            "AR_FORGEJO_TOKEN",
            "LLM_BASE_URL",
        ] {
            assert!(
                env.contains_key(required_name),
                "inner gateway env must include required config/secret {required_name}: {env:?}"
            );
        }

        assert_eq!(env.get("AR_GATEWAY_BIND"), Some(&"127.0.0.1:9090"));
        assert_eq!(env.get("LLM_REASONING_MODEL"), Some(&"qwen-test-model"));
        assert_eq!(
            env.get("LLM_API_KEY"),
            Some(&"llm-api-key-value-that-must-not-leak")
        );
        assert_eq!(
            env.get("AR_GATEWAY_EXTERNAL_ISOLATION"),
            Some(&"container"),
            "inner gateway must see the container marker from staged config.json, not runtime env inheritance"
        );

        for forbidden_name in [
            "AWS_SECRET_ACCESS_KEY",
            "UNRELATED_TOKEN",
            "AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH",
            "AR_GATEWAY_BARE",
        ] {
            assert!(
                !env.contains_key(forbidden_name),
                "unrelated or outer-launcher-only env {forbidden_name} must not enter staged config: {env:?}"
            );
        }
        let rendered = format!("{process_env:?}");
        assert!(
            !rendered.contains("ambient-aws-secret-must-not-propagate")
                && !rendered.contains("ambient-token-must-not-propagate")
                && !rendered.contains("/tmp/unpackaged-youki"),
            "staged allowlist must not contain unrelated ambient values: {rendered}"
        );
    }

    #[test]
    fn inner_gateway_oci_env_missing_required_diagnostic_omits_values() {
        let mut source = required_inner_gateway_env_source();
        source.remove("LLM_BASE_URL");
        source.insert("AWS_SECRET_ACCESS_KEY", "ambient-aws-secret-must-not-leak");

        let diagnostic = inner_gateway_process_env_from_lookup(|name| {
            source.get(name).map(|value| (*value).to_string())
        })
        .unwrap_err();
        let message = format!("{diagnostic:?}");

        assert!(
            message.contains("LLM_BASE_URL"),
            "missing required env diagnostic should name the missing key, got: {message}"
        );
        assert!(
            !message.contains("webhook-secret-value")
                && !message.contains("forgejo-token-value")
                && !message.contains("ambient-aws-secret")
                && !message.contains("https://forgejo.example.test"),
            "missing required env diagnostic must not leak configured values, got: {message}"
        );
    }

    #[test]
    fn staged_oci_config_replaces_process_env_with_explicit_allowlist() {
        let mut source = required_inner_gateway_env_source();
        source.insert("LLM_API_KEY", "llm-api-key-value-that-must-not-leak");
        source.insert(
            "AWS_SECRET_ACCESS_KEY",
            "ambient-aws-secret-must-not-propagate",
        );
        let process_env = inner_gateway_process_env_from_lookup(|name| {
            source.get(name).map(|value| (*value).to_string())
        })
        .unwrap();

        let staged = staged_oci_config_with_process_env(
            serde_json::json!({
                "ociVersion": "1.0.2",
                "process": {
                    "args": ["/bin/auto-review", "gateway"],
                    "env": [
                        "PATH=/host/bin",
                        "AWS_SECRET_ACCESS_KEY=ambient-aws-secret-must-not-propagate",
                        "UNRELATED_TOKEN=ambient-token-must-not-propagate"
                    ]
                },
                "root": { "path": "rootfs", "readonly": true }
            }),
            &process_env,
        )
        .unwrap_or_else(|diagnostic| panic!("staged config should be produced: {diagnostic:?}"));

        let env_entries = staged["process"]["env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        let rendered = format!("{env_entries:?}");

        assert!(
            rendered.contains("WEBHOOK_SECRET=webhook-secret-value-that-must-not-leak"),
            "staged OCI config must carry required gateway secrets in process.env"
        );
        assert!(
            rendered.contains("AR_GATEWAY_EXTERNAL_ISOLATION=container"),
            "staged OCI config must carry the inner isolation marker"
        );
        assert!(
            !rendered.contains("AWS_SECRET_ACCESS_KEY")
                && !rendered.contains("UNRELATED_TOKEN")
                && !rendered.contains("ambient-aws-secret-must-not-propagate")
                && !rendered.contains("ambient-token-must-not-propagate"),
            "staged OCI config must replace ambient config env with explicit allowlist: {rendered}"
        );
        assert_eq!(staged["root"]["path"], "rootfs");
    }

    #[test]
    fn staged_oci_bundle_materializes_config_and_runtime_command_points_at_stage() {
        let mut source = required_inner_gateway_env_source();
        source.insert("LLM_API_KEY", "llm-api-key-value-that-must-not-leak");
        let process_env = inner_gateway_process_env_from_lookup(|name| {
            source.get(name).map(|value| (*value).to_string())
        })
        .unwrap();
        let packaged_bundle = tempfile::tempdir().unwrap();
        std::fs::create_dir(packaged_bundle.path().join("rootfs")).unwrap();
        std::fs::write(
            packaged_bundle.path().join("config.json"),
            serde_json::json!({
                "ociVersion": "1.0.2",
                "process": { "env": ["UNRELATED_TOKEN=ambient-token-must-not-propagate"] },
                "root": { "path": "rootfs", "readonly": true }
            })
            .to_string(),
        )
        .unwrap();
        let stage_parent = tempfile::tempdir().unwrap();
        let stage_bundle = stage_parent.path().join("auto-review-oci-stage");

        let staged_bundle = stage_embedded_oci_gateway_bundle_at_path(
            packaged_bundle.path(),
            &stage_bundle,
            &process_env,
        )
        .unwrap_or_else(|diagnostic| panic!("staged bundle should be created: {diagnostic:?}"));

        assert_eq!(staged_bundle, stage_bundle);
        assert!(
            staged_bundle.join("config.json").is_file(),
            "staged bundle must contain generated config.json"
        );
        assert!(
            staged_bundle.join("rootfs").exists(),
            "staged bundle must contain or link the packaged rootfs"
        );
        let staged_config = std::fs::read_to_string(staged_bundle.join("config.json")).unwrap();
        assert!(staged_config.contains("WEBHOOK_SECRET=webhook-secret-value-that-must-not-leak"));
        assert!(!staged_config.contains("UNRELATED_TOKEN"));

        let command = build_packaged_oci_runtime_command(
            Path::new("/nix/store/test-embedded-youki-runtime/bin/youki"),
            &staged_bundle,
        );

        assert!(command.clear_ambient_env);
        assert!(
            command.env.is_empty(),
            "OCI runtime process env must stay empty; inner gateway env belongs in staged config.json"
        );
        assert_eq!(
            command.args,
            vec![
                PathBuf::from("run"),
                PathBuf::from("--bundle"),
                staged_bundle,
                PathBuf::from("auto-review-gateway"),
            ]
        );
    }

    #[test]
    fn packaged_oci_runtime_success_executes_packaged_runtime_against_packaged_bundle() {
        let packaged_inputs = EmbeddedOciGatewayInputs {
            bundle_path: Path::new("/nix/store/test-ar-gateway-embedded-oci-rootfs"),
            runtime_path: Path::new("/nix/store/test-embedded-youki-runtime"),
        };
        let mut observed_runtime = None;
        let mut observed_args = Vec::new();

        let outcome = execute_packaged_oci_runtime_with_executor(packaged_inputs, |command| {
            observed_runtime = Some(command.program.to_path_buf());
            observed_args = command
                .args
                .iter()
                .map(|arg| arg.to_path_buf())
                .collect::<Vec<_>>();

            Ok(())
        })
        .unwrap_or_else(|diagnostic| {
            panic!(
                "successful fake OCI runtime execution should finish the outer launcher, got setup diagnostic: {diagnostic:?}"
            )
        });

        assert_eq!(outcome, GatewayLaunchOutcome::OuterLauncherFinished);
        assert_eq!(
            observed_runtime.as_deref(),
            Some(packaged_inputs.runtime_path),
            "OCI execution must use the packaged runtime binary"
        );
        assert_eq!(
            observed_args,
            vec![
                PathBuf::from("run"),
                PathBuf::from("--bundle"),
                packaged_inputs.bundle_path.to_path_buf(),
                PathBuf::from("auto-review-gateway"),
            ],
            "OCI execution must use the youki-compatible shape: run --bundle <bundle> <stable-container-id>"
        );
    }

    #[test]
    fn packaged_oci_runtime_command_clears_ambient_env_without_gateway_env_passthrough() {
        let packaged_inputs = EmbeddedOciGatewayInputs {
            bundle_path: Path::new("/nix/store/test-ar-gateway-embedded-oci-rootfs"),
            runtime_path: Path::new("/nix/store/test-embedded-youki-runtime"),
        };

        execute_packaged_oci_runtime_with_executor(packaged_inputs, |command| {
            assert!(
                command.clear_ambient_env,
                "OCI runtime command must clear the ambient process environment"
            );
            assert!(
                command.env.is_empty(),
                "inner gateway env values must be staged into OCI config.json, not inherited by the runtime process: {:?}",
                command.env
            );

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn packaged_oci_runtime_failure_diagnostic_omits_secret_bearing_command_paths() {
        let packaged_inputs = EmbeddedOciGatewayInputs {
            bundle_path: Path::new(
                "/nix/store/test-ar-gateway-embedded-oci-rootfs-/run/secrets/ar-token",
            ),
            runtime_path: Path::new("/nix/store/test-embedded-youki-runtime-/run/secrets/ar-token"),
        };

        let diagnostic = execute_packaged_oci_runtime_with_executor(packaged_inputs, |_command| {
            Err(OciSetupDiagnostic::new(
                "runtime failure while using /run/secrets/ar-token and secret-bearing-runtime-path",
            ))
        })
        .unwrap_err();
        let message = format!("{diagnostic:?}");

        assert!(
            message.contains("runtime") || message.contains("OCI"),
            "runtime failure diagnostic should identify the failing subsystem, got: {message}"
        );
        assert!(
            !message.contains("secret-bearing")
                && !message.contains("ar-token")
                && !message.contains("/run/secrets"),
            "runtime failure diagnostic must not leak raw secret-bearing paths, got: {message}"
        );
    }

    #[test]
    fn launcher_decision_trueish_bare_values_skip_oci_preparation() {
        for bare in ["1", "true", "yes", "on", " TRUE ", "Yes", "On"] {
            let outcome = select_gateway_launcher(
                GatewayLauncherEnvValues {
                    bare: Some(bare),
                    external_isolation: None,
                },
                || panic!("true-ish AR_GATEWAY_BARE={bare:?} must skip OCI preparation"),
            )
            .unwrap();

            assert_eq!(outcome, GatewayLaunchOutcome::ContinueInProcess);
        }
    }

    #[test]
    fn explicit_bare_launcher_selection_emits_prominent_warning() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let subscriber = CapturingSubscriber {
            messages: Arc::clone(&captured),
        };

        tracing::subscriber::with_default(subscriber, || {
            select_gateway_launcher(
                GatewayLauncherEnvValues {
                    bare: Some("true"),
                    external_isolation: None,
                },
                || panic!("explicit bare mode must skip OCI preparation"),
            )
            .unwrap();
        });

        let messages = captured.lock().unwrap();
        assert!(
            messages
                .iter()
                .any(|message| message == explicit_bare_gateway_mode_warning()),
            "select_gateway_launcher must emit the explicit bare warning; captured messages: {messages:?}"
        );
    }

    #[test]
    fn explicit_bare_gateway_mode_warning_names_limited_controls_without_isolation_claim() {
        let warning = explicit_bare_gateway_mode_warning();
        let lower = warning.to_ascii_lowercase();

        assert!(
            lower.contains("warning") || lower.contains("caution"),
            "bare gateway mode notice must be prominent, got: {warning}"
        );
        assert!(
            lower.contains("bare"),
            "bare gateway mode notice should name the selected mode, got: {warning}"
        );
        assert!(
            lower.contains("only application-level controls"),
            "bare gateway mode notice must say only application-level controls are active, got: {warning}"
        );
        assert!(
            lower.contains("not container-equivalent isolation"),
            "bare gateway mode notice must not imply container-equivalent isolation, got: {warning}"
        );
        assert!(
            !lower.contains("container-equivalent isolation is active"),
            "bare gateway mode notice must not claim container-equivalent isolation is active, got: {warning}"
        );
    }

    #[test]
    fn launcher_decision_falseish_bare_values_prepare_oci() {
        for bare in ["0", "false", "no", "off", " FALSE ", "No", "Off"] {
            let mut prepared_oci = false;

            let outcome = select_gateway_launcher(
                GatewayLauncherEnvValues {
                    bare: Some(bare),
                    external_isolation: None,
                },
                || {
                    prepared_oci = true;
                    Ok(GatewayLaunchOutcome::OuterLauncherFinished)
                },
            )
            .unwrap();

            assert_eq!(outcome, GatewayLaunchOutcome::OuterLauncherFinished);

            assert!(
                prepared_oci,
                "false-ish AR_GATEWAY_BARE={bare:?} must call OCI preparation"
            );
        }
    }

    #[test]
    fn external_container_isolation_continues_without_bare_warning_or_oci_preparation() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let subscriber = CapturingSubscriber {
            messages: Arc::clone(&captured),
        };

        let outcome = tracing::subscriber::with_default(subscriber, || {
            select_gateway_launcher(
                GatewayLauncherEnvValues {
                    bare: None,
                    external_isolation: Some("container"),
                },
                || panic!("external container isolation should not prepare embedded OCI"),
            )
        })
        .unwrap();

        assert_eq!(outcome, GatewayLaunchOutcome::ContinueInProcess);
        let messages = captured.lock().unwrap();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("external container isolation marker")),
            "external container isolation must emit an auditable startup posture log; captured messages: {messages:?}"
        );
        assert!(
            messages
                .iter()
                .all(|message| message != explicit_bare_gateway_mode_warning()),
            "external container isolation must not emit explicit bare-mode warnings"
        );
    }

    #[test]
    fn launcher_decision_rejects_unrecognized_bare_opt_out_without_preparing_oci() {
        let err = select_gateway_launcher(
            GatewayLauncherEnvValues {
                bare: Some("maybe-/run/secrets/ar-token"),
                external_isolation: None,
            },
            || panic!("unrecognized AR_GATEWAY_BARE value must fail before OCI preparation"),
        )
        .unwrap_err();
        let message = err.to_string();

        assert!(
            message.contains("AR_GATEWAY_BARE"),
            "unrecognized bare opt-out error should name AR_GATEWAY_BARE, got: {message}"
        );
        assert!(
            !message.contains("maybe-/run/secrets/ar-token")
                && !message.contains("ar-token")
                && !message.contains("/run/secrets"),
            "unrecognized bare opt-out error must not echo raw env values, got: {message}"
        );
    }

    #[test]
    fn startup_options_bare_true_selects_explicit_bare_when_env_unset() {
        select_gateway_launcher_for_startup_options(
            StartupOptions { bare: true },
            GatewayLauncherEnvValues {
                bare: None,
                external_isolation: None,
            },
            || panic!("CLI --bare must skip OCI preparation when AR_GATEWAY_BARE is unset"),
        )
        .unwrap();
    }

    #[test]
    fn startup_options_bare_false_with_env_unset_uses_default_oci_path() {
        let mut prepared_oci = false;

        select_gateway_launcher_for_startup_options(
            StartupOptions { bare: false },
            GatewayLauncherEnvValues {
                bare: None,
                external_isolation: None,
            },
            || {
                prepared_oci = true;
                Ok(GatewayLaunchOutcome::OuterLauncherFinished)
            },
        )
        .unwrap();

        assert!(
            prepared_oci,
            "without CLI --bare or AR_GATEWAY_BARE, startup must fail closed through OCI setup"
        );
    }

    #[test]
    fn startup_options_reject_invalid_env_when_no_cli_bare_override_exists() {
        let err = select_gateway_launcher_for_startup_options(
            StartupOptions { bare: false },
            GatewayLauncherEnvValues {
                bare: Some("maybe-/run/secrets/ar-token"),
                external_isolation: None,
            },
            || panic!("invalid AR_GATEWAY_BARE must fail before OCI preparation"),
        )
        .unwrap_err();
        let message = err.to_string();

        assert!(
            message.contains("AR_GATEWAY_BARE"),
            "invalid env error should name AR_GATEWAY_BARE, got: {message}"
        );
        assert!(
            !message.contains("maybe-/run/secrets/ar-token")
                && !message.contains("ar-token")
                && !message.contains("/run/secrets"),
            "invalid env error must not echo raw env values, got: {message}"
        );
    }
}
