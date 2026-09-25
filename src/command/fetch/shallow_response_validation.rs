//! Validate shallow response metadata against the fetched commit graph before
//! publishing either shallow boundaries or refs.

use std::collections::{BTreeSet, HashMap, HashSet};

use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash},
    internal::object::types::ObjectType,
};

use super::FetchError;
use crate::utils::client_storage::{ClientStorage, ObjectReadFailure};

const MAX_WALK_NODES_OR_EDGES: usize = 262_144;
const MAX_WANT_OBJECT_INSPECTIONS: usize = 16_384;
const MAX_WANT_READ_BYTES: u64 = 256 * 1024 * 1024;
const COMMIT_BATCH_SIZE: usize = 16;
const MAX_TAG_BYTES: u64 = 1024 * 1024;
const MAX_TAG_PEEL_DEPTH: usize = 32;

/// A `shallow` line is valid only when its commit is at most `depth - 1`
/// parent edges from a requested commit. Annotated tag wants seed the walk at
/// their peeled commit. The traversal stops as soon as every marker is found.
pub(super) fn validate_response_shallow_updates(
    storage: &ClientStorage,
    wanted: &[String],
    depth: Option<usize>,
    prior_shallow: &BTreeSet<String>,
    response_shallow: &[String],
    response_unshallow: &[String],
    hash_kind: HashKind,
) -> Result<(), FetchError> {
    let mut remaining = validate_metadata(
        depth,
        prior_shallow,
        response_shallow,
        response_unshallow,
        hash_kind,
    )?;
    if remaining.is_empty() {
        return Ok(());
    }
    // Validate the marker objects even if a marker happens to coincide with a
    // wanted tip and the graph traversal would otherwise stop immediately.
    let marker_ids = remaining.iter().copied().collect::<Vec<_>>();
    for batch in marker_ids.chunks(COMMIT_BATCH_SIZE) {
        let marker_commits = storage
            .commit_parents_many(batch)
            .map_err(|source| classify_read_error(source, "shallow marker commits", true))?;
        if batch.iter().any(|oid| !marker_commits.contains_key(oid)) {
            return Err(FetchError::LocalState {
                message: "commit inspection omitted a shallow marker result".to_string(),
            });
        }
    }

    // Old boundaries stay terminal unless this response explicitly unshallows
    // them. Walking their raw parent IDs could otherwise demand history the
    // repository is intentionally allowed to omit.
    let stop_boundaries =
        effective_stop_boundaries(prior_shallow, response_unshallow, &marker_ids, hash_kind)?;

    let mut wanted_ids = wanted
        .iter()
        .map(|raw| parse_oid(raw, hash_kind, "requested object", false))
        .collect::<Result<HashSet<_>, _>>()?;
    // Marker objects were already parsed as commits above. If every marker is
    // requested directly, no other wanted object needs to be inspected.
    if remove_direct_wanted_markers(&mut remaining, &wanted_ids) {
        return Ok(());
    }
    // A wanted tip that remains shallow is terminal and cannot lead to a
    // different response marker. It needs neither another type probe nor a
    // parent walk. Explicitly unshallowed prior tips are absent from this set.
    wanted_ids.retain(|oid| !stop_boundaries.contains(oid));
    let mut visited = HashSet::new();
    let mut frontier = Vec::new();
    let mut want_read_budget = WantReadBudget::default();
    seed_wanted_commits(
        storage,
        wanted_ids,
        hash_kind,
        &mut want_read_budget,
        &mut remaining,
        &mut visited,
        &mut frontier,
    )?;
    if remaining.is_empty() {
        return Ok(());
    }

    let max_distance = depth.unwrap_or_default().saturating_sub(1);
    let mut edges_seen = 0usize;
    for _ in 0..max_distance {
        if frontier.is_empty() {
            break;
        }
        let mut next_frontier = Vec::new();
        let expandable = frontier
            .into_iter()
            .filter(|commit| !stop_boundaries.contains(commit))
            .collect::<Vec<_>>();
        for batch in expandable.chunks(COMMIT_BATCH_SIZE) {
            let commits = storage
                .commit_parents_many(batch)
                .map_err(|source| classify_read_error(source, "fetched commit ancestry", false))?;
            for child in batch {
                let parents = commits.get(child).ok_or_else(|| FetchError::LocalState {
                    message: format!("commit inspection omitted fetched commit '{child}'"),
                })?;
                edges_seen = edges_seen.saturating_add(parents.len());
                if edges_seen > MAX_WALK_NODES_OR_EDGES {
                    return Err(walk_limit_error("commit parent edges"));
                }
                for parent in parents {
                    if !visited.insert(*parent) {
                        continue;
                    }
                    if visited.len() > MAX_WALK_NODES_OR_EDGES {
                        return Err(walk_limit_error("commits"));
                    }
                    remaining.remove(parent);
                    if remaining.is_empty() {
                        return Ok(());
                    }
                    next_frontier.push(*parent);
                }
            }
        }
        frontier = next_frontier;
    }

    let first = remaining
        .iter()
        .next()
        .ok_or_else(|| FetchError::LocalState {
            message: "shallow marker validation lost its pending markers".to_string(),
        })?;
    Err(FetchError::InvalidShallowResponse {
        reason: format!(
            "shallow commit '{first}' is not within {max_distance} parent edges of any requested commit"
        ),
    })
}

