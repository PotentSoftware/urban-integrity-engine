//! Officer ingestion: give Phoenix its middle hop, without re-fusing distinct humans.
//!
//! Phoenix's motif is `seed Entity → Officer → a dissolved entity`, so an officer node is the
//! only thing connecting a live company to a failed one. Coverage is the binding constraint:
//! `scripts/phoenix_funnel.py` measured 3,234 of 4,371 Contracts Finder awardees carrying no
//! `Directs` edge at all.
//!
//! ## Why the key derivation is the whole problem
//!
//! `scripts/ch_backfill.py` keys officers `director:{applicant_slug(name)}` — on the name alone.
//! That fused 94 groups of provably different people, which had to be split by hand on
//! 2026-08-11, and it is why planning open item 22 marks that script unsafe to re-run. Two
//! officers named "David Jones" are one node under name-only keying.
//!
//! Companies House publishes two identity signals that `graph_events` does not carry, and either
//! one separates two humans who share a name: a per-person officer id embedded in
//! `links.officer.appointments`, and `date_of_birth` as `{month, year}`. Measured over the live
//! fetch cache (2026-08-18): 4,050 appointments, **100% carry an officer id** and 85% a date of
//! birth.
//!
//! This module keys on that evidence. Where a date of birth exists the key takes the established
//! `-b{YYYYMM}` suffix minted by the 2026-08-11 split — 207 such nodes are already live, so this
//! is not a new key format.
//!
//! **Two officer ids on one key is not by itself two people.** Companies House fragments a single
//! human across ids whenever their filings differ, and 99 of those 531 fusions were exactly that.
//! The date of birth settles it: agreeing dates corroborate one node, and where CH publishes none
//! — every secretary — each id keys apart on `-o{id}`, the other suffix the 2026-08-11 split
//! minted. Only an appointment CH gives no identifier at all for is **refused**, because nothing
//! can attribute it. Refusing every collision, the rule until 2026-08-20, dropped both
//! appointments and left the officer missing from the graph, which reads as "holds no
//! directorships" — a wrong answer that logs nothing, this project's characteristic defect.

use std::collections::{HashMap, HashSet};

/// The two Companies House identity signals, neither of which `graph_events` stores.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct OfficerIdentity {
    /// Per-person id from `links.officer.appointments` (`/officers/{id}/appointments`).
    pub officer_id: Option<String>,
    /// `date_of_birth` as `(year, month)`. CH publishes month and year only.
    pub dob: Option<(u16, u8)>,
}

impl OfficerIdentity {
    /// Whether CH gave us anything at all with which to tell this person from a namesake.
    pub fn is_evidenced(&self) -> bool {
        self.officer_id.is_some() || self.dob.is_some()
    }

    /// The established disambiguator, `-b{YYYYMM}`, or `None` when CH published no date of birth.
    pub fn dob_suffix(&self) -> Option<String> {
        let (y, m) = self.dob?;
        Some(format!("-b{y:04}{m:02}"))
    }
}

/// The `EntityId` this officer should be written under, before any collision is considered.
pub fn officer_key(name_slug: &str, ident: &OfficerIdentity) -> String {
    match ident.dob_suffix() {
        Some(suffix) => format!("director:{name_slug}{suffix}"),
        None => format!("director:{name_slug}"),
    }
}

/// The officer-id disambiguator, `-o{id}`, used when nothing corroborates a merge.
///
/// Not a new format: the 2026-08-11 split minted it for 70 secretary appointments, exactly because
/// Companies House publishes no date of birth for a secretary. Those nodes are live, so the
/// charset must match what minted them — `scripts/split_false_merged_officers.py:69` keeps
/// alphanumerics only and lowercases, turning `9-AB1CdeFGhi2JKLmnoPQRstu` into
/// `9ab1cdefghi2jklmnopqrstu`. Emitting the raw id instead would be a third keyspace, which is
/// Open Item 17's failure rebuilt.
fn officer_id_key(base_key: &str, officer_id: &str) -> String {
    let sanitised: String = officer_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();
    format!("{base_key}-o{sanitised}")
}

/// What a batch of appointments would do to the keyspace, decided before anything is written.
#[derive(Debug, Default, PartialEq)]
pub struct KeyPlan {
    /// Keys safe to write: each maps to exactly one identity, or to identities CH cannot
    /// distinguish but which carry no contradicting evidence.
    pub writable: Vec<(String, OfficerIdentity)>,
    /// Keys refused because two or more provably distinct people would land on them.
    pub refused: Vec<RefusedKey>,
}

impl KeyPlan {
    /// An index for resolving one appointment to the key it was planned under.
    ///
    /// The keying rule lives here and in [`plan_keys`] alone. A caller that re-derives it from
    /// `officer_key` would write the collision cases under the bare name again — a second
    /// keyspace, which is Open Item 17 rebuilt.
    pub fn resolver(&self) -> KeyResolver {
        KeyResolver {
            writable: self.writable.iter().map(|(k, _)| k.clone()).collect(),
        }
    }
}

/// Resolves appointments to planned keys. Built by [`KeyPlan::resolver`].
pub struct KeyResolver {
    writable: HashSet<String>,
}

impl KeyResolver {
    /// The key this appointment must be written under, or `None` if its key was refused.
    ///
    /// `None` means skip. It must never fall back to the bare name — that fallback is what fused
    /// 94 nodes.
    pub fn key_for(&self, name_slug: &str, ident: &OfficerIdentity) -> Option<String> {
        let base = officer_key(name_slug, ident);
        if let Some(oid) = ident.officer_id.as_deref() {
            let split = officer_id_key(&base, oid);
            if self.writable.contains(&split) {
                return Some(split);
            }
        }
        self.writable.contains(&base).then_some(base)
    }
}

/// A key that would fuse people Companies House says are different.
#[derive(Debug, PartialEq)]
pub struct RefusedKey {
    pub key: String,
    /// The distinct officer ids that collided, for the operator to adjudicate.
    pub officer_ids: Vec<String>,
}

