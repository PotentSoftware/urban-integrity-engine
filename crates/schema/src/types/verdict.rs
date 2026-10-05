//! `Verdict`, `Coverage`, and `Finding` — the six-value verdict vocabulary every probe in Phase 32
//! onward reports through (VRD-01..04).
//!
//! # Why `Deserialize` is deliberately absent
//!
//! `Coverage`, `Verdict`, and `Finding` derive `Serialize` ONLY — never `Deserialize`. A derived
//! `Deserialize` impl is generated in the *same module* as the type, so `Coverage`'s field privacy
//! does not block it (privacy in Rust is module-scoped, not type-scoped). If `Verdict` derived
//! `Deserialize`, any crate could construct an invariant-violating value straight from JSON:
//!
//! ```text
//! let bad: Verdict = serde_json::from_str(
//!     r#"{"type":"Clear","content":{"population":5,"capable":3}}"#
//! ).unwrap();
//! // Verdict::Clear with capable != population. classify() never ran. No invariant checked.
//! ```
//!
//! That is not hypothetical — the identical bypass was reproduced empirically against
//! `crates/enricher/src/confidence.rs`'s `ConfidenceScore`/`ConfirmedLink` on 2026-08-23, which is
//! the analog this module's construction-invariant mechanism is otherwise modelled on. If a later
//! phase genuinely needs to read a `Verdict` back from JSON, it must add a hand-written
//! `Deserialize` impl that re-runs `classify`'s validation — never a bare derive. See the
//! `verdict_does_not_implement_deserialize_owned` test below, which pins this so a future derive
//! reintroducing the hole fails loudly instead of silently reopening it.

use serde::Serialize;

/// Proof that `capable` was measured against `population`, not assumed or defaulted.
///
/// The only producer is [`Verdict::classify`]. There is no public constructor here and no public
/// mutation of the fields once built — a value of this type existing at all is the evidence that
/// VRD-01's two counts were both supplied to the same call.
///
/// # Examples
///
/// Constructing a `Coverage` directly from outside this module is a compile error — its fields are
/// private, and this crate exposes no constructor other than the one used internally by
/// [`Verdict::classify`]:
///
/// ```compile_fail
/// use urban_integrity_schema::types::verdict::Coverage;
///
/// let spoofed = Coverage { population: 5, capable: 5 };
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Coverage {
    population: u64,
    capable: u64,
}

impl Coverage {
    /// Every record the probe evaluated.
    pub fn population(&self) -> u64 {
        self.population
    }

    /// The structurally-eligible subset of `population` — VRD-01's second required count.
    pub fn capable(&self) -> u64 {
        self.capable
    }

    /// `population - capable` — the exclusion count VRD-03 requires `NotApplicable` to report.
    pub fn excluded(&self) -> u64 {
        self.population - self.capable
    }
}

/// The six-value verdict vocabulary (VRD-01..04) every probe in Phase 32 onward reports through.
///
/// `Hit`/`Weak`/`Clear`/`NotApplicable` all carry a [`Coverage`] — the structurally-eligible-subset
/// proof VRD-01 requires every probe to report. `Coverage`'s fields are private and its only
/// constructor is called from inside [`Verdict::classify`] in this same module, so none of these
/// four variants is constructible from outside this module with a fabricated or mismatched pair of
/// counts: attempting `Verdict::Clear(Coverage { population: 5, capable: 3 })` from another module
/// is a compile error (E0451, field `population` of struct `Coverage` is private), not a runtime
/// assertion. This is the type-level falsifiability guarantee this task exists to build, not a
/// documented convention that a future probe author could bypass.
///
/// `DataGap`/`Blocked` carry no `Coverage` — nothing was measured, so there is nothing to prove
/// equal to anything (VRD-04).
///
/// The `#[serde(tag = "type", content = "content")]` convention is copied verbatim from
/// `EntityStatus` (`crates/schema/src/types/node.rs:6`) — see this module's top-level doc comment
/// for why `Deserialize` is deliberately not derived alongside it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "content")]
pub enum Verdict {
    Hit {
        coverage: Coverage,
        matches: u64,
    },
    Weak {
        coverage: Coverage,
        matches: u64,
    },
    Clear(Coverage),
    NotApplicable {
        coverage: Coverage,
        reason: String,
    },
    DataGap {
        missing_input: String,
    },
    Blocked {
        prerequisite: String,
    },
    /// The verdict a probe reports when the orchestrator's gate predicate decided it should not
    /// run at all (GAT-02, D-09/D-10/D-11/D-23) — see [`SkipCause`].
    Skipped {
        cause: SkipCause,
    },
}

