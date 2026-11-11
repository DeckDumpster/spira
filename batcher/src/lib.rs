//! batcher — pure core only (sp-xr8rf). The IO seam that drives it with git, testenv-batch,
//! the forge and the bead store, and wires it into the queue in place of batch.sh's cut, is
//! a separate crate (sp-jzfog).

pub mod attrib;
pub mod buildattr;
pub mod core;
pub mod drive;

pub mod mender;
pub mod pathattr;
pub mod unitattr;

#[cfg(test)]
mod fork_fence {
    #[test]
    fn batcher_starts_no_spira_child_that_is_not_a_named_fork_edge() {
        let got = forge::fork_scan::scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        assert!(got.is_empty(), "unmarked children:\n{}", got.join("\n"));
    }
}