fn remove_direct_wanted_markers(
    remaining: &mut HashSet<ObjectHash>,
    wanted: &HashSet<ObjectHash>,
) -> bool {
    remaining.retain(|marker| !wanted.contains(marker));
    remaining.is_empty()
}

fn effective_stop_boundaries(
    prior_shallow: &BTreeSet<String>,
    response_unshallow: &[String],
    new_shallow: &[ObjectHash],
    hash_kind: HashKind,
) -> Result<HashSet<ObjectHash>, FetchError> {
    let mut boundaries = prior_shallow
        .iter()
        .map(|raw| {
            ObjectHash::from_hex_for_kind(hash_kind, raw).map_err(|source| FetchError::LocalState {
                message: format!("invalid prior shallow boundary '{raw}': {source}"),
            })
        })
        .collect::<Result<HashSet<_>, _>>()?;
    for raw in response_unshallow {
        let unshallowed = parse_oid(raw, hash_kind, "unshallow marker", true)?;
        boundaries.remove(&unshallowed);
    }
    boundaries.extend(new_shallow);
    Ok(boundaries)
}

#[derive(Default)]
struct WantReadBudget {
    objects: usize,
    bytes: u64,
}

impl WantReadBudget {
    fn inspect_next(&mut self) -> Result<(), FetchError> {
        self.objects = self.objects.saturating_add(1);
        if self.objects > MAX_WANT_OBJECT_INSPECTIONS {
            return Err(FetchError::InvalidShallowResponse {
                reason: format!(
                    "validating shallow markers requires inspecting more than {MAX_WANT_OBJECT_INSPECTIONS} requested objects or tag targets; fetch fewer refs or narrow the refspec"
                ),
            });
        }
        Ok(())
    }

    fn remaining_bytes(&self) -> u64 {
        MAX_WANT_READ_BYTES.saturating_sub(self.bytes)
    }

    fn account_bytes(&mut self, bytes: u64) -> Result<(), FetchError> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > MAX_WANT_READ_BYTES {
            return Err(FetchError::InvalidShallowResponse {
                reason: format!(
                    "validating shallow markers requires reading more than {MAX_WANT_READ_BYTES} object bytes; fetch fewer refs"
                ),
            });
        }
        Ok(())
    }
}

fn validate_metadata(
    depth: Option<usize>,
    prior_shallow: &BTreeSet<String>,
    response_shallow: &[String],
    response_unshallow: &[String],
    hash_kind: HashKind,
) -> Result<HashSet<ObjectHash>, FetchError> {
    match depth {
        None if !response_shallow.is_empty() || !response_unshallow.is_empty() => {
            return Err(FetchError::InvalidShallowResponse {
                reason: "remote sent shallow or unshallow lines without a depth request"
                    .to_string(),
            });
        }
        Some(0) => {
            return Err(FetchError::InvalidShallowResponse {
                reason: "a depth request must be greater than zero".to_string(),
            });
        }
        _ => {}
    }

    let shallow = response_shallow
        .iter()
        .map(|oid| parse_oid(oid, hash_kind, "shallow marker", true))
        .collect::<Result<HashSet<_>, _>>()?;
    for raw_oid in response_unshallow {
        let oid = parse_oid(raw_oid, hash_kind, "unshallow marker", true)?;
        if !prior_shallow.contains(&oid.to_string()) {
            return Err(FetchError::InvalidShallowResponse {
                reason: format!("unshallow commit '{oid}' was not a prior shallow boundary"),
            });
        }
        if shallow.contains(&oid) {
            return Err(FetchError::InvalidShallowResponse {
                reason: format!("commit '{oid}' was marked both shallow and unshallow"),
            });
        }
    }
    Ok(shallow)
}

