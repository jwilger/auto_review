use ar_gateway::{run_from_env, StartupOptions};
use std::env;
use std::ffi::OsString;
use std::fs;
use tokio::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::const_new(());

#[cfg(unix)]
#[tokio::test]
async fn packaged_oci_runtime_passes_rootless_session_env_without_secret_like_ambient_env() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = ENV_LOCK.lock().await;
    let names = [
        "AR_GATEWAY_BARE",
        "AR_GATEWAY_EXTERNAL_ISOLATION",
        "AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH",
        "AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH",
        "WEBHOOK_SECRET",
        "FORGEJO_BASE_URL",
        "AR_FORGEJO_TOKEN",
        "LLM_BASE_URL",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "UNRELATED_TOKEN",
    ];
    let saved = names.map(|name| (name, env::var_os(name)));
    let release_root = tempfile::tempdir().unwrap();
    let bundle = release_root
        .path()
        .join("nix/store/test-ar-gateway-embedded-oci-rootfs");
    let runtime = release_root.path().join("bin/youki");
    let observed_env = release_root.path().join("observed-runtime-env");

    fs::create_dir_all(bundle.join("rootfs")).unwrap();
    fs::write(
        bundle.join("config.json"),
        r#"{"ociVersion":"1.0.2","process":{},"root":{"path":"rootfs","readonly":true}}"#,
    )
    .unwrap();
    fs::create_dir_all(runtime.parent().unwrap()).unwrap();
    fs::write(
        &runtime,
        format!(
            "#!/bin/sh\n{{\nprintf 'DBUS_SESSION_BUS_ADDRESS=%s\\n' \"$DBUS_SESSION_BUS_ADDRESS\"\nprintf 'XDG_RUNTIME_DIR=%s\\n' \"$XDG_RUNTIME_DIR\"\nprintf 'UNRELATED_TOKEN=%s\\n' \"$UNRELATED_TOKEN\"\n}} > '{}'\n",
            observed_env.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();

    env::remove_var("AR_GATEWAY_BARE");
    env::remove_var("AR_GATEWAY_EXTERNAL_ISOLATION");
    env::set_var("AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH", &bundle);
    env::set_var("AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH", &runtime);
    env::set_var("WEBHOOK_SECRET", "webhook-secret-value-that-must-not-leak");
    env::set_var("FORGEJO_BASE_URL", "https://forgejo.example.test");
    env::set_var("AR_FORGEJO_TOKEN", "forgejo-token-value-that-must-not-leak");
    env::set_var("LLM_BASE_URL", "https://llm.example.test/v1");
    env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus");
    env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
    env::set_var("UNRELATED_TOKEN", "ambient-token-must-not-propagate");

    let outcome = run_from_env(StartupOptions { bare: false }).await;
    restore_env(saved);
    outcome.unwrap();
    let observed = fs::read_to_string(observed_env).unwrap();

    assert!(
        observed.contains("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus"),
        "youki rootless cgroup setup needs DBUS_SESSION_BUS_ADDRESS passed explicitly after env_clear; observed runtime env:\n{observed}"
    );
    assert!(
        observed.contains("XDG_RUNTIME_DIR=/run/user/1000"),
        "youki rootless cgroup setup needs XDG_RUNTIME_DIR passed explicitly after env_clear; observed runtime env:\n{observed}"
    );
    assert!(
        observed.contains("UNRELATED_TOKEN=\n")
            && !observed.contains("ambient-token-must-not-propagate"),
        "secret-like unrelated ambient variables must not be passed to the OCI runtime; observed runtime env:\n{observed}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn embedded_oci_gateway_reports_non_unicode_packaged_path_env_as_invalid() {
    use std::os::unix::ffi::OsStringExt;

    let _guard = ENV_LOCK.lock().await;
    let names = [
        "AR_GATEWAY_BARE",
        "AR_GATEWAY_EXTERNAL_ISOLATION",
        "AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH",
        "AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH",
        "WEBHOOK_SECRET",
        "FORGEJO_BASE_URL",
        "AR_FORGEJO_TOKEN",
        "LLM_BASE_URL",
    ];
    let saved = names.map(|name| (name, env::var_os(name)));

    env::remove_var("AR_GATEWAY_BARE");
    env::remove_var("AR_GATEWAY_EXTERNAL_ISOLATION");
    env::set_var(
        "AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH",
        OsString::from_vec(b"/nix/store/non-unicode-\xFF-rootfs".to_vec()),
    );
    env::set_var(
        "AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH",
        "/nix/store/test-embedded-youki-runtime",
    );
    env::set_var("WEBHOOK_SECRET", "webhook-secret-value-that-must-not-leak");
    env::set_var("FORGEJO_BASE_URL", "https://forgejo.example.test");
    env::set_var("AR_FORGEJO_TOKEN", "forgejo-token-value-that-must-not-leak");
    env::set_var("LLM_BASE_URL", "https://llm.example.test/v1");

    let diagnostic = run_from_env(StartupOptions { bare: false })
        .await
        .expect_err("non-Unicode packaged OCI env should fail before startup");
    let message = diagnostic.to_string();

    restore_env(saved);

    assert!(
        message.contains("AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH"),
        "non-Unicode packaged path diagnostic should name the env var, got: {message}"
    );
    assert!(
        message.contains("Unicode") || message.contains("invalid"),
        "non-Unicode packaged path diagnostic should distinguish invalid env from missing env, got: {message}"
    );
    assert!(
        !message.contains("webhook-secret-value")
            && !message.contains("forgejo-token-value")
            && !message.contains("https://forgejo.example.test")
            && !message.contains("/nix/store/non-unicode")
            && !message.contains('\u{FFFD}'),
        "non-Unicode env diagnostics must not leak configured values or malformed env content, got: {message}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn embedded_oci_gateway_reports_non_unicode_inner_env_as_invalid() {
    use std::os::unix::ffi::OsStringExt;

    let _guard = ENV_LOCK.lock().await;
    let names = [
        "AR_GATEWAY_BARE",
        "AR_GATEWAY_EXTERNAL_ISOLATION",
        "AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH",
        "AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH",
        "WEBHOOK_SECRET",
        "FORGEJO_BASE_URL",
        "AR_FORGEJO_TOKEN",
        "LLM_BASE_URL",
        "LLM_API_KEY",
    ];
    let saved = names.map(|name| (name, env::var_os(name)));

    env::remove_var("AR_GATEWAY_BARE");
    env::remove_var("AR_GATEWAY_EXTERNAL_ISOLATION");
    env::set_var(
        "AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH",
        "/nix/store/test-ar-gateway-embedded-oci-rootfs",
    );
    env::set_var(
        "AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH",
        "/nix/store/test-embedded-youki-runtime",
    );
    env::set_var("WEBHOOK_SECRET", "webhook-value-for-test");
    env::set_var("FORGEJO_BASE_URL", "https://forgejo.example.test");
    env::set_var("AR_FORGEJO_TOKEN", "forgejo-value-for-test");
    env::set_var("LLM_BASE_URL", "https://llm.example.test/v1");
    env::set_var(
        "LLM_API_KEY",
        OsString::from_vec(b"llm-api-key-\xFF-value".to_vec()),
    );

    let diagnostic = run_from_env(StartupOptions { bare: false })
        .await
        .expect_err("non-Unicode inner gateway env should fail before staging OCI config");
    let message = diagnostic.to_string();

    restore_env(saved);

    assert!(
        message.contains("LLM_API_KEY"),
        "non-Unicode inner gateway env diagnostic should name the env var, got: {message}"
    );
    assert!(
        message.contains("Unicode") || message.contains("invalid"),
        "non-Unicode inner gateway env diagnostic should distinguish invalid env from omission, got: {message}"
    );
    assert!(
        !message.contains("webhook-value-for-test")
            && !message.contains("forgejo-value-for-test")
            && !message.contains("llm-api-key")
            && !message.contains('\u{FFFD}'),
        "non-Unicode inner gateway env diagnostics must not leak configured values or malformed env content, got: {message}"
    );
}

fn restore_env<const N: usize>(saved: [(&str, Option<OsString>); N]) {
    for (name, value) in saved {
        if let Some(value) = value {
            env::set_var(name, value);
        } else {
            env::remove_var(name);
        }
    }
}
