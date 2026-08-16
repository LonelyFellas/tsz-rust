use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use uuid::Uuid;

use crate::lexicon::repository::SurfaceProjectionSource;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct VisibilityScope {
    pub language: String,
    pub entry_kind: String,
    pub dialect_scope: String,
    pub normalized_headword: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct VisibilityTransition {
    pub scope: VisibilityScope,
    pub before_active_ids: BTreeSet<Uuid>,
    pub after_active_ids: BTreeSet<Uuid>,
    pub new_ids: BTreeSet<Uuid>,
}

pub(crate) fn transitions(
    before: impl IntoIterator<Item = (VisibilityScope, Uuid)>,
    removals: impl IntoIterator<Item = (VisibilityScope, Uuid)>,
    additions: impl IntoIterator<Item = (VisibilityScope, Uuid)>,
) -> Vec<VisibilityTransition> {
    let mut before_by_scope = BTreeMap::<VisibilityScope, BTreeSet<Uuid>>::new();
    for (scope, id) in before {
        before_by_scope.entry(scope).or_default().insert(id);
    }
    let mut after_by_scope = before_by_scope.clone();
    for (scope, id) in removals {
        after_by_scope.entry(scope).or_default().remove(&id);
    }
    for (scope, id) in additions {
        after_by_scope.entry(scope).or_default().insert(id);
    }
    for scope in before_by_scope.keys() {
        after_by_scope.entry(scope.clone()).or_default();
    }
    after_by_scope
        .into_iter()
        .map(|(scope, after_active_ids)| {
            let before_active_ids = before_by_scope.remove(&scope).unwrap_or_default();
            let new_ids = after_active_ids
                .difference(&before_active_ids)
                .copied()
                .collect();
            VisibilityTransition {
                scope,
                before_active_ids,
                after_active_ids,
                new_ids,
            }
        })
        .collect()
}

pub(crate) fn requires_multiple_active_confirmation(transitions: &[VisibilityTransition]) -> bool {
    transitions
        .iter()
        .any(|transition| !transition.new_ids.is_empty() && transition.after_active_ids.len() > 1)
}

pub(crate) fn headword_memberships(
    sources: &[SurfaceProjectionSource],
) -> Vec<(VisibilityScope, Uuid)> {
    sources
        .iter()
        .filter(|source| source.source_kind == "headword")
        .map(|source| {
            (
                VisibilityScope {
                    language: source.language.clone(),
                    entry_kind: source.entry_kind.to_owned(),
                    dialect_scope: source.dialect_scope.to_owned(),
                    normalized_headword: source.normalized_surface.clone(),
                },
                source.entry_id,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> VisibilityScope {
        VisibilityScope {
            language: "en".into(),
            entry_kind: "word".into(),
            dialect_scope: "common".into(),
            normalized_headword: "workspace".into(),
        }
    }

    #[test]
    fn zero_to_one_is_allowed_and_one_to_two_requires_gate() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let zero_to_one = transitions([], [], [(scope(), first)]);
        assert!(!requires_multiple_active_confirmation(&zero_to_one));
        assert_eq!(zero_to_one[0].new_ids, BTreeSet::from([first]));

        let one_to_two = transitions([(scope(), first)], [], [(scope(), second)]);
        assert!(requires_multiple_active_confirmation(&one_to_two));
        assert_eq!(one_to_two[0].new_ids, BTreeSet::from([second]));
    }

    #[test]
    fn zero_to_two_batch_is_evaluated_as_one_atomic_transition() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let result = transitions([], [], [(scope(), first), (scope(), second)]);
        assert!(requires_multiple_active_confirmation(&result));
        assert_eq!(result[0].after_active_ids, BTreeSet::from([first, second]));
    }

    #[test]
    fn publishing_a_new_revision_of_the_same_active_entry_adds_no_id() {
        let entry = Uuid::now_v7();
        let result = transitions([(scope(), entry)], [(scope(), entry)], [(scope(), entry)]);
        assert!(result[0].new_ids.is_empty());
        assert!(!requires_multiple_active_confirmation(&result));
    }

    #[test]
    fn membership_move_between_scopes_uses_complete_before_and_after_sets() {
        let entry = Uuid::now_v7();
        let other = Uuid::now_v7();
        let mut changed = scope();
        changed.normalized_headword = "workspaces".into();
        let result = transitions(
            [(scope(), entry), (changed.clone(), other)],
            [(scope(), entry)],
            [(changed.clone(), entry)],
        );
        let changed = result.iter().find(|item| item.scope == changed).unwrap();
        assert_eq!(changed.before_active_ids, BTreeSet::from([other]));
        assert_eq!(changed.after_active_ids, BTreeSet::from([entry, other]));
        assert_eq!(changed.new_ids, BTreeSet::from([entry]));
        assert!(requires_multiple_active_confirmation(&result));
    }
}