struct PendingWant {
    root: ObjectHash,
    current: ObjectHash,
    expected_type: Option<ObjectType>,
    seen: Vec<ObjectHash>,
}

fn seed_wanted_commits(
    storage: &ClientStorage,
    wanted: HashSet<ObjectHash>,
    hash_kind: HashKind,
    budget: &mut WantReadBudget,
    remaining: &mut HashSet<ObjectHash>,
    visited: &mut HashSet<ObjectHash>,
    frontier: &mut Vec<ObjectHash>,
) -> Result<(), FetchError> {
    let mut pending = wanted
        .into_iter()
        .map(|root| PendingWant {
            root,
            current: root,
            expected_type: None,
            seen: vec![root],
        })
        .collect::<Vec<_>>();
    let mut parsed_tags = HashMap::new();
    for _ in 0..MAX_TAG_PEEL_DEPTH {
        if pending.is_empty() {
            return Ok(());
        }
        let round_ids = pending
            .iter()
            .map(|request| request.current)
            .collect::<HashSet<_>>();
        for _ in &round_ids {
            budget.inspect_next()?;
        }
        let round_ids = round_ids.into_iter().collect::<Vec<_>>();
        // One storage call per peel round preserves the backend's aggregate
        // read cap across every wanted OID in this round.
        let types = probe_round_types(&round_ids, budget, |ids, remaining_bytes| {
            storage.get_object_types_bounded_many_with_budget(ids, remaining_bytes)
        })?;

        let mut next = Vec::new();
        for mut request in pending {
            let actual_type = match types.get(&request.current) {
                Some(actual_type) => *actual_type,
                None => match storage.get_object_type_bounded(&request.current) {
                    Ok(_) => {
                        return Err(FetchError::LocalState {
                            message: format!(
                                "bounded type batch omitted requested object '{}'",
                                request.current
                            ),
                        });
                    }
                    Err(source) => {
                        return Err(classify_read_error(
                            source,
                            "requested object or tag",
                            false,
                        ));
                    }
                },
            };
            if let Some(expected) = request.expected_type
                && expected != actual_type
            {
                return Err(FetchError::IncompleteFetchedHistory {
                    message: format!(
                        "annotated tag in requested object '{}' declares {expected} target '{}', but the object is {actual_type}",
                        request.root, request.current
                    ),
                });
            }
            match actual_type {
                ObjectType::Commit => {
                    if visited.insert(request.current) {
                        if visited.len() > MAX_WALK_NODES_OR_EDGES {
                            return Err(walk_limit_error("commits"));
                        }
                        remaining.remove(&request.current);
                        if remaining.is_empty() {
                            return Ok(());
                        }
                        frontier.push(request.current);
                    }
                }
                ObjectType::Tag => {
                    let (target, declared_type) =
                        if let Some(parsed) = parsed_tags.get(&request.current) {
                            *parsed
                        } else {
                            let (bytes, loaded_type) = storage
                                .get_typed_with_limit(&request.current, MAX_TAG_BYTES)
                                .map_err(|source| {
                                    classify_read_error(source, "requested tag", false)
                                })?;
                            if loaded_type != ObjectType::Tag {
                                return Err(invalid_tag(
                                    request.current,
                                    "object type changed while inspecting the tag",
                                ));
                            }
                            budget.account_bytes(bytes.len() as u64)?;
                            let parsed = parse_tag_target(&bytes, request.current, hash_kind)?;
                            parsed_tags.insert(request.current, parsed);
                            parsed
                        };
                    if request.seen.contains(&target) {
                        return Err(FetchError::IncompleteFetchedHistory {
                            message: format!(
                                "annotated tag chain for requested object '{}' contains a cycle",
                                request.root
                            ),
                        });
                    }
                    request.current = target;
                    request.expected_type = Some(declared_type);
                    request.seen.push(target);
                    next.push(request);
                }
                _ => {}
            }
        }
        pending = next;
    }
    if let Some(request) = pending.first() {
        return Err(FetchError::IncompleteFetchedHistory {
            message: format!(
                "annotated tag chain for requested object '{}' exceeds {MAX_TAG_PEEL_DEPTH} objects",
                request.root
            ),
        });
    }
    Ok(())
}

