//! Git tree entry modes.
//!
//! git uses a restricted set of Unix mode bits. Anything outside this
//! set is invalid in a tree.

/// Tree-entry mode. Corresponds exactly to git's `cache_entry` mode
/// bits; do not invent new variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Regular file.
    RegularFile,
    /// Executable file.
    Executable,
    /// Symbolic link.
    Symlink,
    /// Submodule (gitlink).
    Submodule,
    /// Subtree.
    Tree,
}

impl Mode {
    /// The octal string git writes for this mode in a tree object.
    ///
    /// git uses variable-length (no leading zeros): `100644`, `100755`,
    /// `120000`, `160000`, `40000`. **Trees have NO leading zero** —
    /// it's `40000`, not `040000`. This is a common trap.
    pub fn as_git_str(&self) -> &'static str {
        match self {
            Mode::RegularFile => "100644",
            Mode::Executable => "100755",
            Mode::Symlink => "120000",
            Mode::Submodule => "160000",
            Mode::Tree => "40000",
        }
    }

    /// Parse git's on-wire mode string into the typed variant.
    pub fn from_git_str(s: &str) -> Option<Self> {
        match s {
            "100644" => Some(Mode::RegularFile),
            "100755" => Some(Mode::Executable),
            "120000" => Some(Mode::Symlink),
            "160000" => Some(Mode::Submodule),
            "40000" | "040000" => Some(Mode::Tree),
            _ => None,
        }
    }

    /// True if this mode references a blob (file-like); false if it
    /// references a tree or submodule.
    pub fn is_blob(&self) -> bool {
        matches!(self, Mode::RegularFile | Mode::Executable | Mode::Symlink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_str_round_trip() {
        for m in [
            Mode::RegularFile,
            Mode::Executable,
            Mode::Symlink,
            Mode::Submodule,
            Mode::Tree,
        ] {
            assert_eq!(Mode::from_git_str(m.as_git_str()), Some(m));
        }
    }

    #[test]
    fn tree_mode_has_no_leading_zero() {
        // Canonical form per git-core. A `040000` serialization would
        // produce a different tree hash.
        assert_eq!(Mode::Tree.as_git_str(), "40000");
    }

    #[test]
    fn accepts_040000_on_parse_but_normalises() {
        // Some tools emit `040000` in debug output; be lenient on parse,
        // strict on emit.
        assert_eq!(Mode::from_git_str("040000"), Some(Mode::Tree));
        assert_eq!(Mode::Tree.as_git_str(), "40000");
    }
}