/// Plan the keyspace for a batch of `(name_slug, identity)` appointments.
///
/// Two Companies House `officer_id`s landing on one key is **not** by itself proof of two people:
/// CH fragments a single human across ids whenever their filings differ, and 99 of the 531 fusions
/// adjudicated on 2026-08-11 were exactly that. What settles it is the date of birth:
///
/// - **Distinct dates of birth** — different humans. They never share a key in the first place,
///   because the key already carries `-b{YYYYMM}`.
/// - **A shared date of birth** — one human, one node. CH's own fragmentation, corroborated.
/// - **No date of birth** (CH publishes none for secretaries) — nothing corroborates a merge, so
///   under D-18 each officer id keys apart on `-o{id}` rather than fusing.
/// - **No identifier at all** — the appointment cannot be attributed to either candidate, so the
///   key is refused rather than guessed at.
///
/// Refusing was the previous rule for every collision, and it cost more than it saved: both
/// appointments were dropped, leaving the officer absent from the graph, which is indistinguishable
/// from "this person holds no directorships".
pub fn plan_keys(appointments: &[(String, OfficerIdentity)]) -> KeyPlan {
    let mut by_key: HashMap<String, Vec<&OfficerIdentity>> = HashMap::new();
    for (slug, ident) in appointments {
        by_key
            .entry(officer_key(slug, ident))
            .or_default()
            .push(ident);
    }

    let mut plan = KeyPlan::default();
    let mut keys: Vec<_> = by_key.into_iter().collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0)); // deterministic output; this drives a cursor
    for (key, idents) in keys {
        let ids: HashSet<&str> = idents
            .iter()
            .filter_map(|i| i.officer_id.as_deref())
            .collect();
        // An appointment CH named no officer id for could be either candidate, so it counts as a
        // candidate of its own rather than being absorbed into the one id that is present.
        let unidentified = idents.iter().any(|i| i.officer_id.is_none());
        let candidates = ids.len() + usize::from(unidentified);
        // One identity, or a shared date of birth vouching for the merge: one node.
        if candidates <= 1 || idents.iter().any(|i| i.dob.is_some()) {
            plan.writable.push((key, idents[0].clone()));
            continue;
        }
        // Uncorroborated. Every appointment must name an officer id to key on, or none can.
        if unidentified {
            let mut officer_ids: Vec<String> = ids.into_iter().map(str::to_owned).collect();
            officer_ids.sort();
            plan.refused.push(RefusedKey { key, officer_ids });
            continue;
        }
        let mut split: Vec<&&OfficerIdentity> = idents.iter().collect();
        split.sort_by(|a, b| a.officer_id.cmp(&b.officer_id));
        split.dedup_by(|a, b| a.officer_id == b.officer_id);
        for ident in split {
            let oid = ident.officer_id.as_deref().expect("checked above");
            plan.writable
                .push((officer_id_key(&key, oid), (*ident).clone()));
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(oid: Option<&str>, dob: Option<(u16, u8)>) -> OfficerIdentity {
        OfficerIdentity {
            officer_id: oid.map(str::to_owned),
            dob,
        }
    }

    /// The suffix format is not new — it is what the 2026-08-11 split minted, and 207 nodes are
    /// already live under it (e.g. `director:example-pat-b197303`). Inventing a second format
    /// would produce exactly the mixed keyspace Open Item 17 was.
    #[test]
    fn a_date_of_birth_produces_the_established_suffix() {
        let k = officer_key("example-pat", &ident(Some("abc"), Some((1973, 3))));
        assert_eq!(k, "director:example-pat-b197303");
    }

    #[test]
    fn no_date_of_birth_leaves_the_key_bare() {
        assert_eq!(
            officer_key("smith-john", &ident(Some("abc"), None)),
            "director:smith-john"
        );
    }

    /// **The 94-merge failure, pinned.** Two candidate humans, same name, no date of birth to
    /// corroborate a merge. Name-only keying fused these; the officer id holds them apart — and
    /// because it does, both appointments still reach the graph instead of being dropped.
    #[test]
    fn two_distinct_officer_ids_on_one_key_are_split_not_fused() {
        let batch = vec![
            ("jones-david".to_string(), ident(Some("officer-a"), None)),
            ("jones-david".to_string(), ident(Some("officer-b"), None)),
        ];
        let plan = plan_keys(&batch);
        assert!(plan.refused.is_empty(), "both are identifiable: {plan:?}");
        let keys: Vec<&str> = plan.writable.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "director:jones-david-oofficera",
                "director:jones-david-oofficerb"
            ],
            "one node per Companies House officer id, never one shared node"
        );
    }

    /// **The 2026-08-18 audit's over-claim, pinned.** Companies House issues more than one
    /// `officer_id` for the same human — 99 of the 531 fusions adjudicated on 2026-08-11 were
    /// exactly that, and were ruled defensible. When the colliding ids agree on a date of birth,
    /// that is corroboration, not conflict: one person, one node, no refusal. Splitting them would
    /// shatter the multi-company directors Phoenix exists to traverse.
    #[test]
    fn officer_ids_that_agree_on_a_date_of_birth_are_one_person() {
        let batch = vec![
            (
                "sample-alex-jordan".to_string(),
                ident(Some("officer-x"), Some((1962, 3))),
            ),
            (
                "sample-alex-jordan".to_string(),
                ident(Some("officer-y"), Some((1962, 3))),
            ),
        ];
        let plan = plan_keys(&batch);
        assert!(
            plan.refused.is_empty(),
            "a shared DOB corroborates the merge: {plan:?}"
        );
        let keys: Vec<&str> = plan.writable.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["director:sample-alex-jordan-b196203"]);
    }

    /// An appointment Companies House publishes no identifier for cannot be attributed to either
    /// colliding officer. Guessing is the failure this module exists to prevent, so the key is
    /// refused — the one case that still has no safe key.
    #[test]
    fn a_key_carrying_an_unidentifiable_appointment_is_refused() {
        let batch = vec![
            ("jones-david".to_string(), ident(Some("officer-a"), None)),
            ("jones-david".to_string(), ident(None, None)),
        ];
        let plan = plan_keys(&batch);
        assert!(plan.writable.is_empty(), "nothing here is safely keyable");
        assert_eq!(plan.refused.len(), 1);
        assert_eq!(plan.refused[0].key, "director:jones-david");
    }

    /// The resolver is the single place the keying rule lives, so a caller cannot re-derive it
    /// differently — the mixed-keyspace failure of Open Item 17.
    #[test]
    fn the_resolver_returns_the_key_each_appointment_was_planned_under() {
        let a = ident(Some("officer-a"), None);
        let b = ident(Some("officer-b"), None);
        let solo = ident(Some("officer-c"), Some((1970, 6)));
        let batch = vec![
            ("jones-david".to_string(), a.clone()),
            ("jones-david".to_string(), b.clone()),
            ("smith-jane".to_string(), solo.clone()),
        ];
        let resolver = plan_keys(&batch).resolver();
        assert_eq!(
            resolver.key_for("jones-david", &a).as_deref(),
            Some("director:jones-david-oofficera")
        );
        assert_eq!(
            resolver.key_for("jones-david", &b).as_deref(),
            Some("director:jones-david-oofficerb")
        );
        assert_eq!(
            resolver.key_for("smith-jane", &solo).as_deref(),
            Some("director:smith-jane-b197006")
        );
    }

    /// A refused key resolves to nothing at all, so the write path skips it rather than falling
    /// back to the bare name — the fallback that produced the fusions in the first place.
    #[test]
    fn the_resolver_refuses_rather_than_falling_back_to_the_bare_name() {
        let unidentifiable = ident(None, None);
        let batch = vec![
            ("jones-david".to_string(), ident(Some("officer-a"), None)),
            ("jones-david".to_string(), unidentifiable.clone()),
        ];
        let resolver = plan_keys(&batch).resolver();
        assert_eq!(resolver.key_for("jones-david", &unidentifiable), None);
    }

    /// The same two humans, once CH supplies dates of birth, separate cleanly and both write.
    /// This is the point of keying on evidence rather than on the name.
    #[test]
    fn dates_of_birth_separate_two_namesakes() {
        let batch = vec![
            (
                "jones-david".to_string(),
                ident(Some("officer-a"), Some((1960, 4))),
            ),
            (
                "jones-david".to_string(),
                ident(Some("officer-b"), Some((1985, 11))),
            ),
        ];
        let plan = plan_keys(&batch);
        assert!(plan.refused.is_empty(), "distinct DOBs must not collide");
        let keys: Vec<&str> = plan.writable.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "director:jones-david-b196004",
                "director:jones-david-b198511"
            ]
        );
    }

    /// One person appearing on several boards is the normal case and must not be mistaken for a
    /// collision — otherwise every multi-directorship, which is precisely what Phoenix traverses,
    /// would be refused.
    #[test]
    fn one_person_on_many_boards_is_not_a_collision() {
        let batch = vec![
            (
                "furniss-jonathan-paul".to_string(),
                ident(Some("x1"), Some((1970, 6))),
            ),
            (
                "furniss-jonathan-paul".to_string(),
                ident(Some("x1"), Some((1970, 6))),
            ),
            (
                "furniss-jonathan-paul".to_string(),
                ident(Some("x1"), Some((1970, 6))),
            ),
        ];
        let plan = plan_keys(&batch);
        assert!(plan.refused.is_empty());
        assert_eq!(plan.writable.len(), 1, "one human, one node");
    }

    /// CH supplied nothing to key on. One such appointment is writable — nothing contradicts it —
    /// but it is recorded as unevidenced so a caller can choose to hold it back.
    #[test]
    fn an_unevidenced_appointment_is_visible_as_such() {
        let bare = ident(None, None);
        assert!(!bare.is_evidenced());
        assert!(ident(Some("x"), None).is_evidenced());
        assert!(ident(None, Some((1970, 1))).is_evidenced());
        let plan = plan_keys(&[("smith-john".to_string(), bare)]);
        assert_eq!(plan.writable.len(), 1);
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Fetch cache
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// One company's officer list as cached by the fetch phase.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CachedOfficers {
    #[serde(rename = "cn")]
    pub company_number: String,
    #[serde(default)]
    pub items: Vec<CachedAppointment>,
}

