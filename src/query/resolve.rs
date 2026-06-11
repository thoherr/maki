//! Shared "ParsedSearch → SearchOptions" ID resolution for collection and
//! person filters.
//!
//! `collection:` / `-collection:` and `person:` / `-person:` filters can't be
//! evaluated inside the main search SQL directly — they resolve names to
//! asset-ID lists first (collections via `collection_assets`, persons via the
//! `faces`/`people` tables). Historically the CLI (`QueryEngine::search`), the
//! web handlers (`web::routes::ResolvedSearch`), and the web export-zip
//! handler each carried their own copy of this resolution block, and the
//! copies drifted. This module is the single implementation; every behavioral
//! difference between the consumers is an explicit parameter or a documented
//! call-site decision.
//!
//! # Drift matrix (historical behavior, preserved verbatim)
//!
//! | Consumer | Empty-filter policy | Person combine | Person exclude | Collection exclude |
//! |---|---|---|---|---|
//! | CLI `QueryEngine::search` | `MatchNothing` | `AnyOf` (OR-union) | yes | yes |
//! | Web browse family (browse_page, search_api, all_ids/page_ids, facets) | `Ignore` | `AllOf` (intersection) | **no** | yes |
//! | Web calendar_api / map_api | `MatchNothing` | `AllOf` (intersection) | **no** | yes |
//! | Web export-zip (`media.rs`) | `MatchNothing` | `AllOf` (intersection) | **no** | **no** |
//!
//! The two **no** columns look like plain gaps rather than design decisions:
//! a `-person:Alice` query in the web UI is silently ignored (the CLI honors
//! it), and the export-zip handler additionally ignores `-collection:` — so
//! exporting a filtered browse view that uses `-collection:` can export
//! *more* assets than the grid shows. Both are preserved here because fixing
//! them would change result sets; the consumers opt out explicitly by calling
//! the granular `apply_*` methods instead of [`ResolvedFilterIds::apply`].
//!
//! Within a single filter entry, a comma is always OR (`person:Alice,Bob` =
//! either) — all consumers agree on that. The drift is across *separate*
//! entries (`person:Alice person:Bob`), captured by [`PersonCombine`].
//! Collection entries are OR-unioned across entries by every consumer.

use crate::catalog::{Catalog, SearchOptions};

use super::ParsedSearch;

/// How [`ResolvedFilterIds::apply`] treats a filter that was present in the
/// query but resolved to zero asset IDs.
///
/// The two policies encode a historical behavioral drift between consumers —
/// preserved verbatim by the dedup:
/// - The web browse family (browse_page / search_api / all_ids_api /
///   page_ids_api / facets_api) installed the resolved list only when it was
///   non-empty, so a collection/person filter that matched nothing was
///   silently ignored (and in non-`ai` builds, person filters were always
///   ignored). That is [`EmptyFilterPolicy::Ignore`].
/// - The CLI (`QueryEngine::search`), web calendar_api / map_api, and the web
///   export-zip handler installed `Some(&ids)` whenever the filter was
///   present in the parsed query, so a filter resolving to zero IDs matched
///   nothing (and in non-`ai` builds, a person filter matched nothing). That
///   is [`EmptyFilterPolicy::MatchNothing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyFilterPolicy {
    /// Skip installing an empty resolved list — the filter is silently
    /// ignored (web browse/search/all-ids/page-ids/facets behavior).
    Ignore,
    /// Install the empty slice so the filter matches nothing
    /// (CLI, web calendar/map, and web export-zip behavior).
    MatchNothing,
}

/// How multiple separate `person:` filter entries combine.
///
/// This documents a discovered drift between the CLI and the web layer; it
/// is historical behavior made explicit, **not** a design endorsement:
/// - The CLI (`QueryEngine::search`) uses [`PersonCombine::AnyOf`]:
///   `person:Alice person:Bob` OR-unions across entries — assets showing
///   *either* person match.
/// - The web layer (`ResolvedSearch` for browse/calendar/map, and the
///   export-zip handler) uses [`PersonCombine::AllOf`]: separate entries
///   intersect, matching the catalog's tag semantics — `person:Alice
///   person:Bob` returns assets that contain *both* people.
///
/// Both modes treat a comma *within* one entry as OR (`person:Alice,Bob` =
/// either person), so the modes only diverge when two or more `person:`
/// entries are present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonCombine {
    /// OR-union across separate entries (CLI behavior).
    AnyOf,
    /// Intersection across separate entries (web behavior).
    AllOf,
}