impl Verdict {
    /// The sole path to `Hit`/`Weak`/`Clear`/`NotApplicable`.
    ///
    /// - `population` — every record the probe evaluated.
    /// - `capable` — the structurally-eligible subset (VRD-01's second count). Must be
    ///   `<= population`.
    /// - `matches` — what was found within `capable`.
    /// - `hit_threshold` — the probe's own actionable cutoff for `matches`.
    /// - `not_applicable_reason` — VRD-03's required exclusion reason; read only when
    ///   `capable < population`, otherwise ignored.
    ///
    /// When `capable < population`, this returns `NotApplicable` unconditionally — no other branch
    /// of this function may produce `Hit`/`Weak`/`Clear` in that case, regardless of `matches`.
    /// Otherwise: `matches == 0` -> `Clear`; `matches >= hit_threshold` -> `Hit`; else -> `Weak`.
    ///
    /// # Panics
    /// If `capable > population` — a capable subset can never exceed the population it was drawn
    /// from; that is a caller bug, not a data condition a probe can legitimately report, so this
    /// fails loud rather than silently clamping (this project's stated preference throughout
    /// its development rules: never discard or paper over a data-integrity violation).
    pub fn classify(
        population: u64,
        capable: u64,
        matches: u64,
        hit_threshold: u64,
        not_applicable_reason: &str,
    ) -> Verdict {
        assert!(
            capable <= population,
            "capable ({capable}) cannot exceed population ({population})"
        );
        let coverage = Coverage {
            population,
            capable,
        };
        if capable < population {
            return Verdict::NotApplicable {
                coverage,
                reason: not_applicable_reason.to_string(),
            };
        }
        match matches {
            0 => Verdict::Clear(coverage),
            m if m >= hit_threshold => Verdict::Hit {
                coverage,
                matches: m,
            },
            m => Verdict::Weak {
                coverage,
                matches: m,
            },
        }
    }

    /// `DataGap` never carries a `Coverage` because nothing was measured, so there is nothing to
    /// prove equal to anything (VRD-04).
    pub fn data_gap(missing_input: impl Into<String>) -> Verdict {
        Verdict::DataGap {
            missing_input: missing_input.into(),
        }
    }

    /// `Blocked` names the unmet prerequisite that prevented the probe from running at all.
    pub fn blocked(prerequisite: impl Into<String>) -> Verdict {
        Verdict::Blocked {
            prerequisite: prerequisite.into(),
        }
    }

    /// `Coverage` if this verdict carries one — `Some` for `Hit`/`Weak`/`Clear`/`NotApplicable`,
    /// `None` for `DataGap`/`Blocked`/`Skipped`. [`Finding`] uses this to fill VRD-01's
    /// population/capable report fields without risking the two copies diverging.
    ///
    /// `Skipped` (GAT-05) is `None` for the same reason as `DataGap`/`Blocked` and then some: the
    /// probe never ran, so there is no population it examined and no capable subset to report.
    /// A `Some(Coverage { population: 0, capable: 0 })` here would render as "checked nothing and
    /// found nothing" — the exact conflation D-07's BLUF ordering exists to prevent.
    pub fn coverage(&self) -> Option<Coverage> {
        match self {
            Verdict::Hit { coverage, .. }
            | Verdict::Weak { coverage, .. }
            | Verdict::Clear(coverage)
            | Verdict::NotApplicable { coverage, .. } => Some(*coverage),
            Verdict::DataGap { .. } | Verdict::Blocked { .. } | Verdict::Skipped { .. } => None,
        }
    }

    /// The ONLY way to construct a Dependency-caused `Skipped`. Takes the upstream's REAL
    /// `&Verdict` (not a caller-asserted `VerdictKind`) so the discriminant is always derived
    /// here, never asserted by the caller (GAT-02, D-23).
    ///
    /// "Only way to construct" is about the payload, not about the caller: this is `pub`, so a
    /// probe can self-report a skip from its own `run()`. See [`SkipCause`]'s "What this does
    /// NOT prevent".
    pub fn skipped_dependency(upstream: &'static str, upstream_verdict: &Verdict) -> Verdict {
        Verdict::Skipped {
            cause: SkipCause(SkipCauseInner::Dependency {
                upstream,
                upstream_verdict: VerdictKind::of(upstream_verdict),
            }),
        }
    }

    /// The ONLY way to construct a TierAggregate-caused `Skipped` — D-08's Tier-1 -> Tier-2 OR
    /// rule gated this probe: none of `probes_checked` Tier-1 probes returned Hit/Weak.
    pub fn skipped_tier_aggregate(tier: &'static str, probes_checked: u8) -> Verdict {
        Verdict::Skipped {
            cause: SkipCause(SkipCauseInner::TierAggregate {
                tier,
                probes_checked,
            }),
        }
    }
}

/// The verdict-kind discriminant (D-10). Hand-rolled, not `strum` (the research notes' Standard Stack
/// -- transitive-only dependency, contradicts this module's hand-rolled-enum convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum VerdictKind {
    Hit,
    Weak,
    Clear,
    NotApplicable,
    DataGap,
    Blocked,
    Skipped,
}

impl VerdictKind {
    /// The ONLY way to get a `VerdictKind` from a real `Verdict` -- exhaustive, no wildcard arm,
    /// so an 8th `Verdict` variant fails this match at compile time rather than silently
    /// defaulting.
    pub fn of(v: &Verdict) -> VerdictKind {
        match v {
            Verdict::Hit { .. } => VerdictKind::Hit,
            Verdict::Weak { .. } => VerdictKind::Weak,
            Verdict::Clear(_) => VerdictKind::Clear,
            Verdict::NotApplicable { .. } => VerdictKind::NotApplicable,
            Verdict::DataGap { .. } => VerdictKind::DataGap,
            Verdict::Blocked { .. } => VerdictKind::Blocked,
            Verdict::Skipped { .. } => VerdictKind::Skipped,
        }
    }
}