/// One appointment, reduced to what keying and edge-writing need.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CachedAppointment {
    #[serde(default)]
    pub name: Option<String>,
    /// CH `officer_role`. **Load-bearing, not decoration.** Phoenix drops any officer whose role
    /// starts with `corporate-` (`detector/src/motif/phoenix.rs`), because a
    /// company-secretarial firm is the named officer of many unrelated companies and a motif
    /// through one joins companies sharing nothing but a supplier. Writing officers without this
    /// field silently disables that guard: 130 of the 4,050 cached appointments are
    /// `corporate-nominee-secretary`, `corporate-director` and the like.
    #[serde(default)]
    pub officer_role: Option<String>,
    #[serde(default)]
    pub appointed_on: Option<String>,
    #[serde(default)]
    pub resigned_on: Option<String>,
    #[serde(default)]
    pub date_of_birth: Option<CachedDob>,
    #[serde(default)]
    pub links: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CachedDob {
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub month: Option<u8>,
}

impl CachedAppointment {
    /// `true` while the officer is still on the board. A resigned officer is not a live bridge
    /// between two companies, so Phoenix must not traverse one.
    pub fn is_active(&self) -> bool {
        self.resigned_on.is_none()
    }

    /// Pull CH's two identity signals out of the raw reply.
    pub fn identity(&self) -> OfficerIdentity {
        let officer_id = self
            .links
            .as_ref()
            .and_then(|l| l.get("officer"))
            .and_then(|o| o.get("appointments"))
            .and_then(|a| a.as_str())
            .and_then(|link| {
                // `/officers/{id}/appointments`
                let rest = link.split("/officers/").nth(1)?;
                rest.split('/').next().filter(|s| !s.is_empty())
            })
            .map(str::to_owned);
        let dob = self
            .date_of_birth
            .as_ref()
            .and_then(|d| Some((d.year?, d.month?)));
        OfficerIdentity { officer_id, dob }
    }
}

/// Read the JSONL officer cache written by `scripts/ch_officer_coverage.py --fetch`.
/// A malformed line is skipped, for the same reason the profile cache tolerates one.
pub fn parse_officer_cache(contents: &str) -> Vec<CachedOfficers> {
    contents
        .lines()
        .filter_map(|l| serde_json::from_str::<CachedOfficers>(l.trim()).ok())
        .collect()
}

#[cfg(test)]
mod role_tests {
    use super::*;

    /// **Phoenix's corporate-officer guard depends on this field surviving ingestion.**
    ///
    /// `detector/src/motif/phoenix.rs` drops any officer whose role starts with `corporate-`,
    /// because a company-secretarial firm is the named officer of many unrelated companies and a
    /// motif through one joins companies that share nothing but a supplier — Open Item 28. An
    /// earlier draft of this backfill wrote `officer_role: None` for every appointment, which
    /// would have disabled the guard for everything it ingested. 130 of the 4,050 cached
    /// appointments carry a `corporate-` role.
    #[test]
    fn a_corporate_role_is_carried_through_from_the_cache() {
        // JSONL: one company per line. A pretty-printed fixture would parse to nothing.
        let raw = r#"{"cn":"12345678","items":[{"name":"EXAMPLE CORPORATE SECRETARY LIMITED","officer_role":"corporate-secretary"},{"name":"SMITH, Jane","officer_role":"director","appointed_on":"2019-04-01"}]}"#;
        let parsed = parse_officer_cache(raw);
        let items = &parsed[0].items;
        assert_eq!(
            items[0].officer_role.as_deref(),
            Some("corporate-secretary")
        );
        assert_eq!(items[1].officer_role.as_deref(), Some("director"));
        assert_eq!(items[1].appointed_on.as_deref(), Some("2019-04-01"));
    }

    /// Every `corporate-` variant Companies House actually publishes in this corpus must be
    /// recognisable by the prefix test Phoenix applies, so none slips past as a human.
    #[test]
    fn every_live_corporate_role_matches_phoenixs_prefix_test() {
        for role in [
            "corporate-nominee-secretary",
            "corporate-nominee-director",
            "corporate-secretary",
            "corporate-director",
        ] {
            assert!(
                role.starts_with("corporate-"),
                "{role} must be caught by the detector's guard"
            );
        }
    }
}

#[cfg(test)]
mod live_officer_cache_tests {
    use super::*;
    use crate::planning::mapper::applicant_slug;

    /// **The keying rule, proven against real Companies House data rather than a fixture.**
    ///
    /// Over the 393 companies already fetched into `data/ch-officers-cf-awardees.jsonl`, exactly
    /// 12 keys carry two Companies House officer ids. Measured 2026-08-20: **not one of them has
    /// two distinct dates of birth.** Five agree on a DOB — which is CH fragmenting one human
    /// across two ids, the class ruled defensible
    /// in 99 of the 531 fusions adjudicated on 2026-08-11. The other seven are secretaries, for
    /// whom CH publishes no DOB at all, so nothing corroborates a merge and each keys on its own
    /// officer id under D-18.
    ///
    /// Nothing here is refused, and that is the point: refusing dropped both appointments and left
    /// the officer absent from the graph, which reads exactly like "this person has no
    /// directorships".
    #[test]
    // Reads the operator's local cache, which `.gitignore` excludes — so this cannot run
    // on a fresh clone or in CI. It is `#[ignore]`d rather than skipped at runtime so the
    // default `cargo test` summary reports it as *ignored* instead of *passed*: until
    // 2026-08-20 an absent file returned early and the test counted as green, which is the
    // silent-pass failure mode this suite exists to catch. Run it with:
    //     cargo test -p urban-integrity-ingestor -- --ignored the_real_officer_cache_keys_every_collision_on_evidence
    #[ignore = "needs the operator's local data/ch-officers-cf-awardees.jsonl; run with --ignored"]
    fn the_real_officer_cache_keys_every_collision_on_evidence() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/ch-officers-cf-awardees.jsonl"
        );
        // Hard failure, not a skip: this test is `#[ignore]`d by default, so reaching this
        // line means someone asked for it explicitly and is owed a real answer.
        let contents = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "{path} is missing ({e}). This test asserts invariants against the real \
                 Companies House corpus, not a fixture — populate it with `backfill officers` \
                 and re-run. Do not commit the file: it carries real names and dates of birth."
            )
        });
        let lines = contents.lines().filter(|l| !l.trim().is_empty()).count();
        let companies = parse_officer_cache(&contents);
        assert_eq!(
            companies.len(),
            lines,
            "every non-empty line must parse — a dropped row is silent data loss"
        );
        // `backfill officers` appends to this file, so these are floors, not equalities.
        assert!(companies.len() >= 393, "cache shrank: {}", companies.len());

        let appointments: Vec<(String, OfficerIdentity)> = companies
            .iter()
            .flat_map(|c| c.items.iter())
            .filter(|a| a.is_active())
            .filter_map(|a| Some((applicant_slug(a.name.as_deref()?), a.identity())))
            .collect();
        assert!(
            appointments.len() >= 1_391,
            "active appointments: {}",
            appointments.len()
        );

        let plan = plan_keys(&appointments);
        assert!(
            plan.refused.is_empty(),
            "every colliding key in this cache is keyable on CH evidence: {:?}",
            plan.refused
        );

        let keys: HashSet<&str> = plan.writable.iter().map(|(k, _)| k.as_str()).collect();

        // Group the colliding slugs (two or more CH officer ids) by what evidence they carry.
        let mut by_slug: HashMap<&str, Vec<&OfficerIdentity>> = HashMap::new();
        for (slug, ident) in &appointments {
            by_slug.entry(slug.as_str()).or_default().push(ident);
        }
        let mut dob_agreeing = 0usize;
        let mut dobless = 0usize;
        for (slug, idents) in &by_slug {
            let oids: HashSet<&str> = idents
                .iter()
                .filter_map(|i| i.officer_id.as_deref())
                .collect();
            if oids.len() < 2 {
                continue;
            }
            let dobs: HashSet<(u16, u8)> = idents.iter().filter_map(|i| i.dob).collect();
            if dobs.len() == 1 {
                // Two officer ids agreeing on a DOB stay one node.
                dob_agreeing += 1;
                let dob_key = officer_key(slug, idents.iter().find(|i| i.dob.is_some()).unwrap());
                assert!(
                    keys.contains(dob_key.as_str()),
                    "two officer ids agreeing on a DOB stay one node: {dob_key}"
                );
            } else if dobs.is_empty() {
                // The secretaries CH publishes no DOB for: one node per officer id, no shared node.
                dobless += 1;
                let bare = format!("director:{slug}");
                assert!(
                    !keys.contains(bare.as_str()),
                    "an uncorroborated name-only key must not be written: {bare}"
                );
                let per_id: HashSet<String> =
                    oids.iter().map(|oid| officer_id_key(&bare, oid)).collect();
                assert_eq!(per_id.len(), oids.len(), "one key per officer id: {slug}");
                assert!(
                    per_id.iter().all(|k| keys.contains(k.as_str())),
                    "each unadjudicable secretary keys on their own officer id: {slug}"
                );
            }
        }
        assert!(
            dob_agreeing >= 1,
            "the cache must still exercise the DOB-corroborated merge"
        );
        assert!(
            dobless >= 1,
            "the cache must still exercise the DOB-less secretary split"
        );
    }
}