/// Owned holder for the resolved collection/person asset-ID lists.
///
/// `SearchOptions<'a>` borrows `&[String]` slices, so the resolved Vecs must
/// outlive the options struct; this type owns them. Usage:
/// 1. [`ResolvedFilterIds::resolve`] resolves all four filters
///    (collection/person, include/exclude) from a [`ParsedSearch`].
/// 2. [`ResolvedFilterIds::apply`] installs all four into a `SearchOptions`
///    per the [`EmptyFilterPolicy`]; consumers that historically did not
///    support some of the filters (see the module-level drift matrix) call
///    the granular `apply_*` methods instead.
pub struct ResolvedFilterIds {
    pub collection_ids: Vec<String>,
    pub collection_exclude_ids: Vec<String>,
    pub person_ids: Vec<String>,
    pub person_exclude_ids: Vec<String>,
    /// Whether the parsed query contained a `collection:` filter at all —
    /// drives the `MatchNothing` install of an empty resolved list.
    has_collections: bool,
    has_collection_excludes: bool,
    has_persons: bool,
    has_person_excludes: bool,
    policy: EmptyFilterPolicy,
}

impl ResolvedFilterIds {
    /// Resolve collection include/exclude and person include/exclude filters
    /// from `parsed` against the catalog.
    ///
    /// Collection entries are OR-unioned across entries and within comma
    /// lists (all consumers agree). Person entries combine per
    /// `person_combine`; person *exclude* entries use the same combine mode
    /// (the CLI, the only consumer that installs them, OR-unions both).
    ///
    /// In non-`ai` builds person name resolution is unavailable: person ID
    /// lists resolve to empty, and the `has_*` flags still reflect the parsed
    /// query — so under [`EmptyFilterPolicy::MatchNothing`] a `person:` filter
    /// matches nothing, and under [`EmptyFilterPolicy::Ignore`] it is silently
    /// ignored. Both match the consumers' historical non-`ai` behavior.
    pub fn resolve(
        catalog: &Catalog,
        parsed: &ParsedSearch,
        policy: EmptyFilterPolicy,
        person_combine: PersonCombine,
    ) -> Self {
        let collection_ids = resolve_collection_ids(&parsed.collections, catalog.conn());
        let collection_exclude_ids =
            resolve_collection_ids(&parsed.collections_exclude, catalog.conn());

        #[cfg(feature = "ai")]
        let (person_ids, person_exclude_ids) = (
            resolve_person_ids(catalog, &parsed.persons, person_combine),
            resolve_person_ids(catalog, &parsed.persons_exclude, person_combine),
        );
        #[cfg(not(feature = "ai"))]
        let (person_ids, person_exclude_ids) = {
            let _ = person_combine; // only consumed by the ai-gated resolution
            (Vec::new(), Vec::new())
        };

        Self {
            collection_ids,
            collection_exclude_ids,
            person_ids,
            person_exclude_ids,
            has_collections: !parsed.collections.is_empty(),
            has_collection_excludes: !parsed.collections_exclude.is_empty(),
            has_persons: !parsed.persons.is_empty(),
            has_person_excludes: !parsed.persons_exclude.is_empty(),
            policy,
        }
    }

    fn install_empty(&self) -> bool {
        self.policy == EmptyFilterPolicy::MatchNothing
    }

    /// Install all four resolved ID lists into `opts` per the policy
    /// captured at [`Self::resolve`] time.
    ///
    /// A list is installed when it is non-empty, or when the filter was
    /// present in the query and the policy is
    /// [`EmptyFilterPolicy::MatchNothing`] (reproducing the CLI's historical
    /// unconditional install). Consumers that don't support all four filters
    /// (see the module-level drift matrix) call the granular methods.
    pub fn apply<'a>(&'a self, opts: &mut SearchOptions<'a>) {
        self.apply_collections(opts);
        self.apply_collection_excludes(opts);
        self.apply_persons(opts);
        self.apply_person_excludes(opts);
    }

    /// Install `collection:` include IDs per policy.
    pub fn apply_collections<'a>(&'a self, opts: &mut SearchOptions<'a>) {
        if !self.collection_ids.is_empty() || (self.install_empty() && self.has_collections) {
            opts.collection_asset_ids = Some(&self.collection_ids);
        }
    }

    /// Install `-collection:` exclude IDs per policy. Not called by the web
    /// export-zip handler (historical gap, see module docs).
    pub fn apply_collection_excludes<'a>(&'a self, opts: &mut SearchOptions<'a>) {
        if !self.collection_exclude_ids.is_empty()
            || (self.install_empty() && self.has_collection_excludes)
        {
            opts.collection_exclude_ids = Some(&self.collection_exclude_ids);
        }
    }

    /// Install `person:` include IDs per policy.
    pub fn apply_persons<'a>(&'a self, opts: &mut SearchOptions<'a>) {
        if !self.person_ids.is_empty() || (self.install_empty() && self.has_persons) {
            opts.person_asset_ids = Some(&self.person_ids);
        }
    }

    /// Install `-person:` exclude IDs per policy. Only the CLI calls this —
    /// the web layer has never supported person excludes (historical gap,
    /// see module docs).
    pub fn apply_person_excludes<'a>(&'a self, opts: &mut SearchOptions<'a>) {
        if !self.person_exclude_ids.is_empty() || (self.install_empty() && self.has_person_excludes)
        {
            opts.person_exclude_ids = Some(&self.person_exclude_ids);
        }
    }
}