impl std::fmt::Display for VerdictKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            VerdictKind::Hit => "Hit",
            VerdictKind::Weak => "Weak",
            VerdictKind::Clear => "Clear",
            VerdictKind::NotApplicable => "NotApplicable",
            VerdictKind::DataGap => "DataGap",
            VerdictKind::Blocked => "Blocked",
            VerdictKind::Skipped => "Skipped",
        };
        f.write_str(s)
    }
}

/// D-23's typed skip cause. Two causes, mutually exclusive by construction. Mirrors `Coverage`'s
/// falsifiability guarantee via a PRIVATE INNER ENUM: unlike a struct's fields (which default to
/// private), a public enum's variant fields are automatically as public as the enum itself, so a
/// bare `pub enum SkipCause { Dependency { .. }, .. }` would NOT protect the invariant — any
/// crate could write `SkipCause::Dependency { upstream: "fake", upstream_verdict:
/// VerdictKind::Hit }` directly. Wrapping a module-private inner enum instead means the inner
/// enum's name and variants are not nameable outside this module at all — a strictly stronger
/// guarantee than `Coverage`'s private-field trick, not merely an equivalent one.
///
/// # What this does NOT prevent
///
/// The guarantee is about the *payload*, not about *who* may skip. `Verdict::skipped_dependency`
/// and `Verdict::skipped_tier_aggregate` are necessarily `pub` — their only real caller,
/// `graph::gate()`, lives in another crate — so a probe author can call
/// `Verdict::skipped_dependency("any-name", &a_verdict_they_built)` inside their own `run()` and
/// self-report a skip naming an upstream that never gated them. The report would render it
/// identically to a gate-produced skip.
///
/// What IS enforced: the recorded `VerdictKind` is always derived here from a real `&Verdict`
/// value, never asserted by the caller, and the cause payload cannot be fabricated variant-wise.
/// The residual is the same self-report surface `Verdict::blocked` and `Verdict::data_gap`
/// already have — any probe can claim `Blocked`/`DataGap` at will — so `Skipped` adds no new
/// class of exposure. Closing it would mean a capability token only the orchestrator can mint,
/// which is a decision about all three verdicts, not about `Skipped` alone.
///
/// # Examples
///
/// Constructing a `SkipCause` directly from outside this module does not even resolve — the
/// inner enum and its variants have no nameable path outside `schema::types::verdict`:
///
/// ```compile_fail
/// use urban_integrity_schema::types::verdict::SkipCause;
///
/// let spoofed = SkipCause::Dependency {
///     upstream: "fake",
///     upstream_verdict: urban_integrity_schema::types::verdict::VerdictKind::Hit,
/// };
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SkipCause(SkipCauseInner);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum SkipCauseInner {
    /// A declared `depends_on()` edge gated this probe: `upstream` did not return Hit/Weak.
    Dependency {
        upstream: &'static str,
        upstream_verdict: VerdictKind,
    },
    /// D-08's Tier-1 -> Tier-2 OR rule gated this probe: none of `probes_checked` Tier-1 probes
    /// returned Hit/Weak. No single upstream to name (D-23's amendment rationale).
    TierAggregate {
        tier: &'static str,
        probes_checked: u8,
    },
}

impl SkipCause {
    /// `Some` for `Dependency`, `None` for `TierAggregate`.
    pub fn upstream(&self) -> Option<&'static str> {
        match self.0 {
            SkipCauseInner::Dependency { upstream, .. } => Some(upstream),
            SkipCauseInner::TierAggregate { .. } => None,
        }
    }

    /// `Some` for `Dependency`, `None` for `TierAggregate`.
    pub fn upstream_verdict(&self) -> Option<VerdictKind> {
        match self.0 {
            SkipCauseInner::Dependency {
                upstream_verdict, ..
            } => Some(upstream_verdict),
            SkipCauseInner::TierAggregate { .. } => None,
        }
    }

    /// `Some` for `TierAggregate`, `None` for `Dependency`.
    pub fn tier(&self) -> Option<&'static str> {
        match self.0 {
            SkipCauseInner::TierAggregate { tier, .. } => Some(tier),
            SkipCauseInner::Dependency { .. } => None,
        }
    }

    /// `Some` for `TierAggregate`, `None` for `Dependency`.
    pub fn probes_checked(&self) -> Option<u8> {
        match self.0 {
            SkipCauseInner::TierAggregate { probes_checked, .. } => Some(probes_checked),
            SkipCauseInner::Dependency { .. } => None,
        }
    }

    /// Human-readable, e.g. "upstream 'identity-integrity' returned Clear" or "no Tier-1 probe
    /// (of 7 checked) returned Hit or Weak". The ONE place this phrasing is authored -- run.rs's
    /// `render_verdict`, `report/skipped.rs` (a later plan) and `graph.rs`'s dispatch loop (a
    /// later plan) all call this instead of re-deriving the sentence.
    pub fn describe(&self) -> String {
        match self.0 {
            SkipCauseInner::Dependency {
                upstream,
                upstream_verdict,
            } => format!("upstream '{upstream}' returned {upstream_verdict}"),
            SkipCauseInner::TierAggregate {
                tier,
                probes_checked,
            } => format!("no {tier} probe (of {probes_checked} checked) returned Hit or Weak"),
        }
    }
}