/// One officer key that Companies House evidence says conflates more than one person.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyAudit {
    pub key: String,
    /// The distinct CH officer ids that share this key.
    pub officer_ids: Vec<String>,
    /// Company numbers those officers are appointed at.
    pub company_numbers: Vec<String>,
    /// The key already exists in the graph AND already links companies belonging to more than one
    /// of those officers. The node is a fusion of distinct humans **right now**.
    pub already_fused: bool,
}

/// Audit the officer keys that ALREADY exist in the graph against Companies House evidence.
///
/// [`plan_keys`] guards writes; it cannot see what a previous tool wrote. `scripts/ch_backfill.py`
/// keyed on the name alone and its output is still in the graph, so the collisions that matter most
/// are the ones already there.
///
/// A key is reported only when **nothing corroborates the merge** — no date of birth on any of the
/// colliding appointments. Two officer ids that agree on a date of birth are one human whom
/// Companies House has fragmented, the class ruled defensible in 99 of the 531 fusions adjudicated
/// on 2026-08-11, and reporting them sends an analyst to unpick a correct node. The first version
/// of this audit flagged on officer-id multiplicity alone and so reported all 12 live collisions,
/// five of them wrongly — including one dated-suffix key filed High as a node that "fuses 2
/// people Companies House says are different" when CH publishes the same DOB for both.
///
/// `already_fused` is the distinction that drives severity: an uncorroborated key merely *present*
/// is at risk, whereas a fused one asserts one identity today and every motif traversing it
/// inherits that.
pub fn audit_existing_keys(
    appointments: &[(String, OfficerIdentity, String)],
    graph_edges: &[(String, String)],
) -> Vec<KeyAudit> {
    // key -> officer_id -> companies CH attributes to that officer
    let mut by_key: HashMap<String, HashMap<String, HashSet<String>>> = HashMap::new();
    // Keys where CH published a date of birth, which vouches for every id landing on them.
    let mut corroborated: HashSet<String> = HashSet::new();
    for (slug, ident, cn) in appointments {
        let key = officer_key(slug, ident);
        if ident.dob.is_some() {
            corroborated.insert(key.clone());
        }
        let Some(oid) = ident.officer_id.as_deref() else {
            continue;
        };
        by_key
            .entry(key)
            .or_default()
            .entry(oid.to_string())
            .or_default()
            .insert(cn.clone());
    }
    by_key.retain(|key, _| !corroborated.contains(key));

    // key -> companies the graph currently attaches to it, either edge direction
    let mut graph_companies: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (a, b) in graph_edges {
        for (key, other) in [(a.as_str(), b.as_str()), (b.as_str(), a.as_str())] {
            if key.starts_with("director:") {
                if let Some(cn) = other.strip_prefix(crate::backfill::cessation::CH_PREFIX) {
                    graph_companies.entry(key).or_default().insert(cn);
                }
            }
        }
    }

    let mut out: Vec<KeyAudit> = by_key
        .into_iter()
        .filter(|(_, per_officer)| per_officer.len() > 1)
        .map(|(key, per_officer)| {
            let mut officer_ids: Vec<String> = per_officer.keys().cloned().collect();
            officer_ids.sort();
            let mut company_numbers: Vec<String> = per_officer
                .values()
                .flat_map(|c| c.iter().cloned())
                .collect();
            company_numbers.sort();
            company_numbers.dedup();

            // Fused when the graph node already spans companies from two different officers.
            let live = graph_companies.get(key.as_str());
            let already_fused = live.is_some_and(|live| {
                per_officer
                    .values()
                    .filter(|cs| cs.iter().any(|c| live.contains(c.as_str())))
                    .count()
                    > 1
            });
            KeyAudit {
                key,
                officer_ids,
                company_numbers,
                already_fused,
            }
        })
        .collect();
    // Fused first — those are asserting a false identity today.
    out.sort_by(|a, b| {
        b.already_fused
            .cmp(&a.already_fused)
            .then(a.key.cmp(&b.key))
    });
    out
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Splitting what a previous tool already wrote
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// One Companies House appointment, as the split planner needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct AppointmentRef {
    /// The key this appointment currently sits under in the graph.
    pub key: String,
    pub officer_id: String,
    pub company_number: String,
    pub appointed_on: Option<String>,
    pub officer_role: Option<String>,
}

/// One `graph_events` row sitting on a key that has to be split.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRef {
    /// Record id within `graph_events`, without the table prefix.
    pub record_id: String,
    pub key: String,
    /// From `GB-COH:{cn}` on an edge, or the officer weight's `company_number` on a node. Officer
    /// nodes written before PR #38 carry no company at all, hence the `Option`.
    pub company_number: Option<String>,
    pub appointed_on: Option<String>,
    pub officer_role: Option<String>,
}

/// One planned re-key of a single event.
#[derive(Debug, Clone, PartialEq)]
pub struct ReKey {
    pub record_id: String,
    /// The `payload` field holding the key: `entity_id` on a node, `dst_id` on a `Directs` edge.
    pub field: &'static str,
    pub from: String,
    pub to: String,
}

/// What a split would do, decided before anything is written.
#[derive(Debug, Default, PartialEq)]
pub struct SplitPlan {
    pub rekeys: Vec<ReKey>,
    /// Events that could not be attributed to one officer. **Left exactly where they are.**
    /// Moving them on a guess is the failure this whole module exists to prevent, and leaving them
    /// is safe in one direction: a stale key can only remove motifs, never manufacture one.
    pub unattributable: Vec<String>,
}

