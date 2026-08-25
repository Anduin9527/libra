use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        commit::Commit,
        tree::{Tree, TreeItemMode},
    },
};

use super::domain::EpisodeCodeContextV1;
use crate::utils::object::read_git_object_bounded_validated;

pub(crate) const MAX_APPLICABILITY_COMMITS: usize = 2_048;
pub(crate) const MAX_APPLICABILITY_PATHS: usize = 512;
const MAX_CODE_PATH_BYTES: usize = 4_096;
const MAX_CODE_PATH_COMPONENTS: usize = 256;
const MAX_CODE_COMMIT_BYTES: u64 = 1024 * 1024;
const MAX_CODE_TREE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum CodeApplicability {
    Exact,
    DescendantUnchanged,
    DescendantPathChanged,
    Diverged,
    Unknown,
}

impl CodeApplicability {
    pub(crate) const fn tier(self) -> u8 {
        match self {
            Self::Exact => 0,
            Self::DescendantUnchanged => 1,
            Self::DescendantPathChanged => 2,
            Self::Diverged => 3,
            Self::Unknown => 4,
        }
    }

    pub(crate) const fn injectable(self) -> bool {
        matches!(self, Self::Exact | Self::DescendantUnchanged)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TreeEntryFingerprint {
    oid: ObjectHash,
    mode: TreeItemMode,
}

impl TreeEntryFingerprint {
    #[cfg(test)]
    const fn new(oid: ObjectHash, mode: TreeItemMode) -> Self {
        Self { oid, mode }
    }
}

pub(crate) trait CodeHistory {
    fn parents(&self, commit: ObjectHash) -> Result<Vec<ObjectHash>, ()>;
    fn entry(&self, commit: ObjectHash, path: &str) -> Result<Option<TreeEntryFingerprint>, ()>;
}

pub(crate) struct RepositoryCodeHistory {
    repository_path: PathBuf,
}

impl RepositoryCodeHistory {
    pub(crate) fn new(repository_path: &Path) -> Self {
        Self {
            repository_path: repository_path.to_path_buf(),
        }
    }

    fn load_commit(&self, oid: ObjectHash) -> Result<Commit, ()> {
        let (object_type, bytes) =
            read_git_object_bounded_validated(&self.repository_path, &oid, MAX_CODE_COMMIT_BYTES)
                .map_err(|_| ())?;
        if object_type != "commit" {
            return Err(());
        }
        Commit::from_bytes(&bytes, oid).map_err(|_| ())
    }

    fn load_tree(&self, oid: ObjectHash) -> Result<Tree, ()> {
        let (object_type, bytes) =
            read_git_object_bounded_validated(&self.repository_path, &oid, MAX_CODE_TREE_BYTES)
                .map_err(|_| ())?;
        if object_type != "tree" {
            return Err(());
        }
        Tree::from_bytes(&bytes, oid).map_err(|_| ())
    }
}

impl CodeHistory for RepositoryCodeHistory {
    fn parents(&self, commit: ObjectHash) -> Result<Vec<ObjectHash>, ()> {
        self.load_commit(commit)
            .map(|commit| commit.parent_commit_ids)
    }

    fn entry(&self, commit: ObjectHash, path: &str) -> Result<Option<TreeEntryFingerprint>, ()> {
        let commit = self.load_commit(commit)?;
        let mut tree = self.load_tree(commit.tree_id)?;
        let mut components = path.split('/').peekable();
        while let Some(component) = components.next() {
            let Some(item) = tree
                .tree_items
                .iter()
                .find(|candidate| candidate.name == component)
            else {
                return Ok(None);
            };
            if components.peek().is_none() {
                return Ok(Some(TreeEntryFingerprint {
                    oid: item.id,
                    mode: item.mode,
                }));
            }
            if item.mode != TreeItemMode::Tree {
                return Ok(None);
            }
            tree = self.load_tree(item.id)?;
        }
        Ok(None)
    }
}

pub(crate) fn classify_code_applicability<H: CodeHistory>(
    history: &H,
    current_commit: ObjectHash,
    code: &EpisodeCodeContextV1,
) -> CodeApplicability {
    let Some(result_oid) = code
        .result_oid
        .as_deref()
        .and_then(|value| value.parse::<ObjectHash>().ok())
    else {
        return CodeApplicability::Unknown;
    };
    if result_oid == current_commit {
        return CodeApplicability::Exact;
    }
    if code.paths.is_empty()
        || code.paths.len() > MAX_APPLICABILITY_PATHS
        || code.paths.iter().any(|path| !valid_code_path(path))
    {
        return CodeApplicability::Unknown;
    }
    match bounded_ancestry(history, result_oid, current_commit) {
        Ancestry::Diverged => return CodeApplicability::Diverged,
        Ancestry::Unknown => return CodeApplicability::Unknown,
        Ancestry::Ancestor => {}
    }

    let mut changed = false;
    for path in &code.paths {
        let anchored = match history.entry(result_oid, path) {
            Ok(Some(entry)) => entry,
            Ok(None) | Err(()) => return CodeApplicability::Unknown,
        };
        let current = match history.entry(current_commit, path) {
            Ok(entry) => entry,
            Err(()) => return CodeApplicability::Unknown,
        };
        if current != Some(anchored) {
            changed = true;
        }
    }
    if changed {
        CodeApplicability::DescendantPathChanged
    } else {
        CodeApplicability::DescendantUnchanged
    }
}

enum Ancestry {
    Ancestor,
    Diverged,
    Unknown,
}

fn bounded_ancestry<H: CodeHistory>(
    history: &H,
    ancestor: ObjectHash,
    descendant: ObjectHash,
) -> Ancestry {
    let mut queue = VecDeque::from([descendant]);
    let mut visited = HashSet::new();
    for _ in 0..MAX_APPLICABILITY_COMMITS {
        let Some(commit) = queue.pop_front() else {
            return Ancestry::Diverged;
        };
        if !visited.insert(commit) {
            continue;
        }
        if commit == ancestor {
            return Ancestry::Ancestor;
        }
        let parents = match history.parents(commit) {
            Ok(parents) => parents,
            Err(()) => return Ancestry::Unknown,
        };
        queue.extend(parents);
    }
    if queue.is_empty() {
        Ancestry::Diverged
    } else {
        Ancestry::Unknown
    }
}

fn valid_code_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_CODE_PATH_BYTES
        && !path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').count() <= MAX_CODE_PATH_COMPONENTS
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use git_internal::internal::object::blob::Blob;

    use super::*;

    #[derive(Default)]
    struct FakeHistory {
        parents: HashMap<ObjectHash, Vec<ObjectHash>>,
        entries: HashMap<(ObjectHash, String), Option<TreeEntryFingerprint>>,
    }

    impl CodeHistory for FakeHistory {
        fn parents(&self, commit: ObjectHash) -> Result<Vec<ObjectHash>, ()> {
            self.parents.get(&commit).cloned().ok_or(())
        }

        fn entry(
            &self,
            commit: ObjectHash,
            path: &str,
        ) -> Result<Option<TreeEntryFingerprint>, ()> {
            self.entries
                .get(&(commit, path.to_string()))
                .copied()
                .ok_or(())
        }
    }

    fn oid(seed: &str) -> ObjectHash {
        Blob::from_content(seed).id
    }

    fn code(result: ObjectHash) -> EpisodeCodeContextV1 {
        EpisodeCodeContextV1 {
            base_oid: None,
            result_oid: Some(result.to_string()),
            branch_ref: Some("refs/heads/main".to_string()),
            paths: vec!["src/lib.rs".to_string()],
        }
    }

    #[test]
    fn applicability_distinguishes_exact_unchanged_changed_and_diverged() {
        let result = oid("result");
        let current = oid("current");
        let other = oid("other");
        let blob = oid("same-entry");
        let changed_blob = oid("changed-entry");
        let mut history = FakeHistory::default();
        history.parents.insert(current, vec![result]);
        history.parents.insert(result, Vec::new());
        history.parents.insert(other, Vec::new());
        history.entries.insert(
            (result, "src/lib.rs".to_string()),
            Some(TreeEntryFingerprint::new(blob, TreeItemMode::Blob)),
        );
        history.entries.insert(
            (current, "src/lib.rs".to_string()),
            Some(TreeEntryFingerprint::new(blob, TreeItemMode::Blob)),
        );
        assert_eq!(
            classify_code_applicability(&history, result, &code(result)),
            CodeApplicability::Exact
        );
        assert_eq!(
            classify_code_applicability(&history, current, &code(result)),
            CodeApplicability::DescendantUnchanged
        );
        history.entries.insert(
            (current, "src/lib.rs".to_string()),
            Some(TreeEntryFingerprint::new(changed_blob, TreeItemMode::Blob)),
        );
        assert_eq!(
            classify_code_applicability(&history, current, &code(result)),
            CodeApplicability::DescendantPathChanged
        );
        assert_eq!(
            classify_code_applicability(&history, other, &code(result)),
            CodeApplicability::Diverged
        );
    }

    #[test]
    fn missing_anchor_or_path_is_unknown() {
        let result = oid("result");
        let current = oid("current");
        let mut history = FakeHistory::default();
        history.parents.insert(current, vec![result]);
        history.parents.insert(result, Vec::new());
        history
            .entries
            .insert((result, "src/lib.rs".to_string()), None);
        assert_eq!(
            classify_code_applicability(&history, current, &code(result)),
            CodeApplicability::Unknown
        );
        let mut no_paths = code(result);
        no_paths.paths.clear();
        assert_eq!(
            classify_code_applicability(&history, current, &no_paths),
            CodeApplicability::Unknown
        );
    }
}