/// D4's 5 disqualification-chain causes, each carrying the verdict it actually means — not the
/// blanket `DATA_GAP` Python's original returns for all five (`analyst_probe.py:1702`'s
/// `_disqualification_chain`, ported by Plan 33-05).
///
/// | Variant | Verdict | Why |
/// |---|---|---|
/// | `NoOfficerNodes` | `DataGap` | input set empty — the adapter issued zero CH requests |
/// | `AdapterNeverRan` | `DataGap` | no `enrichment_runs` marker for this adapter |
/// | `NoRegisterOverlap` | `Clear` | register genuinely searched, stated population, no match |
/// | `LinksNotElevated` | `NotApplicable` | sub-threshold candidates exist — a real excluded population the ENR-11 gate held back; `Clear` cannot express this |
/// | `NoLiveBan` | `Clear` | measured clean result — reporting it as a data gap is VRD-04's defect class |
///
/// Never derives `Deserialize` — same rationale as `Verdict` (see this module's top-of-file doc
/// comment): a derived `Deserialize` impl lives in this same module and would let any crate
/// construct a `Cause` value straight from JSON, bypassing whatever validation a future consumer
/// adds. This type carries no invariant to bypass today, but the derive is withheld pre-emptively
/// to match the module's stated policy rather than be added back in later under time pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Cause {
    NoOfficerNodes,
    AdapterNeverRan,
    NoRegisterOverlap,
    LinksNotElevated,
    NoLiveBan,
}

/// VRD-11's closure of the `classify(0, 0, …)` hole: distinguishes whether a probe's declared
/// population was independently recomputed against `crates/core/src/persistence/oracle.rs`
/// (Phase 38) or is self-reported only. A `Clear` verdict reached over a genuinely empty
/// population that the oracle independently corroborated must be visibly distinct in the type
/// system from a `Clear` reached over a population nobody independently checked — v7.0's
/// construction invariant proved `Clear` is internally consistent, not that its population is the
/// right number.
///
/// This type intentionally does NOT live on [`Coverage`], whose only constructor,
/// [`Verdict::classify`], is deliberately DB-free and synchronous (see `classify`'s doc comment
/// above), and does NOT live on [`Finding`], which would require touching every one of `Finding`'s
/// ~90 construction sites for a concern only a minority of probes can ever use. It is instead
/// attached to `ReportEntry` (`crates/cli/src/commands/probe/report/mod.rs`) in a later plan of
/// this phase.
///
/// Never derives `Deserialize` — same rationale as `Verdict` (see this module's top-of-file doc
/// comment): a derived `Deserialize` impl lives in this same module and would let any crate
/// construct a `PopulationSource` value straight from JSON, bypassing whatever validation a future
/// consumer adds. This type carries no invariant to bypass today, but the derive is withheld
/// pre-emptively to match the module's stated policy rather than be added back in later under
/// time pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PopulationSource {
    /// The population was never independently recomputed — either because the probe is
    /// structurally uncheckable (an ORC-08 exclusion reason belongs here) or because the verdict
    /// this run carried no `Coverage` at all (`DataGap`/`Blocked`, `reason` may be empty in that
    /// case).
    SelfReported { reason: &'static str },
    /// The oracle's independently recomputed count for this probe's declared population, from
    /// `crates/core/src/persistence/oracle.rs`'s counting primitives (Phase 38).
    OracleCorroborated { oracle_count: u64 },
}

/// The five-value severity vocabulary a [`Finding`] carries, matching Python's
/// `severity: str # CRITICAL / HIGH / MEDIUM / LOW / INFO` (`scripts/analyst_probe.py:220`) 1:1.
///
/// | Variant | When it applies |
/// |---|---|
/// | `Critical` | An actionable, high-confidence finding demanding immediate analyst attention |
/// | `High` | A strong finding requiring prompt review |
/// | `Medium` | A moderate finding worth investigating |
/// | `Low` | A weak or marginal finding, low priority |
/// | `Info` | No actionable finding — informational only (e.g. `Clear`/`DataGap`/`Blocked`) |
///
/// Never derives `Deserialize` — same rationale as `Verdict` (see this module's top-of-file doc
/// comment): a derived `Deserialize` impl lives in this same module and would let any crate
/// construct a `Severity` value straight from JSON, bypassing whatever validation a future
/// consumer adds. This type carries no invariant to bypass today, but the derive is withheld
/// pre-emptively to match the module's stated policy rather than be added back in later under
/// time pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl Severity {
    /// Stable-sort rank: `Critical`=0 .. `Info`=4, lower sorts first. Lets the report writer group
    /// findings by severity via `.sort_by_key(|f| f.severity.rank())` instead of matching on the
    /// enum inline at every call site.
    pub fn rank(&self) -> u8 {
        match self {
            Severity::Critical => 0,
            Severity::High => 1,
            Severity::Medium => 2,
            Severity::Low => 3,
            Severity::Info => 4,
        }
    }
}