/// Plan the re-keying of events already written under uncorroborated colliding keys.
///
/// Attribution is by evidence, in order: the company number if the event carries one, otherwise a
/// unique match on `(appointed_on, officer_role)` against that key's Companies House appointments.
/// Anything that stays ambiguous is reported, never guessed at.
pub fn plan_collision_split(
    appointments: &[AppointmentRef],
    node_events: &[EventRef],
    edge_events: &[EventRef],
) -> SplitPlan {
    let mut by_key: HashMap<&str, Vec<&AppointmentRef>> = HashMap::new();
    for a in appointments {
        by_key.entry(a.key.as_str()).or_default().push(a);
    }
    for v in by_key.values_mut() {
        v.sort_by(|a, b| a.officer_id.cmp(&b.officer_id)); // deterministic hand-out
    }

    let mut plan = SplitPlan::default();
    // A company holding two same-named appointments gets one event handed to each, not both to
    // the first — one live company really did hold two identically named directors.
    let mut claimed: HashMap<(&str, &str), usize> = HashMap::new();

    for (events, field) in [(node_events, "entity_id"), (edge_events, "dst_id")] {
        for ev in events {
            let Some(pool) = by_key.get(ev.key.as_str()) else {
                continue;
            };
            let matches: Vec<&&AppointmentRef> = match ev.company_number.as_deref() {
                Some(cn) => pool.iter().filter(|a| a.company_number == cn).collect(),
                None => pool
                    .iter()
                    .filter(|a| {
                        a.appointed_on == ev.appointed_on && a.officer_role == ev.officer_role
                    })
                    .collect(),
            };
            let picked = match matches.len() {
                0 => None,
                1 => Some(matches[0]),
                _ => {
                    let cn = ev.company_number.as_deref().unwrap_or("");
                    let n = claimed.entry((ev.key.as_str(), cn)).or_default();
                    let picked = matches.get(*n).copied();
                    *n += 1;
                    picked
                }
            };
            match picked {
                Some(appointment) => plan.rekeys.push(ReKey {
                    record_id: ev.record_id.clone(),
                    field,
                    from: ev.key.clone(),
                    to: officer_id_key(&ev.key, &appointment.officer_id),
                }),
                None => plan.unattributable.push(ev.record_id.clone()),
            }
        }
    }
    plan.rekeys.sort_by(|a, b| a.record_id.cmp(&b.record_id));
    plan.unattributable.sort();
    plan
}

#[cfg(test)]
mod split_tests {
    use super::*;

    fn appointment(key: &str, oid: &str, cn: &str, on: &str, role: &str) -> AppointmentRef {
        AppointmentRef {
            key: key.into(),
            officer_id: oid.into(),
            company_number: cn.into(),
            appointed_on: Some(on.into()),
            officer_role: Some(role.into()),
        }
    }

    fn event(record: &str, key: &str, cn: Option<&str>, on: &str, role: &str) -> EventRef {
        EventRef {
            record_id: record.into(),
            key: key.into(),
            company_number: cn.map(str::to_owned),
            appointed_on: Some(on.into()),
            officer_role: Some(role.into()),
        }
    }

    /// An edge names its company in `src_id`, which settles which officer it belongs to.
    #[test]
    fn an_edge_is_attributed_by_its_company() {
        let appts = vec![
            appointment(
                "director:jones-david",
                "oid-a",
                "111",
                "2020-01-01",
                "secretary",
            ),
            appointment(
                "director:jones-david",
                "oid-b",
                "222",
                "2021-01-01",
                "secretary",
            ),
        ];
        let edges = vec![event("e1", "director:jones-david", Some("222"), "", "")];
        let plan = plan_collision_split(&appts, &[], &edges);
        assert_eq!(plan.unattributable.len(), 0);
        assert_eq!(plan.rekeys.len(), 1);
        assert_eq!(plan.rekeys[0].field, "dst_id");
        assert_eq!(plan.rekeys[0].to, "director:jones-david-ooidb");
    }

    /// Officer nodes written before PR #38 carry no company, so the appointment date and role are
    /// the only evidence left. A unique match is enough.
    #[test]
    fn a_node_without_a_company_is_attributed_by_date_and_role() {
        let appts = vec![
            appointment(
                "director:jones-david",
                "oid-a",
                "111",
                "2020-01-01",
                "secretary",
            ),
            appointment(
                "director:jones-david",
                "oid-b",
                "222",
                "2021-01-01",
                "director",
            ),
        ];
        let nodes = vec![event(
            "n1",
            "director:jones-david",
            None,
            "2021-01-01",
            "director",
        )];
        let plan = plan_collision_split(&appts, &nodes, &[]);
        assert_eq!(plan.rekeys.len(), 1);
        assert_eq!(plan.rekeys[0].field, "entity_id");
        assert_eq!(plan.rekeys[0].to, "director:jones-david-ooidb");
    }

    /// **The guess that must not happen.** Two appointments the event matches equally well, and no
    /// company to separate them: the event stays where it is and is reported.
    #[test]
    fn an_unattributable_node_is_reported_not_guessed() {
        let appts = vec![
            appointment(
                "director:jones-david",
                "oid-a",
                "111",
                "2020-01-01",
                "secretary",
            ),
            appointment(
                "director:jones-david",
                "oid-b",
                "222",
                "2020-01-01",
                "secretary",
            ),
        ];
        let nodes = vec![event(
            "n1",
            "director:jones-david",
            None,
            "1999-12-31",
            "director",
        )];
        let plan = plan_collision_split(&appts, &nodes, &[]);
        assert!(plan.rekeys.is_empty());
        assert_eq!(plan.unattributable, vec!["n1"]);
    }

    /// One company really can hold two same-named officers. Its edges are handed out one per
    /// appointment rather than both landing on the first — the same-named case from 2026-08-11.
    #[test]
    fn two_appointments_at_one_company_take_one_event_each() {
        let appts = vec![
            appointment(
                "director:jones-david",
                "oid-a",
                "111",
                "2020-01-01",
                "director",
            ),
            appointment(
                "director:jones-david",
                "oid-b",
                "111",
                "2020-01-01",
                "director",
            ),
        ];
        let edges = vec![
            event("e1", "director:jones-david", Some("111"), "", ""),
            event("e2", "director:jones-david", Some("111"), "", ""),
        ];
        let plan = plan_collision_split(&appts, &[], &edges);
        let targets: Vec<&str> = plan.rekeys.iter().map(|r| r.to.as_str()).collect();
        assert_eq!(
            targets,
            vec!["director:jones-david-ooida", "director:jones-david-ooidb"]
        );
    }