fn probe_round_types<F>(
    round_ids: &[ObjectHash],
    budget: &mut WantReadBudget,
    probe: F,
) -> Result<HashMap<ObjectHash, ObjectType>, FetchError>
where
    F: FnOnce(&[ObjectHash], u64) -> Result<(HashMap<ObjectHash, ObjectType>, u64), GitError>,
{
    let first = round_ids
        .first()
        .copied()
        .ok_or_else(|| FetchError::LocalState {
            message: "bounded type inspection received an empty request".to_string(),
        })?;
    let (types, remote_bytes) = probe(round_ids, budget.remaining_bytes())
        .map_err(|source| classify_type_probe_error(source, first))?;
    budget.account_bytes(remote_bytes)?;
    Ok(types)
}

fn classify_type_probe_error(source: GitError, oid: ObjectHash) -> FetchError {
    if matches!(&source, GitError::InvalidObjectInfo(_)) {
        return FetchError::InvalidShallowResponse {
            reason: format!(
                "cannot inspect requested object batch starting at '{oid}' within the bounded type check: {source}; fetch fewer refs or omit large blob or tree targets"
            ),
        };
    }
    classify_read_error(source, "requested object or tag", false)
}

fn parse_tag_target(
    bytes: &[u8],
    tag: ObjectHash,
    hash_kind: HashKind,
) -> Result<(ObjectHash, ObjectType), FetchError> {
    let mut lines = bytes.split(|byte| *byte == b'\n');
    let target = lines
        .next()
        .and_then(|line| line.strip_prefix(b"object "))
        .and_then(|value| std::str::from_utf8(value).ok())
        .ok_or_else(|| invalid_tag(tag, "missing object header"))?;
    let target = ObjectHash::from_hex_for_kind(hash_kind, target)
        .map_err(|source| invalid_tag(tag, &format!("invalid target object ID: {source}")))?;
    let declared_type = lines
        .next()
        .and_then(|line| line.strip_prefix(b"type "))
        .ok_or_else(|| invalid_tag(tag, "missing type header"))?;
    let declared_type = match declared_type {
        b"commit" => ObjectType::Commit,
        b"tag" => ObjectType::Tag,
        b"tree" => ObjectType::Tree,
        b"blob" => ObjectType::Blob,
        _ => return Err(invalid_tag(tag, "unknown target object type")),
    };
    Ok((target, declared_type))
}

fn invalid_tag(tag: ObjectHash, reason: &str) -> FetchError {
    FetchError::IncompleteFetchedHistory {
        message: format!("requested annotated tag '{tag}' is invalid: {reason}"),
    }
}

fn parse_oid(
    raw: &str,
    hash_kind: HashKind,
    label: &str,
    response_marker: bool,
) -> Result<ObjectHash, FetchError> {
    ObjectHash::from_hex_for_kind(hash_kind, raw).map_err(|source| {
        let reason = format!("invalid {label} '{raw}': {source}");
        if response_marker {
            FetchError::InvalidShallowResponse { reason }
        } else {
            FetchError::IncompleteFetchedHistory { message: reason }
        }
    })
}