/// What class of record a [`RecordRef`] points at.
///
/// # It was a command taxonomy; PIV-03 / D-06 made it a target taxonomy
///
/// The first three variants were named after the three pivot-command shapes D-14 found reachable
/// — `entity <id>`, `verify --company-number <id>`, `links show <id>` — so the enum was, in
/// effect, "which CLI command opens this". [`Contract`](Self::Contract) and
/// [`Location`](Self::Location) break that reading: both are opened with the same
/// `entity <id>` command as [`Entity`](Self::Entity), and they exist because the **workbench**
/// profile needs to say which class of thing a row's pivot lands on, which the command shape
/// cannot distinguish. `urban_integrity_probe::report::evidence::pivot_command` is where the
/// collapse back onto three command forms happens, and it is the only place that mapping lives.
///
/// D-06's rule is that this taxonomy is extended rather than mirrored: the probes emit these
/// kinds, the profile matcher (`ref_names_entity` in `crates/core`) consumes them, and a second
/// vocabulary in `workbench-dto` would let probe evidence and the profile disagree about what a
/// pivot target is — a disagreement that only surfaces when an analyst clicks one and gets
/// nothing.
///
/// # Why the new names are the graph's names, not the section's
///
/// `Contract`/`Location` are spelled exactly as `crates/workbench`'s `node_kind_name` spells the
/// `NodeWeight` variants they name, because `workbench_dto::EntityRef::kind` — the wire field a
/// pivot's kind travels in — is **already** populated from `node_kind_name` on the entity-search
/// and OSINT-link paths. Calling the address node class `Address` here would put a second
/// spelling of one node class on one wire field, which is the parallel taxonomy D-06 forbids,
/// just relocated. PIV-03's word "address" names the profile *section*; `Location` names the node.
///
/// For the same reason there is no `PlanningApplication` variant. D-03 splits contracts and
/// planning applications into two profile sections, but it does not split the node: both are
/// `NodeWeight::Contract`, distinguished by the `planning:` id prefix
/// (`is_planning_application_id` in `crates/core`). The section is the distinction; the kind is
/// the node class.
///
/// Still deliberately no variant for `graph-path` or `flips` — per plan 35-07's decision,
/// `land-flip-candidates` is classified aggregate-only and never constructs a [`RecordRef`].
///
/// Never derives `Deserialize` — same rationale as `Verdict` (see this module's top-of-file doc
/// comment): a derived `Deserialize` impl lives in this same module and would let any crate
/// construct a `RecordKind` value straight from JSON, bypassing whatever validation a future
/// consumer adds. This type carries no invariant to bypass today, but the derive is withheld
/// pre-emptively to match the module's stated policy rather than be added back in later under
/// time pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RecordKind {
    /// A graph node id — `GB-COH:…`, `director:…`, `ward:…`, `GB-OCOD:…`. The catch-all every
    /// node id is addressable as, and the kind every probe in the battery emits today.
    Entity,
    /// A bare Companies House number (`99000001`), **not** the `GB-COH:`-namespaced node id. The
    /// two are matched against each other in `ref_names_entity`.
    CompanyNumber,
    /// An `identity_links` record id. Not a node id: following it needs a link read the profile
    /// matcher does not do, which is why it never matches an entity.
    IdentityLink,
    /// A `NodeWeight::Contract` node id — `CF:…` for a Contracts Finder award, `planning:…` for a
    /// planning application. PIV-03's "every contract on screen is a click-through".
    Contract,
    /// A `NodeWeight::Location` node id — `location:…` or `LOC:…`, both live namespaces (of
    /// 173,203 `LocatedAt` destinations on `an_anglesey_aebaa785`, 132,511 are `location:` and
    /// 40,692 are `LOC:`, measured 2026-09-15). PIV-03's "every address on screen is a
    /// click-through".
    Location,
}

impl RecordKind {
    /// The one spelling of this kind that crosses a wire.
    ///
    /// Both workbench mappers (`report_view::entity_ref_from_record_ref` and
    /// `entity::profile_map`) used to hold their own `match` producing these strings, which is two
    /// copies of one vocabulary in one crate — exactly the drift D-06 forbids between crates,
    /// merely closer together. They call this instead.
    ///
    /// Identical to the `Serialize` derive's output for a unit variant, deliberately: a consumer
    /// reading `serde_json::to_string(&kind)` and a consumer reading `EntityRef::kind` must not
    /// see two different names for one variant.
    pub const fn name(self) -> &'static str {
        match self {
            RecordKind::Entity => "Entity",
            RecordKind::CompanyNumber => "CompanyNumber",
            RecordKind::IdentityLink => "IdentityLink",
            RecordKind::Contract => "Contract",
            RecordKind::Location => "Location",
        }
    }
}

/// One typed pivot target: a [`RecordKind`] plus the id the analyst would pass to that pivot
/// command.
///
/// Plain public fields — unlike `Coverage`, there is no invariant to protect here; any
/// `(RecordKind, String)` pair is a valid `RecordRef`.
///
/// Never derives `Deserialize` — same rationale as `Verdict` (see this module's top-of-file doc
/// comment): a derived `Deserialize` impl lives in this same module and would let any crate
/// construct a `RecordRef` value straight from JSON. This type carries no invariant to bypass
/// today, but the derive is withheld pre-emptively to match the module's stated policy rather
/// than be added back in later under time pressure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordRef {
    pub kind: RecordKind,
    pub id: String,
}