    /// A key nobody planned a split for is left completely alone.
    #[test]
    fn events_on_unaffected_keys_are_untouched() {
        let appts = vec![appointment(
            "director:jones-david",
            "oid-a",
            "111",
            "2020-01-01",
            "director",
        )];
        let nodes = vec![event(
            "n1",
            "director:smith-jane",
            None,
            "2020-01-01",
            "director",
        )];
        let plan = plan_collision_split(&appts, &nodes, &[]);
        assert_eq!(plan, SplitPlan::default());
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    fn ident(oid: &str, dob: Option<(u16, u8)>) -> OfficerIdentity {
        OfficerIdentity {
            officer_id: Some(oid.to_string()),
            dob,
        }
    }

    /// **The audit's own over-claim, pinned.** This is the live shape that was reported High on
    /// 2026-08-18 as a node that "ALREADY fuses 2 people Companies House says are different".
    /// Companies House says the opposite: one date of birth on both appointments. Two
    /// officer ids for one human is CH's own fragmentation, not two humans, so a node linking both
    /// their companies is correct and reporting it sends an analyst to unpick a true positive.
    #[test]
    fn officer_ids_agreeing_on_a_date_of_birth_are_not_reported_even_when_the_node_links_both() {
        let appts = vec![
            (
                "sample-alex-jordan".into(),
                ident("oid-a", Some((1962, 3))),
                "99000031".into(),
            ),
            (
                "sample-alex-jordan".into(),
                ident("oid-b", Some((1962, 3))),
                "99000032".into(),
            ),
        ];
        let edges = vec![
            (
                "GB-COH:99000031".to_string(),
                "director:sample-alex-jordan-b196203".to_string(),
            ),
            (
                "GB-COH:99000032".to_string(),
                "director:sample-alex-jordan-b196203".to_string(),
            ),
        ];
        assert!(
            audit_existing_keys(&appts, &edges).is_empty(),
            "a shared DOB corroborates the merge; it is not a collision"
        );
    }

    /// The genuine fusion the audit exists to catch: no date of birth anywhere, two officer ids,
    /// and the graph node already carrying a company from each. Nothing corroborates the merge and
    /// the node asserts one identity right now, so it must be reported as fused.
    #[test]
    fn an_uncorroborated_key_linking_both_officers_companies_is_fused() {
        let appts = vec![
            ("jones-david".into(), ident("oid-a", None), "111".into()),
            ("jones-david".into(), ident("oid-b", None), "222".into()),
        ];
        let edges = vec![
            ("GB-COH:111".to_string(), "director:jones-david".to_string()),
            ("GB-COH:222".to_string(), "director:jones-david".to_string()),
        ];
        let audit = audit_existing_keys(&appts, &edges);
        assert_eq!(audit.len(), 1);
        assert!(audit[0].already_fused);
        assert_eq!(audit[0].officer_ids, vec!["oid-a", "oid-b"]);
    }

    /// A colliding key whose graph node only carries ONE officer's company is at risk, but is not
    /// currently asserting anything false. The distinction drives severity, so it must hold.
    #[test]
    fn a_colliding_key_touching_one_officer_only_is_not_yet_fused() {
        let appts = vec![
            ("jones-david".into(), ident("oid-a", None), "111".into()),
            ("jones-david".into(), ident("oid-b", None), "222".into()),
        ];
        let edges = vec![("GB-COH:111".to_string(), "director:jones-david".to_string())];
        let audit = audit_existing_keys(&appts, &edges);
        assert_eq!(audit.len(), 1);
        assert!(!audit[0].already_fused);
    }

    /// One person on several boards is not a collision and must never be audited as one.
    #[test]
    fn one_officer_on_two_boards_is_not_reported() {
        let appts = vec![
            ("smith-jane".into(), ident("oid-a", None), "111".into()),
            ("smith-jane".into(), ident("oid-a", None), "222".into()),
        ];
        let edges = vec![
            ("GB-COH:111".to_string(), "director:smith-jane".to_string()),
            ("GB-COH:222".to_string(), "director:smith-jane".to_string()),
        ];
        assert!(audit_existing_keys(&appts, &edges).is_empty());
    }

    /// Fused keys sort first: they are asserting a false identity now, the others only might.
    #[test]
    fn fused_keys_are_reported_before_merely_colliding_ones() {
        let appts = vec![
            ("aaa-first".into(), ident("o1", None), "111".into()),
            ("aaa-first".into(), ident("o2", None), "222".into()),
            ("zzz-last".into(), ident("o3", None), "333".into()),
            ("zzz-last".into(), ident("o4", None), "444".into()),
        ];
        // Only zzz-last is fused, but aaa-first sorts first alphabetically.
        let edges = vec![
            ("GB-COH:333".to_string(), "director:zzz-last".to_string()),
            ("GB-COH:444".to_string(), "director:zzz-last".to_string()),
            ("GB-COH:111".to_string(), "director:aaa-first".to_string()),
        ];
        let audit = audit_existing_keys(&appts, &edges);
        assert_eq!(audit[0].key, "director:zzz-last");
        assert!(audit[0].already_fused);
        assert!(!audit[1].already_fused);
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Runner
// ─────────────────────────────────────────────────────────────────────────────────────────────

use crate::backfill::cursor::{resume_index, CursorState, ResumeCursor};
use crate::backfill::session::DbSession;
use crate::backfill::{extract_id_str, BackfillStats};
use anyhow::Result;
use urban_integrity_schema::anomaly::{
    AnomalySeverity, AnomalyStatus, DataAnomaly, KIND_OFFICER_KEY_COLLISION,
};

/// Cursor job name — `backfill_cursors:officers`.
pub const OFFICERS_JOB: &str = "officers";

/// The record key of a `graph_events` row, without its table prefix.
///
/// `UPDATE type::record('graph_events', $key)` wants the key alone. The backticks SurrealDB wraps
/// around ids containing a colon are stripped for the same reason `anomalies show` had to: they
/// are display syntax, and passing them through addresses a record that does not exist.
fn record_key_of(id: &serde_json::Value) -> Option<String> {
    let raw = extract_id_str(id)?;
    Some(
        raw.strip_prefix("graph_events:")
            .unwrap_or(&raw)
            .trim_matches('`')
            .to_string(),
    )
}

/// Companies House company numbers that carry no `Directs` edge yet, in deterministic order.
///
/// Both edge directions are read. The live graph holds 24,120 edges written company→officer and
/// 157 written officer→company; taking only one direction would mis-report 157 companies as
/// uncovered and re-fetch them. That mixed direction is also why Open Item 20 needed a reverse
/// adjacency index before the motif could traverse at all.
pub fn select_uncovered(
    entity_rows: &[serde_json::Value],
    directs_rows: &[serde_json::Value],
) -> Vec<String> {
    let mut covered: HashSet<String> = HashSet::new();
    for row in directs_rows {
        for side in ["src_id", "dst_id"] {
            if let Some(id) = extract_id_str(&row["payload"][side]) {
                if let Some(cn) = id.strip_prefix(crate::backfill::cessation::CH_PREFIX) {
                    covered.insert(cn.to_string());
                }
            }
        }
    }
    let mut out: Vec<String> = entity_rows
        .iter()
        .filter_map(|row| {
            let id = extract_id_str(&row["payload"]["entity_id"])?;
            let cn = id.strip_prefix(crate::backfill::cessation::CH_PREFIX)?;
            (!covered.contains(cn)).then(|| cn.to_string())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Fetches officers for companies that have none and writes them under evidence-derived keys.
pub struct OfficerBackfiller {
    session: DbSession,
    api_key: String,
    rate_limit: u32,
    cache_path: std::path::PathBuf,
    pub dry_run: bool,
    pub restart: bool,
    pub max_fetches: Option<usize>,
    /// Audit the keys already in the graph instead of ingesting new officers.
    pub audit_only: bool,
}

impl OfficerBackfiller {
    pub fn new(session: DbSession, api_key: String, rate_limit: u32, dry_run: bool) -> Self {
        Self {
            session,
            api_key,
            rate_limit,
            cache_path: std::path::PathBuf::from("data/ch-officers-cf-awardees.jsonl"),
            dry_run,
            restart: false,
            max_fetches: None,
            audit_only: false,
        }
    }

    pub fn restart(mut self, restart: bool) -> Self {
        self.restart = restart;
        self
    }

    pub fn audit_only(mut self, audit: bool) -> Self {
        self.audit_only = audit;
        self
    }

    /// Audit the officer keys already in the graph against Companies House evidence.
    ///
    /// Distinct from the write guard, and more important: the guard cannot see what a previous
    /// tool wrote, and `scripts/ch_backfill.py`'s name-only output is still in the graph. A fused
    /// key is asserting a false identity today, and every motif traversing it inherits that error.
    pub async fn run_audit(&self) -> Result<BackfillStats> {
        use crate::planning::mapper::applicant_slug;

        let mut stats = BackfillStats::default();
        let directs_rows = crate::backfill::scan_events_by_type(
            &self.session,
            "EdgeAdded",
            "\"type\":\"Directs\"",
        )
        .await?;
        let edges: Vec<(String, String)> = directs_rows
            .iter()
            .filter_map(|row| {
                Some((
                    extract_id_str(&row["payload"]["src_id"])?,
                    extract_id_str(&row["payload"]["dst_id"])?,
                ))
            })
            .collect();

        let cache = self.load_cache();
        let appointments: Vec<(String, OfficerIdentity, String)> = cache
            .values()
            .flat_map(|c| {
                c.items
                    .iter()
                    .filter(|a| a.is_active())
                    .filter_map(move |a| {
                        Some((
                            applicant_slug(a.name.as_deref()?),
                            a.identity(),
                            c.company_number.clone(),
                        ))
                    })
            })
            .collect();

        let findings = audit_existing_keys(&appointments, &edges);
        stats.scanned = appointments.len();
        stats.matched = findings.iter().filter(|f| f.already_fused).count();
        stats.skipped = findings.len() - stats.matched;

        tracing::info!(
            appointments = stats.scanned,
            colliding_keys = findings.len(),
            already_fused = stats.matched,
            "Officer key audit complete"
        );

        for f in &findings {
            let (severity, detail) = if f.already_fused {
                (
                    AnomalySeverity::High,
                    format!(
                        "This officer node links companies belonging to {} different Companies \
                         House officer ids, and CH publishes no date of birth for any of them, so \
                         nothing corroborates treating them as one person. Every motif traversing \
                         it inherits an unproven identity. Split on the officer id before trusting \
                         a finding that runs through this node.",
                        f.officer_ids.len()
                    ),
                )
            } else {
                (
                    AnomalySeverity::Medium,
                    format!(
                        "{} Companies House officer ids share this key with no date of birth to \
                         corroborate a merge. The graph node currently touches only one of them, \
                         so nothing unproven is asserted yet; a re-run keys them apart on `-o{{id}}` \
                         rather than fusing them.",
                        f.officer_ids.len()
                    ),
                )
            };
            let anomaly = DataAnomaly {
                kind: KIND_OFFICER_KEY_COLLISION.to_string(),
                severity,
                subject: f.key.clone(),
                detail,
                evidence_json: serde_json::json!({
                    "officer_ids": f.officer_ids,
                    "company_numbers": f.company_numbers,
                    "already_fused": f.already_fused,
                })
                .to_string(),
                source: format!("backfill:{OFFICERS_JOB}:audit"),
                status: AnomalyStatus::Open,
            };
            if !self.dry_run {
                crate::backfill::anomaly_sink::record_anomaly(&self.session, &anomaly).await?;
            }
            tracing::warn!(key = %f.key, fused = f.already_fused, ids = ?f.officer_ids, "officer key collision");
        }
        Ok(stats)
    }

    pub fn max_fetches(mut self, max: Option<usize>) -> Self {
        self.max_fetches = max;
        self
    }

    fn load_cache(&self) -> HashMap<String, CachedOfficers> {
        std::fs::read_to_string(&self.cache_path)
            .map(|c| {
                parse_officer_cache(&c)
                    .into_iter()
                    .map(|o| (o.company_number.clone(), o))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn append_cache(&self, entry: &CachedOfficers) -> Result<()> {
        use std::io::Write;
        if let Some(dir) = self.cache_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.cache_path)?;
        writeln!(f, "{}", serde_json::to_string(entry)?)?;
        Ok(())
    }

    /// Split the officer nodes a previous tool already wrote under an uncorroborated key.
    ///
    /// The write guard keys new appointments correctly; it cannot move what is already there.
    /// `scripts/ch_backfill.py` keyed on the name alone, so its output still sits on bare
    /// `director:{slug}` keys, and where Companies House publishes no date of birth nothing
    /// corroborates treating those appointments as one person.
    ///
    /// Rewrites `graph_events`, the append-only source of truth — a data correction, not a routine
    /// write. Dry-run by default, and every write is verified to have returned a row, because
    /// SurrealDB reports `status: OK` with an empty result for an UPDATE that matched nothing.
    /// A `project` run is required afterwards, then a bake to re-derive alerts.
    pub async fn run_split_collisions(&self) -> Result<BackfillStats> {
        use crate::planning::mapper::applicant_slug;

        let mut stats = BackfillStats::default();
        let directs_rows = crate::backfill::scan_events_by_type(
            &self.session,
            "EdgeAdded",
            "\"type\":\"Directs\"",
        )
        .await?;
        let edges: Vec<(String, String)> = directs_rows
            .iter()
            .filter_map(|row| {
                Some((
                    extract_id_str(&row["payload"]["src_id"])?,
                    extract_id_str(&row["payload"]["dst_id"])?,
                ))
            })
            .collect();

        let cache = self.load_cache();
        let audit_input: Vec<(String, OfficerIdentity, String)> = cache
            .values()
            .flat_map(|c| {
                c.items
                    .iter()
                    .filter(|a| a.is_active())
                    .filter_map(move |a| {
                        Some((
                            applicant_slug(a.name.as_deref()?),
                            a.identity(),
                            c.company_number.clone(),
                        ))
                    })
            })
            .collect();

        // Only keys the audit still reports: uncorroborated, and therefore unsafe to leave fused.
        let targets: HashSet<String> = audit_existing_keys(&audit_input, &edges)
            .into_iter()
            .map(|f| f.key)
            .collect();
        if targets.is_empty() {
            tracing::info!("No uncorroborated officer-key collisions to split.");
            return Ok(stats);
        }

        let appointments: Vec<AppointmentRef> = cache
            .values()
            .flat_map(|c| {
                c.items
                    .iter()
                    .filter(|a| a.is_active())
                    .map(move |a| (c, a))
            })
            .filter_map(|(c, a)| {
                let slug = applicant_slug(a.name.as_deref()?);
                let ident = a.identity();
                let key = officer_key(&slug, &ident);
                if !targets.contains(&key) {
                    return None;
                }
                Some(AppointmentRef {
                    key,
                    officer_id: ident.officer_id?,
                    company_number: c.company_number.clone(),
                    appointed_on: a.appointed_on.clone(),
                    officer_role: a.officer_role.clone(),
                })
            })
            .collect();

        let node_rows = crate::backfill::scan_events_by_type(
            &self.session,
            "NodeAdded",
            "\"type\":\"Officer\"",
        )
        .await?;
        let node_events: Vec<EventRef> = node_rows
            .iter()
            .filter_map(|row| {
                let key = extract_id_str(&row["payload"]["entity_id"])?;
                if !targets.contains(&key) {
                    return None;
                }
                let weight: serde_json::Value =
                    serde_json::from_str(row["payload"]["weight_json"].as_str()?).ok()?;
                Some(EventRef {
                    record_id: record_key_of(&row["id"])?,
                    key,
                    company_number: weight["company_number"].as_str().map(str::to_owned),
                    appointed_on: weight["appointed_on"].as_str().map(str::to_owned),
                    officer_role: weight["officer_role"].as_str().map(str::to_owned),
                })
            })
            .collect();
        let edge_events: Vec<EventRef> = directs_rows
            .iter()
            .filter_map(|row| {
                let key = extract_id_str(&row["payload"]["dst_id"])?;
                if !targets.contains(&key) {
                    return None;
                }
                let src = extract_id_str(&row["payload"]["src_id"])?;
                Some(EventRef {
                    record_id: record_key_of(&row["id"])?,
                    key,
                    company_number: src
                        .strip_prefix(crate::backfill::cessation::CH_PREFIX)
                        .map(str::to_owned),
                    appointed_on: None,
                    officer_role: None,
                })
            })
            .collect();

        let plan = plan_collision_split(&appointments, &node_events, &edge_events);
        stats.scanned = node_events.len() + edge_events.len();
        stats.matched = plan.rekeys.len();
        stats.skipped = plan.unattributable.len();
        tracing::info!(
            keys = targets.len(),
            events = stats.scanned,
            rekeys = stats.matched,
            unattributable = stats.skipped,
            dry_run = self.dry_run,
            "Officer collision split planned"
        );
        for r in &plan.rekeys {
            tracing::info!(record = %r.record_id, from = %r.from, to = %r.to, "re-key");
        }
        if !plan.unattributable.is_empty() {
            tracing::warn!(
                events = ?plan.unattributable,
                "events left on their current key — no Companies House evidence attributes them \
                 to one officer, and guessing is the failure this split exists to undo"
            );
        }
        if self.dry_run {
            return Ok(stats);
        }

        for r in &plan.rekeys {
            let sql = format!(
                "UPDATE type::record($tb, $key) SET payload.{} = $v",
                r.field
            );
            let written: Vec<serde_json::Value> = self
                .session
                .query(
                    &sql,
                    &[
                        ("tb", serde_json::json!("graph_events")),
                        ("key", serde_json::json!(r.record_id)),
                        ("v", serde_json::json!(r.to)),
                    ],
                )
                .await?
                .take(0)?;
            if written.is_empty() {
                anyhow::bail!(
                    "re-key matched NO ROWS despite status OK for {} — refusing to report it as \
                     a write",
                    r.record_id
                );
            }
        }
        Ok(stats)
    }

    pub async fn run(&self) -> Result<BackfillStats> {
        use crate::planning::mapper::applicant_slug;

        if self.audit_only {
            return self.run_audit().await;
        }

        use urban_integrity_schema::types::{
            edge::{DirectsData, EdgeMeta, EdgeWeight},
            node::{NodeWeight, OfficerData},
        };

        let mut stats = BackfillStats::default();

        let entity_rows =
            crate::backfill::scan_events_by_type(&self.session, "NodeAdded", "\"type\":\"Entity\"")
                .await?;
        let directs_rows = crate::backfill::scan_events_by_type(
            &self.session,
            "EdgeAdded",
            "\"type\":\"Directs\"",
        )
        .await?;

        let targets = select_uncovered(&entity_rows, &directs_rows);
        stats.scanned = targets.len();
        tracing::info!(
            uncovered_companies = stats.scanned,
            "Officer backfill: companies with no Directs edge"
        );
        if targets.is_empty() {
            return Ok(stats);
        }

        let cache = self.load_cache();
        let cached = targets.iter().filter(|cn| cache.contains_key(*cn)).count();
        tracing::info!(
            cache_entries = cache.len(),
            targets_already_cached = cached,
            needing_fetch = targets.len() - cached,
            "Officer backfill: cache state"
        );

        let cursor = ResumeCursor::new(self.session.clone(), OFFICERS_JOB);
        let mut start = 0usize;
        if !self.dry_run {
            if self.restart {
                cursor.clear().await?;
            } else if let Some(state) = cursor.load().await? {
                start = resume_index(&targets, &state.last_record_id, |t| t.as_str());
                stats.matched = state.matched;
                stats.skipped = state.skipped;
                stats.errors = state.errors;
                tracing::info!(resumed_after = %state.last_record_id, skipping = start, "Officer backfill: resuming");
            }
        }

        let rest = crate::backfill::ch::ChRestClient::new(self.api_key.clone(), self.rate_limit);
        let mut fetches = 0usize;

        // Gather every appointment first, then plan the whole keyspace at once. Planning per
        // company would miss the collisions that matter: two people sharing a name appear at
        // DIFFERENT companies, so a per-company view never sees them together.
        let mut appointments: Vec<(String, OfficerIdentity, String, CachedAppointment)> =
            Vec::new();
        let mut last_seen = String::new();

        for cn in targets.iter().skip(start) {
            let entry = match cache.get(cn) {
                Some(e) => e.clone(),
                None => {
                    if self.dry_run {
                        stats.skipped += 1;
                        continue;
                    }
                    if self.max_fetches.is_some_and(|m| fetches >= m) {
                        tracing::info!(fetches, "Officer backfill: --max-fetches reached");
                        break;
                    }
                    fetches += 1;
                    match rest.company_officers(cn).await {
                        Ok(Some(items)) => {
                            let e = CachedOfficers {
                                company_number: cn.clone(),
                                items,
                            };
                            self.append_cache(&e)?;
                            e
                        }
                        Ok(None) => break, // rate-limited; resume next run
                        Err(e) => {
                            tracing::warn!(cn, error = %e, "CH officers fetch failed");
                            stats.errors += 1;
                            continue;
                        }
                    }
                }
            };
            last_seen = cn.clone();
            for item in entry.items.iter().filter(|i| i.is_active()) {
                let Some(name) = item.name.as_deref() else {
                    continue;
                };
                appointments.push((
                    applicant_slug(name),
                    item.identity(),
                    cn.clone(),
                    item.clone(),
                ));
            }
        }

        let planned: Vec<(String, OfficerIdentity)> = appointments
            .iter()
            .map(|(slug, ident, _, _)| (slug.clone(), ident.clone()))
            .collect();
        let plan = plan_keys(&planned);
        let resolver = plan.resolver();

        // A refusal is a FINDING, not a log line. Two Companies House officers sharing a name —
        // and sometimes a birth month — inside one procurement corpus is exactly the shape an
        // analyst should see, whether it is coincidence or deliberate. Recording it as an anomaly
        // makes it queryable and puts it in the evidence report; a `tracing::warn!` alone would be
        // gone by the next run, which is how every silent-loss defect in this project survived.
        if !plan.refused.is_empty() {
            tracing::warn!(
                refused = plan.refused.len(),
                "Officer backfill: keys REFUSED — an appointment on each carries no Companies \
                 House officer id, so it cannot be attributed to either candidate and cannot be \
                 keyed apart. Writing it would repeat the 94 name-only merges split on 2026-08-11 \
                 (Open Item 22). Recorded as `officer_key_collision` anomalies."
            );
            for r in &plan.refused {
                let names: Vec<&str> = appointments
                    .iter()
                    .filter(|(slug, ident, _, _)| officer_key(slug, ident) == r.key)
                    .filter_map(|(_, _, _, item)| item.name.as_deref())
                    .collect();
                let companies: Vec<&str> = appointments
                    .iter()
                    .filter(|(slug, ident, _, _)| officer_key(slug, ident) == r.key)
                    .map(|(_, _, cn, _)| cn.as_str())
                    .collect();
                let anomaly = DataAnomaly {
                    kind: KIND_OFFICER_KEY_COLLISION.to_string(),
                    severity: AnomalySeverity::High,
                    subject: r.key.clone(),
                    detail: format!(
                        "{} Companies House officers share this key, no date of birth corroborates \
                         a merge, and at least one appointment carries no CH officer id to key it \
                         apart on. Those appointments were NOT written, so Phoenix cannot traverse \
                         them. Adjudicate before assuming the officer is absent from the graph.",
                        r.officer_ids.len()
                    ),
                    evidence_json: serde_json::json!({
                        "officer_ids": r.officer_ids,
                        "names": names,
                        "company_numbers": companies,
                        "why_refused": "uncorroborated `director:` key carrying an appointment CH \
                            published no officer id for; exact-match-only policy forbids \
                            resolving this by similarity",
                    })
                    .to_string(),
                    source: format!("backfill:{OFFICERS_JOB}"),
                    status: AnomalyStatus::Open,
                };
                if !self.dry_run {
                    crate::backfill::anomaly_sink::record_anomaly(&self.session, &anomaly).await?;
                }
                tracing::warn!(key = %r.key, officer_ids = ?r.officer_ids, "refused officer key");
            }
        }

        for (slug, ident, cn, item) in &appointments {
            // The resolver, never `officer_key` directly: an uncorroborated collision is written
            // under its `-o{id}` split key, and re-deriving the bare key here would put both
            // people back on one node.
            let Some(key) = resolver.key_for(slug, ident) else {
                stats.skipped += 1;
                continue;
            };
            if self.dry_run {
                stats.matched += 1;
                continue;
            }
            let officer = OfficerData {
                name: item.name.clone().unwrap_or_default(),
                officer_role: item.officer_role.clone(),
                appointed_on: item.appointed_on.clone(),
                resigned_on: item.resigned_on.clone(),
                // The officer's company scope lives on the weight, never in the key — the key is
                // `director:{slug}` precisely so one human is one node across every appointment.
                company_number: Some(cn.clone()),
            };
            crate::backfill::upsert_node_updated_event(
                &self.session,
                OFFICERS_JOB,
                &key,
                &NodeWeight::Officer(officer),
            )
            .await?;
            crate::backfill::upsert_edge_event(
                &self.session,
                OFFICERS_JOB,
                &format!("{}{cn}", crate::backfill::cessation::CH_PREFIX),
                &key,
                &EdgeWeight::Directs {
                    meta: EdgeMeta {
                        created_at: chrono::Utc::now(),
                        effective_from: None,
                        effective_to: None,
                    },
                    data: DirectsData {
                        details: "backfill:ch_officers".to_string(),
                        officer_role: item.officer_role.clone(),
                    },
                },
            )
            .await?;
            stats.matched += 1;
        }

        if !self.dry_run && !last_seen.is_empty() {
            cursor
                .save(&CursorState {
                    last_record_id: last_seen,
                    processed: stats.matched + stats.skipped,
                    matched: stats.matched,
                    skipped: stats.skipped,
                    errors: stats.errors,
                })
                .await?;
        }
        tracing::info!(
            scanned = stats.scanned,
            matched = stats.matched,
            skipped = stats.skipped,
            errors = stats.errors,
            refused_keys = plan.refused.len(),
            fetches,
            "Officer backfill complete — refused keys are recorded as `officer_key_collision` \
             anomalies, queryable via `anomalies list`"
        );
        Ok(stats)
    }
}
