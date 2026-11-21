//! Resolve env-var-driven storage backings.

use std::path::{Path, PathBuf};

/// Concrete backing chosen for a per-store env var.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "preserve public enum discriminants"
)]
pub enum DbBacking {
    Sqlite(PathBuf),
    InMemory,
}

/// Pure XDG-state path composer. Inputs are explicit so tests don't
/// need to mutate the process env.
///
/// `xdg_state_home` and `home` should be the values of `$XDG_STATE_HOME`
/// and `$HOME` respectively (or `None` if unset). Returns the path
/// `<base>/auto_review/<filename>`, where `<base>` is the first of:
///   * `xdg_state_home` (when `Some` and non-empty)
///   * `home/.local/state` (when `home` is `Some`)
///   * `.` (last-ditch fallback so this never panics)
#[inline]
#[must_use = "this function only composes a path"]
pub fn compose_state_path(
    xdg_state_home: Option<&Path>,
    home: Option<&Path>,
    filename: &str,
) -> PathBuf {
    let base = if let Some(state) = xdg_state_home.filter(|state| !state.as_os_str().is_empty()) {
        state.to_path_buf()
    } else if let Some(home_path) = home {
        home_path.join(".local/state")
    } else {
        PathBuf::from(".")
    };
    base.join("auto_review").join(filename)
}

#[inline]
pub fn resolve_db_backing(env_value: Option<&str>, default_path: &Path) -> DbBacking {
    match env_value.map(str::trim) {
        Some(":memory:") => DbBacking::InMemory,
        Some(path) if !path.is_empty() => DbBacking::Sqlite(PathBuf::from(path)),
        _ => DbBacking::Sqlite(default_path.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_env_resolves_to_default_sqlite_path() {
        let default = PathBuf::from("/state/auto_review/learnings.db");
        assert_eq!(
            resolve_db_backing(None, &default),
            DbBacking::Sqlite(default),
        );
    }

    #[test]
    fn explicit_memory_marker_resolves_to_in_memory() {
        let default = PathBuf::from("/state/auto_review/learnings.db");
        assert_eq!(
            resolve_db_backing(Some(":memory:"), &default),
            DbBacking::InMemory,
        );
    }

    #[test]
    fn non_empty_path_overrides_default() {
        let default = PathBuf::from("/state/auto_review/learnings.db");
        assert_eq!(
            resolve_db_backing(Some("/srv/pr-bot/learnings.sqlite"), &default),
            DbBacking::Sqlite(PathBuf::from("/srv/pr-bot/learnings.sqlite")),
        );
    }

    #[test]
    fn compose_state_path_uses_xdg_state_home_when_present() {
        let path = compose_state_path(
            Some(Path::new("/var/lib/state")),
            Some(Path::new("/home/alice")),
            "learnings.db",
        );
        assert_eq!(
            path,
            PathBuf::from("/var/lib/state/auto_review/learnings.db")
        );
    }

    #[test]
    fn compose_state_path_falls_back_to_home_dot_local_state() {
        let path = compose_state_path(None, Some(Path::new("/home/alice")), "history.db");
        assert_eq!(
            path,
            PathBuf::from("/home/alice/.local/state/auto_review/history.db"),
        );
    }

    #[test]
    fn compose_state_path_treats_empty_xdg_as_unset() {
        // Container envs sometimes set XDG_STATE_HOME="" defensively;
        // an empty value should not become `/auto_review/...` at the
        // filesystem root.
        let path = compose_state_path(
            Some(Path::new("")),
            Some(Path::new("/home/alice")),
            "vector.db",
        );
        assert_eq!(
            path,
            PathBuf::from("/home/alice/.local/state/auto_review/vector.db"),
        );
    }

    #[test]
    fn compose_state_path_falls_back_to_cwd_when_neither_env_present() {
        let path = compose_state_path(None, None, "dedup.db");
        assert_eq!(path, PathBuf::from("./auto_review/dedup.db"));
    }

    #[test]
    fn empty_or_whitespace_env_falls_through_to_default() {
        // Mirrors `read_non_empty_env` semantics: empty/whitespace
        // values are treated as misconfiguration, not opt-out, so the
        // operator gets the persistent default rather than silently
        // switching to in-memory.
        let default = PathBuf::from("/state/auto_review/learnings.db");
        for raw in ["", "   ", "\t"] {
            assert_eq!(
                resolve_db_backing(Some(raw), &default),
                DbBacking::Sqlite(default.clone()),
                "value {raw:?} should fall through to default",
            );
        }
    }
}