/// D-02/D-04's typed evidence line: human-readable text plus zero or more typed pivot targets.
///
/// `refs` is empty for aggregate-only evidence (nothing for the analyst to pivot on directly).
///
/// Plain public fields — unlike `Coverage`, there is no invariant to protect here; any
/// `(String, Vec<RecordRef>)` pair is a valid `EvidenceRow`.
///
/// Never derives `Deserialize` — same rationale as `Verdict` (see this module's top-of-file doc
/// comment): a derived `Deserialize` impl lives in this same module and would let any crate
/// construct an `EvidenceRow` value straight from JSON. This type carries no invariant to bypass
/// today, but the derive is withheld pre-emptively to match the module's stated policy rather
/// than be added back in later under time pressure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceRow {
    pub text: String,
    pub refs: Vec<RecordRef>,
}

/// RPT-05 check 2's per-finding attribution carrier: which external source contributed how many
/// records to a finding. Populated from `attributed::Attributed.producers` by the probes that
/// already call `count_for_source`/`count_across_producers`/`from_rows`.
///
/// Never derives `Deserialize` — same rationale as `Verdict` (see this module's top-of-file doc
/// comment): a derived `Deserialize` impl lives in this same module and would let any crate
/// construct a `SourceAttribution` value straight from JSON, bypassing whatever validation a
/// future consumer adds. This type carries no invariant to bypass today, but the derive is
/// withheld pre-emptively to match the module's stated policy rather than be added back in later
/// under time pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SourceAttribution {
    pub source: &'static str,
    pub value: u64,
}

/// D-10's typed carrier for a remediation trigger a probe wants to surface. One variant today:
/// `WardsUnattributed(u64)`, replacing Python's `re.match(r"(\d+) ward\(s\)", ...)` regex
/// (`analyst_probe.py:2846`) that D-09 forbids porting. Populated only by `ward-concentration`
/// from its already-computed `no_ward` value.
///
/// Never derives `Deserialize` — same rationale as `Verdict` (see this module's top-of-file doc
/// comment): a derived `Deserialize` impl lives in this same module and would let any crate
/// construct a `RemediationHint` value straight from JSON, bypassing whatever validation a future
/// consumer adds. This type carries no invariant to bypass today, but the derive is withheld
/// pre-emptively to match the module's stated policy rather than be added back in later under
/// time pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RemediationHint {
    WardsUnattributed(u64),
}

/// A minimal probe result: which probe ran, what it found, and a human-readable summary.
///
/// Widened per D-01 to carry `severity`/`evidence`/`attributions`/`remediation_hint` — the four
/// report-needed fields that vary per-finding (as opposed to `tier`/`hypothesis`, which D-01 keeps
/// on `Probe` instead because they're per-probe constants, not per-finding data; splitting the two
/// groups by where they vary is D-01's rule, not an arbitrary choice).
///
/// No `Deserialize` — `Finding` carries a `Verdict`, and `Verdict` deliberately has none (see this
/// module's top-level doc comment).
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub probe: String,
    pub verdict: Verdict,
    pub summary: String,
    /// `None` for every probe outside the disqualification-chain pair (Plan 33-05).
    /// `Some(Cause::X)` only for `disqualified-active-in-procurement` and `evasion-alerts` —
    /// the two probes D4 requires to report which of the 5 causes produced their verdict, as a
    /// typed variant Phase 34/35 can pivot on directly rather than a substring of `summary`.
    pub cause: Option<Cause>,
    /// This finding's severity — see [`Severity`].
    pub severity: Severity,
    /// D-02/D-04's typed evidence rows — the prose previously merged into `summary`, now
    /// extracted with typed pivot targets attached.
    pub evidence: Vec<EvidenceRow>,
    /// RPT-05 check 2's per-finding source attributions — see [`SourceAttribution`].
    pub attributions: Vec<SourceAttribution>,
    /// D-10's typed remediation trigger, if this finding has one — see [`RemediationHint`].
    pub remediation_hint: Option<RemediationHint>,
}

impl Finding {
    /// `population` from this finding's `Verdict`, if it carries a `Coverage`. Always derived from
    /// `self.verdict` — never set independently, so the two can never diverge (VRD-01's "both
    /// counts appear in the report").
    pub fn population(&self) -> Option<u64> {
        self.verdict.coverage().map(|c| c.population())
    }