fn classify_read_error(source: GitError, context: &str, response_marker: bool) -> FetchError {
    match ClientStorage::classify_read_failure(&source) {
        ObjectReadFailure::Unavailable | ObjectReadFailure::Other => FetchError::LocalState {
            message: format!("failed to inspect {context}: {source}"),
        },
        ObjectReadFailure::Missing | ObjectReadFailure::Corrupt | ObjectReadFailure::TooLarge => {
            let reason = format!("remote supplied missing or invalid {context}: {source}");
            if response_marker {
                FetchError::InvalidShallowResponse { reason }
            } else {
                FetchError::IncompleteFetchedHistory { message: reason }
            }
        }
    }
}

fn walk_limit_error(resource: &str) -> FetchError {
    FetchError::InvalidShallowResponse {
        reason: format!(
            "validating shallow markers requires more than {MAX_WALK_NODES_OR_EDGES} {resource}; fetch fewer refs or use a smaller depth"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use git_internal::hash::{HashKind, ObjectHash};

    use super::{
        MAX_WANT_OBJECT_INSPECTIONS, MAX_WANT_READ_BYTES, WantReadBudget,
        classify_type_probe_error, effective_stop_boundaries, parse_tag_target, probe_round_types,
        remove_direct_wanted_markers, validate_metadata,
    };
    use crate::command::fetch::FetchError;

    const OID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn rejects_shallow_lines_without_depth() {
        let result = validate_metadata(
            None,
            &BTreeSet::new(),
            &[OID_A.to_string()],
            &[],
            HashKind::Sha1,
        );
        assert!(matches!(
            result,
            Err(FetchError::InvalidShallowResponse { .. })
        ));

        let unshallow = validate_metadata(
            None,
            &BTreeSet::from([OID_A.to_string()]),
            &[],
            &[OID_A.to_string()],
            HashKind::Sha1,
        );
        assert!(matches!(
            unshallow,
            Err(FetchError::InvalidShallowResponse { .. })
        ));
    }

    #[test]
    fn rejects_unshallow_outside_prior_boundaries_and_conflicts() {
        let no_prior = validate_metadata(
            Some(2),
            &BTreeSet::new(),
            &[],
            &[OID_A.to_string()],
            HashKind::Sha1,
        );
        assert!(matches!(
            no_prior,
            Err(FetchError::InvalidShallowResponse { .. })
        ));

        let prior = BTreeSet::from([OID_A.to_string()]);
        let conflict = validate_metadata(
            Some(2),
            &prior,
            &[OID_A.to_string()],
            &[OID_A.to_string()],
            HashKind::Sha1,
        );
        assert!(matches!(
            conflict,
            Err(FetchError::InvalidShallowResponse { .. })
        ));

        let zero_depth = validate_metadata(Some(0), &prior, &[], &[], HashKind::Sha1);
        assert!(matches!(
            zero_depth,
            Err(FetchError::InvalidShallowResponse { .. })
        ));

        let wrong_kind =
            validate_metadata(Some(1), &prior, &[OID_A.to_string()], &[], HashKind::Sha256);
        assert!(matches!(
            wrong_kind,
            Err(FetchError::InvalidShallowResponse { .. })
        ));
    }

    #[test]
    fn tag_target_uses_explicit_hash_kind_and_required_headers() {
        let tag = ObjectHash::from_hex_for_kind(HashKind::Sha1, OID_A).expect("valid test tag ID");
        let valid = format!("object {OID_B}\ntype commit\ntag example\n\nmessage");
        let (target, kind) =
            parse_tag_target(valid.as_bytes(), tag, HashKind::Sha1).expect("valid tag target");
        assert_eq!(target.to_string(), OID_B);
        assert_eq!(
            kind,
            git_internal::internal::object::types::ObjectType::Commit
        );

        assert!(parse_tag_target(valid.as_bytes(), tag, HashKind::Sha256).is_err());
        assert!(parse_tag_target(b"object bad\ntype commit\n", tag, HashKind::Sha1).is_err());
    }

    #[test]
    fn want_inspection_budget_is_cumulative() {
        let mut budget = WantReadBudget {
            objects: MAX_WANT_OBJECT_INSPECTIONS,
            bytes: 0,
        };
        assert!(matches!(
            budget.inspect_next(),
            Err(FetchError::InvalidShallowResponse { .. })
        ));

        let mut budget = WantReadBudget {
            objects: 0,
            bytes: MAX_WANT_READ_BYTES - 1,
        };
        budget.account_bytes(1).expect("within test byte budget");
        assert!(matches!(
            budget.account_bytes(1),
            Err(FetchError::InvalidShallowResponse { .. })
        ));
    }

    #[test]
    fn bounded_type_probe_distinguishes_invalid_object_from_local_io() {
        let oid = ObjectHash::from_hex_for_kind(HashKind::Sha1, OID_A).expect("valid test ID");
        let invalid = classify_type_probe_error(
            git_internal::errors::GitError::InvalidObjectInfo("too large".to_string()),
            oid,
        );
        assert!(matches!(invalid, FetchError::InvalidShallowResponse { .. }));

        let io = classify_type_probe_error(
            git_internal::errors::GitError::IOError(std::io::Error::other("offline")),
            oid,
        );
        assert!(matches!(io, FetchError::LocalState { .. }));
    }

    #[test]
    fn retained_prior_boundaries_stop_the_walk_and_unshallowed_ones_do_not() {
        let prior = BTreeSet::from([OID_A.to_string()]);
        let new_shallow =
            ObjectHash::from_hex_for_kind(HashKind::Sha1, OID_B).expect("valid test ID");
        let retained = effective_stop_boundaries(&prior, &[], &[new_shallow], HashKind::Sha1)
            .expect("valid boundaries");
        assert_eq!(retained.len(), 2);

        let deepened =
            effective_stop_boundaries(&prior, &[OID_A.to_string()], &[new_shallow], HashKind::Sha1)
                .expect("valid boundaries");
        assert_eq!(deepened.len(), 1);
        assert!(deepened.contains(&new_shallow));
    }

    #[test]
    fn directly_wanted_shallow_markers_need_no_other_want_inspection() {
        let marker = ObjectHash::from_hex_for_kind(HashKind::Sha1, OID_A).expect("valid test ID");
        let unrelated =
            ObjectHash::from_hex_for_kind(HashKind::Sha1, OID_B).expect("valid test ID");
        let wanted = std::collections::HashSet::from([marker, unrelated]);
        let mut remaining = std::collections::HashSet::from([marker]);
        assert!(remove_direct_wanted_markers(&mut remaining, &wanted));
        assert!(remaining.is_empty());
    }

    #[test]
    fn one_type_probe_receives_the_entire_peel_round() {
        let first = ObjectHash::from_hex_for_kind(HashKind::Sha1, OID_A).expect("valid test ID");
        let second = ObjectHash::from_hex_for_kind(HashKind::Sha1, OID_B).expect("valid test ID");
        let ids = [first, second];
        let calls = std::cell::Cell::new(0);
        let mut budget = WantReadBudget::default();
        let types = probe_round_types(&ids, &mut budget, |received, remaining| {
            calls.set(calls.get() + 1);
            assert_eq!(received, &ids);
            assert_eq!(remaining, MAX_WANT_READ_BYTES);
            Ok((
                std::collections::HashMap::from([
                    (
                        first,
                        git_internal::internal::object::types::ObjectType::Commit,
                    ),
                    (
                        second,
                        git_internal::internal::object::types::ObjectType::Tag,
                    ),
                ]),
                128,
            ))
        })
        .expect("one successful batch");
        assert_eq!(calls.get(), 1);
        assert_eq!(types.len(), 2);
        assert_eq!(budget.remaining_bytes(), MAX_WANT_READ_BYTES - 128);
    }

    #[test]
    fn remote_type_probe_bytes_accumulate_across_peel_rounds() {
        let oid = ObjectHash::from_hex_for_kind(HashKind::Sha1, OID_A).expect("valid test ID");
        let mut budget = WantReadBudget::default();
        let first = probe_round_types(&[oid], &mut budget, |_, remaining| {
            assert_eq!(remaining, MAX_WANT_READ_BYTES);
            Ok((std::collections::HashMap::new(), MAX_WANT_READ_BYTES - 1))
        });
        assert!(first.is_ok());
        let second = probe_round_types(&[oid], &mut budget, |_, remaining| {
            assert_eq!(remaining, 1);
            Ok((std::collections::HashMap::new(), 2))
        });
        assert!(matches!(
            second,
            Err(FetchError::InvalidShallowResponse { .. })
        ));
    }
}