/// Resolve a list of comma-OR'd collection name entries to asset IDs.
///
/// Each entry may be a comma-separated list (OR within entry). Entries are
/// also OR-unioned — collections don't AND like tags/persons (every consumer
/// agrees on this). Unknown collection names contribute nothing. Returns a
/// Vec of distinct asset IDs in arbitrary order; empty when `entries` is
/// empty or nothing matched.
fn resolve_collection_ids(entries: &[String], conn: &rusqlite::Connection) -> Vec<String> {
    if entries.is_empty() {
        return Vec::new();
    }
    let store = crate::collection::CollectionStore::new(conn);
    union_name_groups(entries, |name| {
        store.asset_ids_for_collection(name).unwrap_or_default()
    })
}

/// Resolve person name entries to asset IDs via the face store, combining
/// separate entries per `combine` (comma within an entry is always OR).
#[cfg(feature = "ai")]
fn resolve_person_ids(catalog: &Catalog, entries: &[String], combine: PersonCombine) -> Vec<String> {
    if entries.is_empty() {
        return Vec::new();
    }
    let face_store = crate::face_store::FaceStore::new(catalog.conn());
    let lookup = |name: &str| face_store.find_person_asset_ids(name).unwrap_or_default();
    match combine {
        PersonCombine::AnyOf => union_name_groups(entries, lookup),
        PersonCombine::AllOf => intersect_name_groups(entries, lookup),
    }
}

/// OR-union: every name in every entry contributes its IDs to one set
/// (CLI person semantics; collection semantics everywhere).
fn union_name_groups<F>(entries: &[String], lookup: F) -> Vec<String>
where
    F: Fn(&str) -> Vec<String>,
{
    let mut all_ids = std::collections::HashSet::new();
    for entry in entries {
        for name in split_names(entry) {
            all_ids.extend(lookup(name));
        }
    }
    all_ids.into_iter().collect()
}

/// Resolve a list of comma-OR'd / entry-ANDed name groups against a lookup
/// function and return the asset IDs that match all entries.
///
/// Matches the catalog's tag semantics: comma within an entry is OR ("any of
/// these names"), separate entries are AND ("must match all of these"). For
/// person filters this means `person:Alice person:Bob` returns assets that
/// contain BOTH Alice and Bob, while `person:Alice,Bob` returns assets that
/// contain EITHER. (Web person semantics — see [`PersonCombine`].)
#[cfg(any(feature = "ai", test))]
fn intersect_name_groups<F>(entries: &[String], lookup: F) -> Vec<String>
where
    F: Fn(&str) -> Vec<String>,
{
    let mut current: Option<std::collections::HashSet<String>> = None;
    for entry in entries {
        let mut group: std::collections::HashSet<String> = std::collections::HashSet::new();
        for name in split_names(entry) {
            for id in lookup(name) {
                group.insert(id);
            }
        }
        current = match current {
            None => Some(group),
            Some(prev) => Some(prev.intersection(&group).cloned().collect()),
        };
    }
    current.unwrap_or_default().into_iter().collect()
}