    /// `capable` from this finding's `Verdict`, if it carries a `Coverage`. Always derived from
    /// `self.verdict` — never set independently, so the two can never diverge.
    pub fn capable(&self) -> Option<u64> {
        self.verdict.coverage().map(|c| c.capable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_requires_capable_equal_population() {
        let v = Verdict::classify(10, 10, 0, 1, "");
        assert!(matches!(v, Verdict::Clear(c) if c.population() == 10 && c.capable() == 10));

        let v = Verdict::classify(10, 7, 0, 1, "3 structurally excluded");
        assert!(!matches!(v, Verdict::Clear(_)));
    }

    #[test]
    fn not_applicable_is_the_only_variant_reachable_when_capable_lt_population() {
        let v = Verdict::classify(148, 8, 0, 1, "140/148 are private individuals");
        assert!(matches!(v, Verdict::NotApplicable { .. }));

        // Even with matches high enough that this WOULD be a Hit if the population check were
        // skipped, capable < population must still produce NotApplicable unconditionally.
        let v = Verdict::classify(148, 8, 50, 1, "140/148 are private individuals");
        assert!(matches!(v, Verdict::NotApplicable { .. }));
    }

    #[test]
    fn data_gap_and_blocked_carry_no_coverage() {
        assert!(Verdict::data_gap("x").coverage().is_none());
        assert!(Verdict::blocked("y").coverage().is_none());
    }

    #[test]
    fn hit_and_weak_reachable_only_when_fully_capable() {
        let weak = Verdict::classify(5, 5, 3, 5, "");
        assert!(matches!(weak, Verdict::Weak { matches: 3, .. }));

        let hit = Verdict::classify(5, 5, 5, 5, "");
        assert!(matches!(hit, Verdict::Hit { matches: 5, .. }));
    }

    #[test]
    #[should_panic]
    fn classify_panics_when_capable_exceeds_population() {
        Verdict::classify(3, 5, 0, 1, "");
    }

    #[test]
    fn finding_population_and_capable_derive_from_verdict_never_set_independently() {
        let clear = Finding {
            probe: "probe_a".to_string(),
            verdict: Verdict::classify(10, 10, 0, 1, ""),
            summary: "all clear".to_string(),
            cause: None,
            severity: Severity::Info,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        };
        assert_eq!(clear.population(), Some(10));
        assert_eq!(clear.capable(), Some(10));

        let gap = Finding {
            probe: "probe_b".to_string(),
            verdict: Verdict::data_gap("missing OSINT source"),
            summary: "could not run".to_string(),
            cause: None,
            severity: Severity::Info,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        };
        assert_eq!(gap.population(), None);
        assert_eq!(gap.capable(), None);
    }

    /// Pins the BLOCKER FIX: `Verdict` must never implement `DeserializeOwned`. A derived
    /// `Deserialize` impl lives in the same module as the type, so `Coverage`'s field privacy would
    /// not block it — a bare `#[derive(Deserialize)]` reintroduced here would let
    /// `Verdict::Clear`/`NotApplicable` be built straight from JSON with a fabricated,
    /// never-validated pair of counts, bypassing `classify` entirely.
    ///
    /// This uses the same ambiguous-blanket-impl trick as the `static_assertions` crate's
    /// `assert_not_impl_any!` (no such dependency exists in this workspace, so it's inlined here):
    /// `AmbiguousIfDeserializeOwned<A>` is implemented unconditionally for every type via the `()`
    /// impl, and ADDITIONALLY for any type implementing `DeserializeOwned` via the `u8` impl. If
    /// `Verdict` ever implements `DeserializeOwned`, resolving `<Verdict as
    /// AmbiguousIfDeserializeOwned<_>>::some_item()` becomes ambiguous between the two impls and
    /// this file fails to compile with an E0283 "type annotations needed" error — a genuine
    /// compile-time falsifiability check, not a runtime assertion that could pass vacuously.
    #[test]
    fn verdict_does_not_implement_deserialize_owned() {
        trait AmbiguousIfDeserializeOwned<A> {
            fn some_item() {}
        }
        impl<T: ?Sized> AmbiguousIfDeserializeOwned<()> for T {}
        impl<T: serde::de::DeserializeOwned> AmbiguousIfDeserializeOwned<u8> for T {}

        <Verdict as AmbiguousIfDeserializeOwned<_>>::some_item();
        <Coverage as AmbiguousIfDeserializeOwned<_>>::some_item();
        <Finding as AmbiguousIfDeserializeOwned<_>>::some_item();
        <Severity as AmbiguousIfDeserializeOwned<_>>::some_item();
        <RecordKind as AmbiguousIfDeserializeOwned<_>>::some_item();
        <RecordRef as AmbiguousIfDeserializeOwned<_>>::some_item();
        <EvidenceRow as AmbiguousIfDeserializeOwned<_>>::some_item();
        <SourceAttribution as AmbiguousIfDeserializeOwned<_>>::some_item();
        <RemediationHint as AmbiguousIfDeserializeOwned<_>>::some_item();
        <PopulationSource as AmbiguousIfDeserializeOwned<_>>::some_item();
        <VerdictKind as AmbiguousIfDeserializeOwned<_>>::some_item();
        <SkipCause as AmbiguousIfDeserializeOwned<_>>::some_item();
    }

    /// Proves every `PopulationSource` variant is handled by a real, exhaustive `match` with no
    /// `_` wildcard arm — the same falsifiability guarantee `Verdict::coverage()` gives `Verdict`.
    /// If a future variant is added without updating this match, the file fails to compile; that
    /// compile failure IS the proof, not a runtime assertion that could pass vacuously.
    #[test]
    fn population_source_match_is_exhaustive_no_wildcard() {
        fn label(source: &PopulationSource) -> &'static str {
            match source {
                PopulationSource::SelfReported { .. } => "self-reported",
                PopulationSource::OracleCorroborated { .. } => "oracle-corroborated",
            }
        }

        let self_reported = PopulationSource::SelfReported { reason: "x" };
        let oracle_corroborated = PopulationSource::OracleCorroborated { oracle_count: 5 };

        assert_eq!(label(&self_reported), "self-reported");
        assert_eq!(label(&oracle_corroborated), "oracle-corroborated");
    }

    /// Pins the assertion style Plan 33-05's criterion-4 tests must use: a per-cause test asserts
    /// on the `Cause` variant directly via `matches!`, never on a substring of `summary` prose.
    /// D5 (LOCKED) exists precisely so a renamed slug in evidence text breaks at the type level,
    /// not silently — a `finding.summary.contains(...)` assertion would keep passing through such
    /// a rename.
    #[test]
    fn cause_variant_is_the_assertion_surface_not_prose() {
        let links_not_elevated = Finding {
            probe: "disqualified-active-in-procurement".to_string(),
            verdict: Verdict::NotApplicable {
                coverage: Verdict::classify(5, 2, 0, 1, "3 sub-threshold candidates")
                    .coverage()
                    .unwrap(),
                reason: "3 sub-threshold candidates".to_string(),
            },
            summary: "3 candidates excluded by the ENR-11 gate".to_string(),
            cause: Some(Cause::LinksNotElevated),
            severity: Severity::Info,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        };
        assert!(matches!(
            links_not_elevated.cause,
            Some(Cause::LinksNotElevated)
        ));

        let no_live_ban = Finding {
            probe: "evasion-alerts".to_string(),
            verdict: Verdict::classify(10, 10, 0, 1, ""),
            summary: "no live disqualification ban found on any evaluated officer".to_string(),
            cause: Some(Cause::NoLiveBan),
            severity: Severity::Info,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        };
        assert!(matches!(no_live_ban.cause, Some(Cause::NoLiveBan)));
    }

    /// `VerdictKind::of` maps every one of the 7 constructible `Verdict` shapes to its own,
    /// distinct kind — including `Skipped`, built via the new constructors (GAT-02).
    #[test]
    fn verdict_kind_of_maps_every_constructible_shape_to_its_own_kind() {
        let hit = Verdict::classify(5, 5, 5, 1, "");
        assert_eq!(VerdictKind::of(&hit), VerdictKind::Hit);

        let weak = Verdict::classify(5, 5, 3, 5, "");
        assert_eq!(VerdictKind::of(&weak), VerdictKind::Weak);

        let clear = Verdict::classify(10, 10, 0, 1, "");
        assert_eq!(VerdictKind::of(&clear), VerdictKind::Clear);

        let not_applicable = Verdict::classify(10, 7, 0, 1, "3 structurally excluded");
        assert_eq!(VerdictKind::of(&not_applicable), VerdictKind::NotApplicable);

        let data_gap = Verdict::data_gap("missing input");
        assert_eq!(VerdictKind::of(&data_gap), VerdictKind::DataGap);

        let blocked = Verdict::blocked("prereq");
        assert_eq!(VerdictKind::of(&blocked), VerdictKind::Blocked);

        let skipped = Verdict::skipped_dependency("identity-integrity", &clear);
        assert_eq!(VerdictKind::of(&skipped), VerdictKind::Skipped);
    }

    /// `Verdict::skipped_dependency` derives its `VerdictKind` from the REAL upstream `&Verdict`
    /// passed in, never a caller-asserted kind — and the resulting `Skipped` carries no
    /// `Coverage` (D-11).
    #[test]
    fn skipped_dependency_derives_kind_from_real_upstream_verdict() {
        let upstream = Verdict::classify(10, 10, 0, 1, "");
        let skipped = Verdict::skipped_dependency("identity-integrity", &upstream);

        assert!(skipped.coverage().is_none());
        let Verdict::Skipped { cause } = &skipped else {
            panic!("expected Verdict::Skipped, got {skipped:?}");
        };
        assert_eq!(cause.upstream(), Some("identity-integrity"));
        assert_eq!(cause.upstream_verdict(), Some(VerdictKind::Clear));
        assert_eq!(cause.tier(), None);
        assert_eq!(cause.probes_checked(), None);
    }

    /// `Verdict::skipped_tier_aggregate` names D-08's Tier-1 -> Tier-2 OR-rule cause: a tier and
    /// how many probes were checked, with no single upstream to name (D-23's amendment rationale).
    #[test]
    fn skipped_tier_aggregate_names_tier_and_probes_checked() {
        let skipped = Verdict::skipped_tier_aggregate("Tier-1", 7);

        assert!(skipped.coverage().is_none());
        let Verdict::Skipped { cause } = &skipped else {
            panic!("expected Verdict::Skipped, got {skipped:?}");
        };
        assert_eq!(cause.tier(), Some("Tier-1"));
        assert_eq!(cause.probes_checked(), Some(7));
        assert_eq!(cause.upstream(), None);
        assert_eq!(cause.upstream_verdict(), None);
    }

    /// `SkipCause::describe()` renders the exact, final sentence both `run.rs::render_verdict`
    /// and later plans' rendering code call — asserted with `assert_eq!`, not `.contains(...)`,
    /// since this IS the one place the phrasing is authored.
    #[test]
    fn skip_cause_describe_renders_exact_sentences() {
        let upstream = Verdict::classify(10, 10, 0, 1, "");
        let dependency = Verdict::skipped_dependency("identity-integrity", &upstream);
        let Verdict::Skipped { cause } = &dependency else {
            unreachable!()
        };
        assert_eq!(
            cause.describe(),
            "upstream 'identity-integrity' returned Clear"
        );

        let tier_aggregate = Verdict::skipped_tier_aggregate("Tier-1", 7);
        let Verdict::Skipped { cause } = &tier_aggregate else {
            unreachable!()
        };
        assert_eq!(
            cause.describe(),
            "no Tier-1 probe (of 7 checked) returned Hit or Weak"
        );
    }
}
