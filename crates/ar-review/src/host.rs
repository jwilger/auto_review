//! Repository-host trait used by the review pipeline.

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use and module_name_repetitions, yet the expectations below are what suppress \
              those lints here"
)]
#[expect(
    clippy::pub_use,
    reason = "pipeline imports the host trait as crate::host::ReviewHost and the crate root \
              re-exports it as ar_review::ReviewHost, which the red-team tests implement"
)]
#[expect(
    clippy::module_name_repetitions,
    reason = "the re-export keeps ar-forge's trait name ReviewHost so callers see one name for it"
)]
pub use ar_forge::ReviewHost;