/// Split one filter entry on commas, trimming and dropping empty names.
fn split_names(entry: &str) -> impl Iterator<Item = &str> {
    entry.split(',').map(|s| s.trim()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catalog with three assets (`a1`–`a3` name lookup via returned Vec),
    /// a collection "Favs" = {a1}, "Picks" = {a2}.
    /// With the `ai` feature: person Alice on {a1, a2}, Bob on {a2, a3}.
    fn setup_catalog() -> (Catalog, Vec<String>) {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let mut ids = Vec::new();
        for i in 1..=3 {
            let hash = format!("sha256:resolve{i}");
            let mut asset =
                crate::models::Asset::new(crate::models::AssetType::Image, &hash);
            let variant = crate::models::Variant {
                content_hash: hash,
                asset_id: asset.id.clone(),
                role: crate::models::VariantRole::Original,
                format: "jpg".to_string(),
                file_size: 1000,
                original_filename: format!("resolve{i}.jpg"),
                source_metadata: Default::default(),
                locations: vec![],
            };
            asset.variants.push(variant.clone());
            catalog.insert_asset(&asset).unwrap();
            catalog.insert_variant(&variant).unwrap();
            ids.push(asset.id.to_string());
        }

        let store = crate::collection::CollectionStore::new(catalog.conn());
        store.create("Favs", None).unwrap();
        store.add_assets("Favs", &[ids[0].clone()]).unwrap();
        store.create("Picks", None).unwrap();
        store.add_assets("Picks", &[ids[1].clone()]).unwrap();

        #[cfg(feature = "ai")]
        {
            let faces = crate::face_store::FaceStore::new(catalog.conn());
            let alice = faces.create_person(Some("Alice")).unwrap();
            let bob = faces.create_person(Some("Bob")).unwrap();
            let emb = [0.5f32; 4];
            for (n, (asset_idx, person)) in [
                (0usize, &alice),
                (1usize, &alice),
                (1usize, &bob),
                (2usize, &bob),
            ]
            .iter()
            .enumerate()
            {
                let face_id = format!("face-{n}");
                faces
                    .store_face(&face_id, &ids[*asset_idx], 0.1, 0.1, 0.2, 0.2, &emb, 0.99, "test")
                    .unwrap();
                faces.assign_face_to_person(&face_id, person).unwrap();
            }
        }

        (catalog, ids)
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn no_filters_installs_nothing() {
        let (catalog, _ids) = setup_catalog();
        let parsed = ParsedSearch::default();
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::MatchNothing, PersonCombine::AnyOf,
        );
        let mut opts = SearchOptions::default();
        resolved.apply(&mut opts);
        assert!(opts.collection_asset_ids.is_none());
        assert!(opts.collection_exclude_ids.is_none());
        assert!(opts.person_asset_ids.is_none());
        assert!(opts.person_exclude_ids.is_none());
    }

    #[test]
    fn matched_collection_resolves_and_installs() {
        let (catalog, ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.collections.push("Favs".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::Ignore, PersonCombine::AllOf,
        );
        let mut opts = SearchOptions::default();
        resolved.apply(&mut opts);
        assert_eq!(opts.collection_asset_ids.unwrap(), &[ids[0].clone()]);
    }

    #[test]
    fn collection_comma_list_unions_within_entry() {
        let (catalog, ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.collections.push("Favs, Picks".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::Ignore, PersonCombine::AllOf,
        );
        assert_eq!(
            sorted(resolved.collection_ids.clone()),
            sorted(vec![ids[0].clone(), ids[1].clone()])
        );
    }

    #[test]
    fn collection_entries_union_across_entries() {
        // Collections OR across entries in every consumer (unlike web persons).
        let (catalog, ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.collections.push("Favs".to_string());
        parsed.collections.push("Picks".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::Ignore, PersonCombine::AllOf,
        );
        assert_eq!(
            sorted(resolved.collection_ids.clone()),
            sorted(vec![ids[0].clone(), ids[1].clone()])
        );
    }

    #[test]
    fn unmatched_collection_ignored_under_ignore_policy() {
        let (catalog, _ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.collections.push("NoSuchCollection".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::Ignore, PersonCombine::AllOf,
        );
        let mut opts = SearchOptions::default();
        resolved.apply(&mut opts);
        assert!(opts.collection_asset_ids.is_none(), "Ignore policy must skip empty list");
    }

    #[test]
    fn unmatched_collection_matches_nothing_under_match_nothing_policy() {
        let (catalog, _ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.collections.push("NoSuchCollection".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::MatchNothing, PersonCombine::AllOf,
        );
        let mut opts = SearchOptions::default();
        resolved.apply(&mut opts);
        assert_eq!(
            opts.collection_asset_ids.unwrap().len(),
            0,
            "MatchNothing policy must install the empty slice"
        );
    }

    #[test]
    fn collection_exclude_resolves_and_installs() {
        let (catalog, ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.collections_exclude.push("Picks".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::MatchNothing, PersonCombine::AnyOf,
        );
        let mut opts = SearchOptions::default();
        resolved.apply(&mut opts);
        assert_eq!(opts.collection_exclude_ids.unwrap(), &[ids[1].clone()]);
        assert!(opts.collection_asset_ids.is_none());
    }

    /// Unknown person resolves to empty in BOTH builds (no people fixture
    /// rows match; in non-ai builds resolution is skipped entirely) — so this
    /// locks the policy behavior identically with and without `ai`.
    #[test]
    fn unmatched_person_follows_policy_in_all_builds() {
        let (catalog, _ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.persons.push("NoSuchPerson".to_string());

        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::Ignore, PersonCombine::AllOf,
        );
        let mut opts = SearchOptions::default();
        resolved.apply(&mut opts);
        assert!(opts.person_asset_ids.is_none());

        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::MatchNothing, PersonCombine::AnyOf,
        );
        let mut opts = SearchOptions::default();
        resolved.apply(&mut opts);
        assert_eq!(opts.person_asset_ids.unwrap().len(), 0);
    }

    #[cfg(feature = "ai")]
    #[test]
    fn person_any_of_unions_across_entries() {
        let (catalog, ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.persons.push("Alice".to_string());
        parsed.persons.push("Bob".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::MatchNothing, PersonCombine::AnyOf,
        );
        assert_eq!(sorted(resolved.person_ids.clone()), sorted(ids.clone()));
    }

    #[cfg(feature = "ai")]
    #[test]
    fn person_all_of_intersects_across_entries() {
        let (catalog, ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.persons.push("Alice".to_string());
        parsed.persons.push("Bob".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::Ignore, PersonCombine::AllOf,
        );
        assert_eq!(resolved.person_ids, vec![ids[1].clone()]);
    }

    #[cfg(feature = "ai")]
    #[test]
    fn person_comma_within_entry_is_or_even_under_all_of() {
        let (catalog, ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.persons.push("Alice,Bob".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::Ignore, PersonCombine::AllOf,
        );
        assert_eq!(sorted(resolved.person_ids.clone()), sorted(ids.clone()));
    }

    #[cfg(feature = "ai")]
    #[test]
    fn person_exclude_resolves_and_installs() {
        let (catalog, ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.persons_exclude.push("Alice".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::MatchNothing, PersonCombine::AnyOf,
        );
        let mut opts = SearchOptions::default();
        resolved.apply(&mut opts);
        assert_eq!(
            sorted(opts.person_exclude_ids.unwrap().to_vec()),
            sorted(vec![ids[0].clone(), ids[1].clone()])
        );
        assert!(opts.person_asset_ids.is_none());
    }

    #[test]
    fn granular_apply_installs_only_requested_filters() {
        // Export-zip semantics: only collection-include + person-include.
        let (catalog, ids) = setup_catalog();
        let mut parsed = ParsedSearch::default();
        parsed.collections.push("Favs".to_string());
        parsed.collections_exclude.push("Picks".to_string());
        parsed.persons_exclude.push("Alice".to_string());
        let resolved = ResolvedFilterIds::resolve(
            &catalog, &parsed, EmptyFilterPolicy::MatchNothing, PersonCombine::AllOf,
        );
        let mut opts = SearchOptions::default();
        resolved.apply_collections(&mut opts);
        resolved.apply_persons(&mut opts);
        assert_eq!(opts.collection_asset_ids.unwrap(), &[ids[0].clone()]);
        assert!(opts.collection_exclude_ids.is_none());
        assert!(opts.person_exclude_ids.is_none());
    }

    #[test]
    fn union_and_intersect_group_combinators() {
        let lookup = |name: &str| match name {
            "a" => vec!["1".to_string(), "2".to_string()],
            "b" => vec!["2".to_string(), "3".to_string()],
            _ => Vec::new(),
        };
        let entries = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            sorted(union_name_groups(&entries, lookup)),
            vec!["1".to_string(), "2".to_string(), "3".to_string()]
        );
        assert_eq!(intersect_name_groups(&entries, lookup), vec!["2".to_string()]);
        // Comma within one entry is OR in both modes.
        let one_entry = vec!["a, b".to_string()];
        assert_eq!(
            sorted(intersect_name_groups(&one_entry, lookup)),
            vec!["1".to_string(), "2".to_string(), "3".to_string()]
        );
        // Empty / whitespace names are dropped.
        assert_eq!(union_name_groups(&[" , ".to_string()], lookup), Vec::<String>::new());
    }
}
