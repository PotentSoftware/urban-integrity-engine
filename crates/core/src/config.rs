use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};
use urban_integrity_schema::nation::Nation;

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Selected analysis id (registry `db_name`). `None`/`main` → default database.
    /// Set per-invocation via the global `--analysis` flag (bridged to
    /// `URBAN__ANALYSIS`) or the env var directly. Resolved in `connect_db`.
    #[serde(default)]
    pub analysis: Option<String>,
    pub surreal: SurrealConfig,
    pub graph: GraphConfig,
    pub ingest: IngestConfig,
    pub detector: DetectorConfigRaw,
    #[serde(default)]
    pub enricher: EnricherConfig,
    /// Which UK nation this analysis covers. Derived from the configured ward codes;
    /// this section only exists to override or relax that derivation.
    #[serde(default)]
    pub jurisdiction: JurisdictionConfig,
    /// The `urban-integrity-cli workbench` HTTP server bind address (D-05, Phase 42).
    /// Absent from config.toml → defaults to `WorkbenchConfig::default()` (127.0.0.1:8090)
    /// so every existing config.toml without a `[workbench]` section keeps loading.
    #[serde(default)]
    pub workbench: WorkbenchConfig,
}

/// `[workbench]` — bind address for the `urban-integrity-cli workbench` HTTP server (D-05).
///
/// Operator-controlled only: config file, `URBAN__WORKBENCH__BIND_ADDR` env var, or a
/// `--bind` CLI flag (added in a later Phase 42 plan) — no client-supplied input reaches
/// this field. A taken port is a fail-loud, named-cause error; the workbench never
/// auto-picks a different one.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchConfig {
    /// Local bind address for the workbench HTTP server, e.g. "127.0.0.1:8090".
    pub bind_addr: String,
}

impl Default for WorkbenchConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8090".to_string(),
        }
    }
}

/// `[jurisdiction]` — nation scope of the analysis.
///
/// Normally empty: the nation is derived from `ingest.planning_portals[].ward_code`,
/// whose ONS GSS prefix (`E05` / `W05` / `S13` / `N08`) already names it. Both keys here
/// exist for the two cases derivation cannot serve.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct JurisdictionConfig {
    /// Explicit nation for an analysis whose scope is not expressed as wards
    /// (`"england"` | `"wales"` | `"scotland"` | `"northern ireland"`). If wards ARE
    /// configured and disagree with this, the config is refused rather than silently
    /// resolved either way.
    #[serde(default)]
    pub nation: Option<String>,
    /// Permit wards from more than one nation in a single analysis.
    ///
    /// Off by default because no deprivation index spans two nations, and the
    /// England-and-Wales registries (HMLR CCOD/OCOD/PPD, Charity Commission) would then
    /// be correct for only part of the corpus. Setting this true keeps the analysis
    /// nation unknown, which admits every adapter — the operator is taking on the
    /// interpretation themselves.
    #[serde(default)]
    pub allow_mixed_nations: bool,
}

/// Why an analysis's nation could not be established.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum JurisdictionError {
    #[error(
        "planning_portals span more than one UK nation ({}) — no deprivation index covers \
         two nations and HM Land Registry covers England and Wales only. Split the analysis, \
         or set [jurisdiction] allow_mixed_nations = true to take that on deliberately.",
        .0.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" and ")
    )]
    MixedNations(Vec<Nation>),
    #[error(
        "planning_portals ward_code '{0}' is not an ONS GSS code (one nation letter — E, W, S \
         or N — then eight digits) — the analysis nation cannot be derived from it"
    )]
    UnrecognisedWardCode(String),
    #[error(
        "[jurisdiction] nation = '{0}' is not one of: england, wales, scotland, northern ireland"
    )]
    UnrecognisedNation(String),
    #[error(
        "[jurisdiction] nation = '{declared}' contradicts the configured wards, which are in \
         {derived}. One of the two is wrong; fix it rather than letting the engine pick."
    )]
    OverrideDisagreesWithWards { declared: Nation, derived: Nation },
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct SurrealConfig {
    pub ws_url: String,
    pub namespace: String,
    pub database: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Graph engine runtime configuration.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    /// Number of events before triggering an automatic checkpoint
    pub checkpoint_events_n: u64,
    /// Seconds between automatic checkpoints (whichever fires first)
    pub checkpoint_interval_secs: u64,
    /// Directory to write checkpoint files
    pub checkpoint_path: String,
    /// WAL crossbeam-channel capacity
    pub wal_channel_capacity: usize,
    /// SurrealDB WAL write batch size
    pub wal_batch_size: usize,
    /// SurrealDB WAL batch window in milliseconds
    pub wal_batch_window_ms: u64,
    /// DashMap shard count
    pub shard_count: usize,
    /// Snapshot every N mutations
    pub snapshot_every_n_events: u64,
    /// Snapshot interval in seconds
    pub snapshot_interval_secs: u64,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            checkpoint_events_n: 1_000,
            checkpoint_interval_secs: 60,
            checkpoint_path: "data/checkpoints".to_string(),
            wal_channel_capacity: 10_000,
            wal_batch_size: 100,
            wal_batch_window_ms: 100,
            shard_count: 64,
            snapshot_every_n_events: 1_000,
            snapshot_interval_secs: 30,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DetectorConfigRaw {
    pub phoenix_window_days: Option<i64>,
    pub phoenix_min_value: Option<f64>,
    pub imd_csv_path: Option<String>,
}

impl Default for DetectorConfigRaw {
    fn default() -> Self {
        Self {
            phoenix_window_days: Some(180),
            phoenix_min_value: Some(10000.0),
            imd_csv_path: None,
        }
    }
}

/// Configuration for a single planning portal entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningPortalConfig {
    /// ONS Local Authority code for the LPA (e.g. "X06999001", the fictional example LPA).
    /// Used for entity resolution across LPAs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lpa_code: Option<String>,
    /// ONS GSS ward code (e.g. "X05999001", the fictional example ward).
    pub ward_code: String,
    /// Portal system type — drives sidecar scraping strategy.
    /// Values: "idox" | "arcus" | "wfs" | "planapi"
    /// Defaults to "idox" (covers ~69% of UK LPAs).
    #[serde(default = "default_portal_type")]
    pub portal_type: String,
    /// URL to scrape for this ward's planning applications.
    pub url: String,
}

fn default_portal_type() -> String {
    "idox".to_string()
}

fn default_planning_poll_interval() -> u64 {
    3600
}

/// Default HTTP client timeout for a single sidecar `/scrape` call.
///
/// A full ward re-list is one request that scrapes every application in the ward, so the
/// ceiling has to clear the largest ward, not the average one. Measured live against the
/// maintainers' reference Arcus portal (2026-07-27): its two largest wards returned 4,986 rows
/// in 157.6s and 3,996 rows in 132.3s. The previous hardcoded 180s left ~13% headroom over the worst case and a ward
/// duly timed out under concurrent load during a backfill run. 300s restores real headroom.
fn default_scrapling_timeout_secs() -> u64 {
    300
}

/// Ingestion pipeline configuration.
#[derive(Deserialize, Debug, Clone)]
pub struct IngestConfig {
    /// Companies House streaming API key (set via URBAN__INGEST__CH_API_KEY env var)
    pub ch_api_key: Option<String>,
    /// Firecrawl API key — used by the Scrapling sidecar as a fallback when
    /// scrapling is blocked. Set via URBAN__INGEST__FIRECRAWL_API_KEY env var.
    #[serde(default)]
    pub firecrawl_api_key: Option<String>,
    /// Contracts Finder polling interval in seconds (default: 900 = 15 min)
    pub cf_poll_interval_secs: u64,
    /// Scrapling sidecar base URL (no trailing slash)
    pub scrapling_url: String,
    /// Scrapling sidecar port
    pub scrapling_port: u16,
    /// HTTP client timeout, in seconds, for a single sidecar `/scrape` call
    /// (default: 300). Must clear the slowest single ward re-list — see
    /// `default_scrapling_timeout_secs`.
    #[serde(default = "default_scrapling_timeout_secs")]
    pub scrapling_timeout_secs: u64,
    /// Planning portal entries — one per ward (ward_code + URL)
    #[serde(default)]
    pub planning_portals: Vec<PlanningPortalConfig>,
    /// Planning portal poll interval in seconds (default: 3600 = 1 hour)
    #[serde(default = "default_planning_poll_interval")]
    pub planning_poll_interval_secs: u64,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            ch_api_key: None,
            firecrawl_api_key: None,
            cf_poll_interval_secs: 900,
            scrapling_url: "http://localhost".to_string(),
            scrapling_port: 8765,
            scrapling_timeout_secs: default_scrapling_timeout_secs(),
            planning_portals: vec![],
            planning_poll_interval_secs: 3600,
        }
    }
}

/// Companies House enricher configuration — controls the PSC and Disqualified
/// Directors stream adapters (Phase 17, CHA-03).
///
/// Absent or empty `api_key` means the adapters are disabled (`is_enabled() == false`).
/// Does NOT have `#[serde(deny_unknown_fields)]` so future fields can be added without
/// breaking existing config.toml files.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ChEnricherConfig {
    /// CH Streaming API key for the enricher adapters.
    /// Set via `URBAN__ENRICHER__CH__API_KEY` env var.
    /// An absent or empty value means the adapters are skipped (graceful degradation).
    #[serde(default)]
    pub api_key: Option<String>,
}

impl ChEnricherConfig {
    /// Returns `true` when a non-empty API key is present — the CHA-03
    /// graceful-degradation gate used by both CH enricher adapters.
    ///
    /// Treats a whitespace-only key as absent (mirrors the `main.rs:165`
    /// CH ingest key handling pattern).
    pub fn is_enabled(&self) -> bool {
        self.api_key
            .as_deref()
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    }
}

/// HM Land Registry "Use Land and Property Data" enricher configuration (Phase 18, PROP-01).
///
/// Controls the CCOD bulk-CSV adapter. Absent or empty `api_key` means the adapter
/// is disabled (`is_enabled() == false`) — graceful degradation per D-03.
/// Does NOT have `#[serde(deny_unknown_fields)]` — same pattern as `ChEnricherConfig`.
#[derive(Deserialize, Debug, Clone)]
pub struct LandRegistryConfig {
    /// HMLR "Use Land and Property Data" API key.
    /// Set via `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` env var.
    /// SECURITY: Never log this value — log only `api_key_present = true/false` (T-18-01-01).
    /// An absent or empty value disables the CCOD adapter gracefully.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Local directory for downloaded CCOD ZIPs (D-02).
    /// Default: `data/ccod` (relative to working directory).
    #[serde(default)]
    pub cache_dir: Option<String>,

    /// Days between CCOD re-downloads — matches the monthly HMLR publication cadence (D-01).
    /// Default: 30 days.
    #[serde(default = "default_ccod_refresh_interval_days")]
    pub ccod_refresh_interval_days: u32,
}

fn default_ccod_refresh_interval_days() -> u32 {
    30
}

impl Default for LandRegistryConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            cache_dir: None,
            ccod_refresh_interval_days: default_ccod_refresh_interval_days(),
        }
    }
}

impl LandRegistryConfig {
    /// Returns `true` when a non-empty API key is present — the D-03 graceful-degradation
    /// gate for the CCOD adapter. Treats a whitespace-only key as absent.
    pub fn is_enabled(&self) -> bool {
        self.api_key
            .as_deref()
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    }
}

/// HM Land Registry OCOD (Overseas Companies that Own Property) adapter
/// configuration (Phase 22, PROP-05/PROP-06, OPS-01).
///
/// Mirrors `LandRegistryConfig` (the CCOD adapter). Disabled-by-default: an
/// absent or empty `api_key` means `is_enabled() == false`, so the OCOD adapter
/// degrades gracefully until an HMLR licence is signed (OPS-01 gate).
///
/// RESEARCH Q3: one HMLR account licenses BOTH ccod + ocod, so `api_key` may be
/// left blank here to inherit the shared `[enricher.land_registry]` key — the
/// OCOD adapter (Plan 03) falls back to the `land_registry` key when
/// `ocod.api_key` is empty.
/// Does NOT have `#[serde(deny_unknown_fields)]` — same pattern as `LandRegistryConfig`.
#[derive(Deserialize, Debug, Clone)]
pub struct OcodConfig {
    /// HMLR "Use Land and Property Data" API key for the OCOD dataset.
    /// Set via `URBAN__ENRICHER__OCOD__API_KEY`, or leave blank to inherit the
    /// shared `[enricher.land_registry]` key (one HMLR account licenses both).
    /// SECURITY: Never log this value — log only `api_key_present = true/false`.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Local directory for downloaded OCOD ZIPs. Default: `data/ocod`.
    #[serde(default)]
    pub cache_dir: Option<String>,

    /// Days between OCOD re-downloads — matches the monthly HMLR publication cadence.
    /// Default: 30 days.
    #[serde(default = "default_ocod_refresh_interval_days")]
    pub ocod_refresh_interval_days: u32,
}

fn default_ocod_refresh_interval_days() -> u32 {
    30
}

impl Default for OcodConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            cache_dir: None,
            ocod_refresh_interval_days: default_ocod_refresh_interval_days(),
        }
    }
}

impl OcodConfig {
    /// Returns `true` when this config's own `api_key` is non-empty.
    ///
    /// NOTE: this does NOT account for the shared `land_registry` fallback —
    /// the OCOD adapter (Plan 03) resolves the effective key (own key, else the
    /// `land_registry` key) before deciding whether to run. Treats a
    /// whitespace-only key as absent.
    pub fn is_enabled(&self) -> bool {
        self.api_key
            .as_deref()
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    }
}

/// HM Land Registry Price Paid Data (PPD) adapter configuration (Phase 23, FLIP-01).
///
/// PPD is OGL v3 open data — no API key required. Disabled by default: set
/// `enabled = true` to activate. The 5.1 GB national `pp-complete.txt` is never
/// targeted; only yearly files (`pp-{year}.txt`) for the last `years_back` years
/// are downloaded. An LPA-postcode allowlist (D-04) filters rows at load time.
///
/// Does NOT have `#[serde(deny_unknown_fields)]` — mirrors `OcodConfig` pattern.
#[derive(Deserialize, Debug, Clone)]
pub struct MutualsConfig {
    /// Enable the FCA Mutuals Public Register adapter. Default: `true`.
    ///
    /// Unlike most adapters this needs no credential and hits no rate limit — the whole
    /// register is one public CSV — so there is nothing to opt into and a real cost to
    /// leaving it off: registered societies are otherwise INVISIBLE to this engine. They are
    /// not companies, hold no Companies House number, and (having charitable rules) are
    /// exempt charities absent from the Charity Commission register too. Housing
    /// associations are among the largest recipients of public land and money in Wales;
    /// ClwydAlyn alone holds 52 planning applications in the maintainers' reference analysis
    /// and reached the graph as a bare name (planning open item 24).
    #[serde(default = "default_mutuals_enabled")]
    pub enabled: bool,

    /// Bulk society list. Published by the FCA on Azure blob storage, unauthenticated.
    #[serde(default = "default_mutuals_url")]
    pub url: String,

    /// Local cache directory for the downloaded CSV. Default: `data/mutuals`.
    #[serde(default)]
    pub cache_dir: Option<String>,
}

fn default_mutuals_enabled() -> bool {
    true
}

fn default_mutuals_url() -> String {
    "https://fcastoragemprprod.blob.core.windows.net/societylist/SocietyList.csv".to_string()
}

impl Default for MutualsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            url: default_mutuals_url(),
            cache_dir: None,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PpdConfig {
    /// Enable the PPD adapter. Default: `false` — explicit opt-in required.
    #[serde(default)]
    pub enabled: bool,

    /// Number of past years to download (default 5). Each yearly file is ~800 MB;
    /// only in-scope LPA rows are retained after the district filter.
    #[serde(default = "default_ppd_years_back")]
    pub years_back: u32,

    /// Optional allowlist of postcode districts (e.g. `["XX1", "XX2"]`).
    /// When present, overrides the dynamic derivation from Location-node postcodes
    /// (D-04 fallback). Absent → derive from graph.
    #[serde(default)]
    pub postcode_districts: Option<Vec<String>>,

    /// Local directory for cached PPD text files. Default: `data/ppd`.
    #[serde(default)]
    pub cache_dir: Option<String>,
}

fn default_ppd_years_back() -> u32 {
    5
}

impl Default for PpdConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            years_back: default_ppd_years_back(),
            postcode_districts: None,
            cache_dir: None,
        }
    }
}

impl PpdConfig {
    /// Returns `true` when `enabled = true` in config.
    /// PPD is open data so no API-key check is needed.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// OpenSanctions yente sidecar configuration (Phase 19, FUZ-03).
///
/// D-06: fail-hard if `non_commercial_confirmed` is absent — CC-BY-NC compliance.
/// The `is_enabled()` check in the adapter enforces this gate.
/// Does NOT have `#[serde(deny_unknown_fields)]` — follows `LandRegistryConfig` pattern.
#[derive(Deserialize, Debug, Clone)]
pub struct OpenSanctionsConfig {
    /// Whether the OpenSanctions adapter is enabled.
    /// Default: `false` — requires explicit opt-in AND `non_commercial_confirmed = true`.
    #[serde(default)]
    pub enabled: bool,

    /// Yente sidecar endpoint (D-05). Default: `http://localhost:8766`.
    #[serde(default = "default_opensanctions_endpoint")]
    pub endpoint: String,

    /// Must be explicitly set to `true` — confirms CC-BY-NC non-commercial use compliance (D-06).
    /// Absent → `false` → adapter `is_enabled()` returns `false` with compliance error.
    #[serde(default)]
    pub non_commercial_confirmed: bool,

    /// Batch size for yente `/match` requests (D-07). Default: 50.
    #[serde(default = "default_opensanctions_batch_size")]
    pub batch_size: usize,
}

fn default_opensanctions_endpoint() -> String {
    "http://localhost:8766".to_string()
}

fn default_opensanctions_batch_size() -> usize {
    50
}

impl Default for OpenSanctionsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_opensanctions_endpoint(),
            non_commercial_confirmed: false,
            batch_size: default_opensanctions_batch_size(),
        }
    }
}

/// Charity Commission bulk-download adapter configuration (Phase 19, FUZ-02).
///
/// OGL v3 open data — no API key required. Enabled by default.
/// Does NOT have `#[serde(deny_unknown_fields)]`.
#[derive(Deserialize, Debug, Clone)]
pub struct CharityCommissionConfig {
    /// Whether the Charity Commission adapter is enabled.
    /// Default: `true` — OGL v3 open data, no key required.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Local directory for downloaded Charity Commission ZIPs.
    /// Default: `None` (uses a system temp directory).
    #[serde(default)]
    pub cache_dir: Option<String>,

    /// Days between Charity Commission re-downloads. Default: 30.
    #[serde(default = "default_cc_refresh_interval_days")]
    pub refresh_interval_days: u64,
}

fn default_true() -> bool {
    true
}

fn default_cc_refresh_interval_days() -> u64 {
    30
}

impl Default for CharityCommissionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_dir: None,
            refresh_interval_days: default_cc_refresh_interval_days(),
        }
    }
}

/// ICIJ Offshore Leaks bulk CSV adapter configuration (Phase 19, FUZ-04).
///
/// ODbL open data — no API key required. Enabled by default.
/// Does NOT have `#[serde(deny_unknown_fields)]`.
#[derive(Deserialize, Debug, Clone)]
pub struct IcijConfig {
    /// Whether the ICIJ adapter is enabled.
    /// Default: `true` — ODbL open data, no key required.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Local directory for downloaded ICIJ ZIPs.
    /// Default: `None` (uses a system temp directory).
    #[serde(default)]
    pub cache_dir: Option<String>,
}

impl Default for IcijConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_dir: None,
        }
    }
}

/// Electoral Commission donations register adapter configuration (Phase 20, POL-01).
///
/// OGL v3 open data — no API key required. Enabled by default.
/// Does NOT have `#[serde(deny_unknown_fields)]` — follows `IcijConfig` pattern.
#[derive(Deserialize, Debug, Clone)]
pub struct ElectoralCommissionConfig {
    /// Whether the Electoral Commission donations adapter is enabled.
    /// Default: `true` — OGL v3 open data, no key required.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Local directory for cached Electoral Commission CSV downloads.
    /// Default: `None` (uses a system temp directory).
    #[serde(default)]
    pub cache_dir: Option<String>,
}

impl Default for ElectoralCommissionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_dir: None,
        }
    }
}

/// Council Registers of Interest adapter configuration (Phase 20, POL-02).
///
/// Scrapling sidecar required. Disabled by default until `lpa_codes` is configured.
/// Does NOT have `#[serde(deny_unknown_fields)]` — follows `OpenSanctionsConfig` pattern.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct CouncilRegistersConfig {
    /// Whether the Council Registers adapter is enabled.
    /// Default: `false` — requires `lpa_codes` to be configured.
    #[serde(default)]
    pub enabled: bool,

    /// Target LPA codes to scrape for register-of-interest declarations.
    /// Default: empty (adapter disabled until populated).
    #[serde(default)]
    pub lpa_codes: Vec<String>,
}

/// OpenCorporates cross-jurisdiction company lookup adapter configuration (Phase 20, POL-03).
///
/// ODbL licence gate + SurrealDB monthly quota counter. Disabled by default.
/// D-06 analogue: `oc_open_data_confirmed` must be `true` before adapter activates.
/// Does NOT have `#[serde(deny_unknown_fields)]` — follows `OpenSanctionsConfig` pattern.
#[derive(Deserialize, Debug, Clone)]
pub struct OpenCorporatesConfig {
    /// Whether the OpenCorporates adapter is enabled.
    /// Default: `false` — requires explicit opt-in AND `oc_open_data_confirmed = true`.
    #[serde(default)]
    pub enabled: bool,

    /// OpenCorporates API key (optional — free tier works without a key for some endpoints).
    /// Set via `URBAN__ENRICHER__OPENCORPORATES__API_KEY` env var.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Must be explicitly set to `true` to confirm ODbL licence terms (mirrors
    /// OpenSanctions `non_commercial_confirmed` gate, per D-06 from Phase 19).
    /// Absent → `false` → adapter returns an error with licence guidance.
    #[serde(default)]
    pub oc_open_data_confirmed: bool,

    /// Monthly request limit for the free tier (200) or Essentials tier (500).
    /// Default: 200 (true free tier per api.opencorporates.com docs — Pitfall 4).
    /// Set to 500 in config.toml if on the Essentials paid tier.
    #[serde(default = "default_oc_monthly_limit")]
    pub monthly_limit: u32,
}

fn default_oc_monthly_limit() -> u32 {
    200
}

impl Default for OpenCorporatesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: None,
            oc_open_data_confirmed: false,
            monthly_limit: default_oc_monthly_limit(),
        }
    }
}

/// London Gazette insolvency adapter configuration (PROP-02).
///
/// Public REST API — no API key required. Always enabled.
/// Does NOT have `#[serde(deny_unknown_fields)]`.
#[derive(Deserialize, Debug, Clone)]
pub struct LondonGazetteConfig {
    /// Wall-clock budget for a single enrichment pass in seconds.
    /// Default: 300 — roughly one ~75-fetch rate-limit batch.
    /// Override via URBAN__ENRICHER__LONDON_GAZETTE__MAX_PASS_SECS env var.
    #[serde(default = "default_gazette_max_pass_secs")]
    pub max_pass_secs: u64,

    /// Maximum days to look back when the cursor is empty or older than this window.
    /// Default: 30. Override via URBAN__ENRICHER__LONDON_GAZETTE__LOOKBACK_DAYS env var.
    #[serde(default = "default_gazette_lookback_days")]
    pub lookback_days: i64,
}

fn default_gazette_max_pass_secs() -> u64 {
    300
}
fn default_gazette_lookback_days() -> i64 {
    30
}

impl Default for LondonGazetteConfig {
    fn default() -> Self {
        Self {
            max_pass_secs: default_gazette_max_pass_secs(),
            lookback_days: default_gazette_lookback_days(),
        }
    }
}

/// Enricher crate configuration — controls batch OSINT enrichment (Phase 16+).
/// Does NOT have #[serde(deny_unknown_fields)] — follows IngestConfig pattern.
#[derive(Deserialize, Debug, Clone)]
pub struct EnricherConfig {
    /// Interval between enrichment runs in seconds (default: 86400 = 24 hours).
    /// Aligns with the daily cadence of Companies House bulk data exports.
    /// Override via URBAN__ENRICHER__ENRICH_INTERVAL_SECS env var.
    #[serde(default = "default_enrich_interval_secs")]
    pub enrich_interval_secs: u64,

    /// Companies House enricher adapter configuration (Phase 17, CHA-03).
    /// Absent from config.toml → defaults to `ChEnricherConfig::default()` → `is_enabled() == false`.
    #[serde(default)]
    pub ch: ChEnricherConfig,

    /// HM Land Registry CCOD adapter configuration (Phase 18, PROP-01, D-01/D-02/D-03).
    /// Absent from config.toml → defaults to `LandRegistryConfig::default()` → `is_enabled() == false`.
    #[serde(default)]
    pub land_registry: LandRegistryConfig,

    /// HM Land Registry OCOD overseas-ownership adapter configuration (Phase 22, PROP-05/06).
    /// Absent from config.toml → defaults to `OcodConfig::default()` → `is_enabled() == false`.
    #[serde(default)]
    pub ocod: OcodConfig,

    /// OpenSanctions yente sidecar configuration (Phase 19, FUZ-03).
    /// Absent from config.toml → defaults to `OpenSanctionsConfig::default()` → disabled.
    #[serde(default)]
    pub opensanctions: OpenSanctionsConfig,

    /// Charity Commission bulk-download adapter configuration (Phase 19, FUZ-02).
    /// Absent from config.toml → defaults to `CharityCommissionConfig::default()` → enabled.
    #[serde(default)]
    pub charity_commission: CharityCommissionConfig,

    /// ICIJ Offshore Leaks adapter configuration (Phase 19, FUZ-04).
    /// Absent from config.toml → defaults to `IcijConfig::default()` → enabled.
    #[serde(default)]
    pub icij: IcijConfig,

    /// Electoral Commission donations register adapter configuration (Phase 20, POL-01).
    /// Absent from config.toml → defaults to `ElectoralCommissionConfig::default()` → enabled.
    #[serde(default)]
    pub electoral_commission: ElectoralCommissionConfig,

    /// Council Registers of Interest adapter configuration (Phase 20, POL-02).
    /// Absent from config.toml → defaults to `CouncilRegistersConfig::default()` → disabled.
    #[serde(default)]
    pub council_registers: CouncilRegistersConfig,

    /// OpenCorporates cross-jurisdiction adapter configuration (Phase 20, POL-03).
    /// Absent from config.toml → defaults to `OpenCorporatesConfig::default()` → disabled.
    #[serde(default)]
    pub opencorporates: OpenCorporatesConfig,

    /// London Gazette insolvency adapter configuration (PROP-02).
    /// Absent from config.toml → defaults to `LondonGazetteConfig::default()` → enabled with
    /// 300 s budget and 30-day lookback.
    #[serde(default)]
    pub london_gazette: LondonGazetteConfig,

    /// HM Land Registry Price Paid Data (PPD) adapter configuration (Phase 23, FLIP-01).
    /// Absent from config.toml → defaults to `PpdConfig::default()` → disabled.
    /// PPD is OGL v3 open data — no API key required; enabled = true activates the adapter.
    #[serde(default)]
    pub ppd: PpdConfig,

    /// FCA Mutuals Public Register adapter configuration (Open Item 24).
    /// Absent from config.toml → defaults to `MutualsConfig::default()` → enabled.
    /// The register is a public bulk CSV — no API key, no scraping.
    #[serde(default)]
    pub mutuals: MutualsConfig,
}

fn default_enrich_interval_secs() -> u64 {
    86400
}

impl Default for EnricherConfig {
    fn default() -> Self {
        Self {
            enrich_interval_secs: default_enrich_interval_secs(),
            ch: ChEnricherConfig::default(),
            land_registry: LandRegistryConfig::default(),
            ocod: OcodConfig::default(),
            opensanctions: OpenSanctionsConfig::default(),
            charity_commission: CharityCommissionConfig::default(),
            icij: IcijConfig::default(),
            electoral_commission: ElectoralCommissionConfig::default(),
            council_registers: CouncilRegistersConfig::default(),
            opencorporates: OpenCorporatesConfig::default(),
            london_gazette: LondonGazetteConfig::default(),
            ppd: PpdConfig::default(),
            mutuals: MutualsConfig::default(),
        }
    }
}

/// The committed, fictional example configuration.
pub const EXAMPLE_CONFIG_FILE: &str = "config.toml";
/// The operator's own, gitignored configuration for a real analysis.
pub const LOCAL_CONFIG_FILE: &str = "config.local.toml";

impl AppConfig {
    /// Load the configuration from the current working directory.
    ///
    /// Two layers, lowest to highest precedence:
    ///
    /// 1. **One file** — `config.local.toml` if it exists, otherwise `config.toml`. The local
    ///    file *replaces* the committed example; the two are never merged. See
    ///    [`AppConfig::config_file_in`].
    /// 2. **`URBAN__*` environment variables** (`__` separates nesting levels, e.g.
    ///    `URBAN__SURREAL__DATABASE`), after `.env` has been read into the environment.
    ///
    /// Why replace rather than overlay: a merge would let any key the operator's file omits
    /// fall through to the fictional example, so a real analysis could silently inherit an
    /// example portal, postcode district or database name. `config.local.toml` is therefore
    /// a complete config — start it as a copy of `config.toml`.
    pub fn load() -> Result<Self, ConfigError> {
        dotenv::dotenv().ok();
        Self::load_file(&Self::config_file_in(std::path::Path::new(".")))
    }

    /// The config file [`AppConfig::load`] reads when run from `dir`: `dir/config.local.toml`
    /// when it exists, otherwise `dir/config.toml`.
    pub fn config_file_in(dir: &std::path::Path) -> std::path::PathBuf {
        let local = dir.join(LOCAL_CONFIG_FILE);
        if local.is_file() {
            local
        } else {
            dir.join(EXAMPLE_CONFIG_FILE)
        }
    }

    /// Load exactly `path` (required) with the `URBAN__*` environment layered on top.
    /// Does not read `.env` and does not consider any other file.
    pub fn load_file(path: &std::path::Path) -> Result<Self, ConfigError> {
        Config::builder()
            .add_source(File::from(path).required(true))
            .add_source(Environment::with_prefix("URBAN").separator("__"))
            .build()?
            .try_deserialize()
    }
}

impl AppConfig {
    /// The single UK nation this analysis covers, derived from the configured ward codes.
    ///
    /// `Ok(None)` means there is no nation to gate on — no wards are configured, or the
    /// operator deliberately allowed a mixed-nation analysis. Callers treat that as
    /// "admit every source" and say so; see `crates/enricher/src/engine.rs`.
    ///
    /// This is the input to every jurisdiction check: which deprivation index is valid,
    /// and which OSINT sources have any data for this analysis at all
    /// (see the jurisdiction-gate spec).
    pub fn analysis_nation(&self) -> Result<Option<Nation>, JurisdictionError> {
        let declared = match self.jurisdiction.nation.as_deref() {
            Some(name) => Some(
                Nation::from_name(name)
                    .ok_or_else(|| JurisdictionError::UnrecognisedNation(name.to_string()))?,
            ),
            None => None,
        };

        let mut derived: Vec<Nation> = Vec::new();
        for portal in &self.ingest.planning_portals {
            let nation = Nation::from_gss_code(&portal.ward_code)
                .ok_or_else(|| JurisdictionError::UnrecognisedWardCode(portal.ward_code.clone()))?;
            if !derived.contains(&nation) {
                derived.push(nation);
            }
        }
        derived.sort();

        match (declared, derived.len()) {
            (Some(d), 0) => Ok(Some(d)),
            (Some(d), 1) if d == derived[0] => Ok(Some(d)),
            (Some(d), 1) => Err(JurisdictionError::OverrideDisagreesWithWards {
                declared: d,
                derived: derived[0],
            }),
            // A declared nation does not license a mixed set of wards: the mix is the
            // problem, and picking one of them would hide it.
            (_, 0) => Ok(None),
            (_, 1) => Ok(Some(derived[0])),
            _ if self.jurisdiction.allow_mixed_nations => Ok(None),
            _ => Err(JurisdictionError::MixedNations(derived)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workspace root, computed absolutely from this crate's own manifest dir.
    ///
    /// Any test that needs `config.toml` on disk must `set_current_dir` to THIS, never a
    /// relative `"../../"`. Rust test binaries run tests concurrently in one process with
    /// one shared cwd; a relative chdir is only correct if it is the very first cwd change
    /// in the process, which cannot be guaranteed once more than one test in this module
    /// needs the workspace root (a second test's `"../../"`, run after the first already
    /// landed at the workspace root, walks two levels ABOVE it and fails to find
    /// config.toml — observed intermittently once `test_workbench_bind_addr_env_override`
    /// was added alongside the pre-existing `test_load_default_config`).
    fn workspace_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("crates/core/../.. should resolve to the workspace root")
            .to_path_buf()
    }

    /// The committed example must load and must pass the jurisdiction preflight's nation
    /// derivation, since a fresh clone runs every CLI command against it.
    ///
    /// Reads `config.toml` by path, NOT through `AppConfig::load()`: on a maintainer's
    /// machine `load()` picks up the gitignored `config.local.toml` instead, and this test
    /// is about the file that ships.
    #[test]
    fn test_committed_example_config_loads() {
        let config = AppConfig::load_file(&workspace_root().join(EXAMPLE_CONFIG_FILE))
            .expect("committed config.toml must load");
        assert_eq!(config.surreal.database, "an_example_00000000");
        assert!(
            config.ingest.planning_portals.is_empty(),
            "the example's portals are commented out; active fictional X05 codes would be \
             refused by the preflight as unrecognised"
        );
        assert_eq!(config.analysis_nation().unwrap(), None);
        assert!(!config.enricher.ppd.enabled);
    }

    /// Published examples must not point at real places. Scans the committed file's raw
    /// text — comments included — for anything shaped like a real ONS GSS code
    /// (E/W/S/N + 8 digits) or the maintainer's own analysis.
    #[test]
    fn test_committed_example_config_names_no_real_place() {
        let text = std::fs::read_to_string(workspace_root().join(EXAMPLE_CONFIG_FILE))
            .expect("read config.toml");
        for (n, line) in text.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            for banned in ["anglesey", "aebaa785", "my.site.com"] {
                assert!(
                    !lower.contains(banned),
                    "config.toml:{}: names a real place ({banned}): {line}",
                    n + 1
                );
            }
            let bytes = line.as_bytes();
            for i in 0..bytes.len().saturating_sub(8) {
                let real_prefix = matches!(bytes[i], b'E' | b'W' | b'S' | b'N');
                let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
                if real_prefix
                    && boundary
                    && bytes[i + 1..i + 9].iter().all(u8::is_ascii_digit)
                    && bytes.get(i + 9).is_none_or(|b| !b.is_ascii_alphanumeric())
                {
                    panic!(
                        "config.toml:{}: looks like a real ONS code — use the fictional \
                         X06…/X05… style: {line}",
                        n + 1
                    );
                }
            }
        }
    }

    /// Writes a minimal-but-complete config with the given portal ward codes and PPD
    /// districts. `extra` is appended verbatim.
    fn write_config(path: &std::path::Path, database: &str, wards: &[&str], extra: &str) {
        let mut s = format!(
            "[surreal]\nws_url = \"ws://localhost:8000\"\nnamespace = \"urban_integrity\"\n\
             database = \"{database}\"\n\n\
             [graph]\ncheckpoint_events_n = 1000\ncheckpoint_interval_secs = 60\n\
             checkpoint_path = \"data/checkpoints\"\nwal_channel_capacity = 10000\n\
             wal_batch_size = 100\nwal_batch_window_ms = 100\nshard_count = 64\n\
             snapshot_every_n_events = 1000\nsnapshot_interval_secs = 30\n\n\
             [ingest]\ncf_poll_interval_secs = 900\nscrapling_url = \"http://localhost\"\n\
             scrapling_port = 8765\n\n"
        );
        for w in wards {
            s.push_str(&format!(
                "[[ingest.planning_portals]]\nward_code = \"{w}\"\nportal_type = \"arcus\"\n\
                 url = \"https://planning.example.invalid/?ward={w}\"\n\n"
            ));
        }
        s.push_str("[detector]\nphoenix_window_days = 180\nphoenix_min_value = 10000.0\n\n");
        s.push_str(extra);
        std::fs::write(path, s).expect("write config");
    }

    #[test]
    fn config_file_in_falls_back_to_the_example_without_a_local_file() {
        let dir = tempfile::tempdir().unwrap();
        write_config(&dir.path().join(EXAMPLE_CONFIG_FILE), "an_example", &[], "");
        assert_eq!(
            AppConfig::config_file_in(dir.path()),
            dir.path().join(EXAMPLE_CONFIG_FILE)
        );
    }

    /// `config.local.toml` REPLACES `config.toml`; nothing from the example leaks through.
    /// The example has three portals and a PPD allowlist, the local file has one portal and
    /// no allowlist: a merge (whether whole-array or index-wise) would leave a trace of the
    /// example in one of those, so neither must appear.
    #[test]
    fn local_config_replaces_the_example_without_leakage() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            &dir.path().join(EXAMPLE_CONFIG_FILE),
            "an_example",
            &["X05999001", "X05999002", "X05999003"],
            "[enricher.ppd]\nenabled = true\npostcode_districts = [\"XX1\", \"XX2\"]\n",
        );
        write_config(
            &dir.path().join(LOCAL_CONFIG_FILE),
            "an_local",
            &["E05000001"],
            "",
        );

        let chosen = AppConfig::config_file_in(dir.path());
        assert_eq!(chosen, dir.path().join(LOCAL_CONFIG_FILE));

        let cfg = AppConfig::load_file(&chosen).expect("local config loads");
        assert_eq!(cfg.surreal.database, "an_local");
        let wards: Vec<&str> = cfg
            .ingest
            .planning_portals
            .iter()
            .map(|p| p.ward_code.as_str())
            .collect();
        assert_eq!(
            wards,
            ["E05000001"],
            "example portals leaked into the local config"
        );
        assert!(
            cfg.enricher.ppd.postcode_districts.is_none() && !cfg.enricher.ppd.enabled,
            "example [enricher.ppd] leaked into the local config: {:?}",
            cfg.enricher.ppd
        );
    }

    /// A directory holding only a non-file named `config.local.toml` (e.g. a directory) must
    /// not shadow the example.
    #[test]
    fn a_non_file_local_path_does_not_shadow_the_example() {
        let dir = tempfile::tempdir().unwrap();
        write_config(&dir.path().join(EXAMPLE_CONFIG_FILE), "an_example", &[], "");
        std::fs::create_dir(dir.path().join(LOCAL_CONFIG_FILE)).unwrap();
        assert_eq!(
            AppConfig::config_file_in(dir.path()),
            dir.path().join(EXAMPLE_CONFIG_FILE)
        );
    }

    /// Environment variables still override whichever file was chosen.
    #[test]
    fn env_overrides_the_chosen_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCAL_CONFIG_FILE);
        write_config(&path, "an_local", &[], "");
        // SAFETY: test-only env mutation, restored before returning. No other test reads
        // URBAN__GRAPH__SHARD_COUNT.
        unsafe {
            std::env::set_var("URBAN__GRAPH__SHARD_COUNT", "7");
        }
        let cfg = AppConfig::load_file(&path);
        unsafe {
            std::env::remove_var("URBAN__GRAPH__SHARD_COUNT");
        }
        assert_eq!(cfg.expect("loads").graph.shard_count, 7);
    }

    #[test]
    fn test_appconfig_analysis_defaults_to_none() {
        let json = serde_json::json!({
            "surreal": { "ws_url": "ws://localhost:8000", "namespace": "urban_integrity", "database": "main" },
            "graph": {
                "checkpoint_events_n": 1000, "checkpoint_interval_secs": 60,
                "checkpoint_path": "data/checkpoints", "wal_channel_capacity": 10000,
                "wal_batch_size": 100, "wal_batch_window_ms": 100, "shard_count": 64,
                "snapshot_every_n_events": 1000, "snapshot_interval_secs": 30
            },
            "ingest": { "cf_poll_interval_secs": 900, "scrapling_url": "http://localhost", "scrapling_port": 8765 },
            "detector": { "phoenix_window_days": 180, "phoenix_min_value": 10000.0 }
        });
        let cfg: AppConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.analysis.is_none());
    }

    #[test]
    fn test_appconfig_analysis_deserializes_when_set() {
        let json = serde_json::json!({
            "analysis": "an_black_country_7f3a9c2b",
            "surreal": { "ws_url": "ws://localhost:8000", "namespace": "urban_integrity", "database": "main" },
            "graph": {
                "checkpoint_events_n": 1000, "checkpoint_interval_secs": 60,
                "checkpoint_path": "data/checkpoints", "wal_channel_capacity": 10000,
                "wal_batch_size": 100, "wal_batch_window_ms": 100, "shard_count": 64,
                "snapshot_every_n_events": 1000, "snapshot_interval_secs": 30
            },
            "ingest": { "cf_poll_interval_secs": 900, "scrapling_url": "http://localhost", "scrapling_port": 8765 },
            "detector": { "phoenix_window_days": 180, "phoenix_min_value": 10000.0 }
        });
        let cfg: AppConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg.analysis.as_deref(), Some("an_black_country_7f3a9c2b"));
    }

    // ── WorkbenchConfig (D-05, Phase 42 plan 02) ───────────────────────────────

    #[test]
    fn test_workbench_config_default_bind_addr() {
        assert_eq!(WorkbenchConfig::default().bind_addr, "127.0.0.1:8090");
    }

    #[test]
    fn test_appconfig_absent_workbench_section_uses_default() {
        // No "workbench" key at all — #[serde(default)] must supply WorkbenchConfig::default().
        let cfg: AppConfig = serde_json::from_value(serde_json::json!({
            "surreal": { "ws_url": "ws://localhost:8000", "namespace": "urban_integrity", "database": "main" },
            "graph": {
                "checkpoint_events_n": 1000, "checkpoint_interval_secs": 60,
                "checkpoint_path": "data/checkpoints", "wal_channel_capacity": 10000,
                "wal_batch_size": 100, "wal_batch_window_ms": 100, "shard_count": 64,
                "snapshot_every_n_events": 1000, "snapshot_interval_secs": 30
            },
            "ingest": { "cf_poll_interval_secs": 900, "scrapling_url": "http://localhost", "scrapling_port": 8765 },
            "detector": { "phoenix_window_days": 180, "phoenix_min_value": 10000.0 }
        }))
        .unwrap();
        assert_eq!(cfg.workbench.bind_addr, "127.0.0.1:8090");
    }

    /// URBAN__WORKBENCH__BIND_ADDR must override config.toml's [workbench] bind_addr —
    /// exercises the real `AppConfig::load()` path (Environment::with_prefix("URBAN")),
    /// not a hand-built serde_json fixture, per the plan's acceptance criteria.
    #[test]
    fn test_workbench_bind_addr_env_override() {
        // Absolute, idempotent cwd change — see `workspace_root()` doc comment for why a
        // relative "../../" is unsafe once more than one test in this module changes cwd.
        std::env::set_current_dir(workspace_root()).ok();
        // SAFETY: test-only env mutation, restored before returning. Real races against
        // parallel tests reading URBAN__WORKBENCH__BIND_ADDR are theoretical — no other
        // test in this module sets or reads that key.
        unsafe {
            std::env::set_var("URBAN__WORKBENCH__BIND_ADDR", "127.0.0.1:9999");
        }
        let config = AppConfig::load();
        unsafe {
            std::env::remove_var("URBAN__WORKBENCH__BIND_ADDR");
        }
        let config = config.expect("AppConfig::load() should succeed with the env override set");
        assert_eq!(config.workbench.bind_addr, "127.0.0.1:9999");
    }

    #[test]
    fn test_graph_config_default_values() {
        let c = GraphConfig::default();
        assert_eq!(c.checkpoint_events_n, 1_000);
        assert_eq!(c.checkpoint_interval_secs, 60);
        assert_eq!(c.checkpoint_path, "data/checkpoints");
        assert_eq!(c.wal_channel_capacity, 10_000);
        assert_eq!(c.wal_batch_size, 100);
        assert_eq!(c.wal_batch_window_ms, 100);
        assert_eq!(c.shard_count, 64);
        assert_eq!(c.snapshot_every_n_events, 1_000);
        assert_eq!(c.snapshot_interval_secs, 30);
    }

    #[test]
    fn test_detector_config_raw_default_values() {
        let c = DetectorConfigRaw::default();
        assert_eq!(c.phoenix_window_days, Some(180));
        assert_eq!(c.phoenix_min_value, Some(10_000.0));
        assert!(c.imd_csv_path.is_none());
    }

    #[test]
    fn test_ingest_config_default_values() {
        let c = IngestConfig::default();
        assert!(c.ch_api_key.is_none());
        assert!(c.firecrawl_api_key.is_none());
        assert_eq!(c.cf_poll_interval_secs, 900);
        assert_eq!(c.scrapling_url, "http://localhost");
        assert_eq!(c.scrapling_port, 8765);
        assert!(c.planning_portals.is_empty());
        assert_eq!(c.planning_poll_interval_secs, 3600);
    }

    #[test]
    fn test_planning_portal_config_defaults_portal_type_to_idox() {
        let json = serde_json::json!({
            "ward_code": "W05001492",
            "url": "https://example.com/planning"
        });
        let p: PlanningPortalConfig = serde_json::from_value(json).unwrap();
        assert_eq!(p.portal_type, "idox");
        assert_eq!(p.ward_code, "W05001492");
        assert_eq!(p.url, "https://example.com/planning");
        assert!(p.lpa_code.is_none());
    }

    #[test]
    fn test_planning_portal_config_explicit_portal_type() {
        let json = serde_json::json!({
            "ward_code": "E05001234",
            "url": "https://planning.sandwell.gov.uk/portal",
            "portal_type": "arcus",
            "lpa_code": "E08000028"
        });
        let p: PlanningPortalConfig = serde_json::from_value(json).unwrap();
        assert_eq!(p.portal_type, "arcus");
        assert_eq!(p.lpa_code, Some("E08000028".into()));
    }

    #[test]
    fn test_ingest_config_firecrawl_key_defaults_to_none() {
        let json = serde_json::json!({
            "cf_poll_interval_secs": 900,
            "scrapling_url": "http://localhost",
            "scrapling_port": 8765
        });
        let ingest: IngestConfig = serde_json::from_value(json).unwrap();
        assert!(ingest.firecrawl_api_key.is_none());
    }

    #[test]
    fn ingest_config_firecrawl_key_deserializes_when_set() {
        let json = serde_json::json!({
            "cf_poll_interval_secs": 900,
            "scrapling_url": "http://localhost",
            "scrapling_port": 8765,
            "firecrawl_api_key": "fc-testkey"
        });
        let ingest: IngestConfig = serde_json::from_value(json).unwrap();
        assert_eq!(ingest.firecrawl_api_key.as_deref(), Some("fc-testkey"));
    }

    // ── ChEnricherConfig / graceful-degradation gate (CHA-03) ─────────────────

    #[test]
    fn test_ch_enricher_config_absent_api_key_is_disabled() {
        // Deserialising a config WITHOUT [enricher.ch] must yield is_enabled() == false
        let enricher: EnricherConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(enricher.ch.api_key.is_none());
        assert!(
            !enricher.ch.is_enabled(),
            "absent api_key should disable CH enricher"
        );
    }

    #[test]
    fn test_ch_enricher_config_empty_api_key_is_disabled() {
        let cfg: ChEnricherConfig =
            serde_json::from_value(serde_json::json!({"api_key": ""})).unwrap();
        assert!(
            !cfg.is_enabled(),
            "empty api_key should disable CH enricher"
        );
    }

    #[test]
    fn test_ch_enricher_config_whitespace_api_key_is_disabled() {
        let cfg: ChEnricherConfig =
            serde_json::from_value(serde_json::json!({"api_key": "   "})).unwrap();
        assert!(
            !cfg.is_enabled(),
            "whitespace-only api_key should disable CH enricher"
        );
    }

    #[test]
    fn test_ch_enricher_config_valid_api_key_is_enabled() {
        let cfg: ChEnricherConfig =
            serde_json::from_value(serde_json::json!({"api_key": "ch-testkey-abc123"})).unwrap();
        assert!(
            cfg.is_enabled(),
            "non-empty api_key should enable CH enricher"
        );
    }

    #[test]
    fn test_enricher_config_default_has_ch_disabled() {
        let enricher = EnricherConfig::default();
        assert!(!enricher.ch.is_enabled());
        assert_eq!(enricher.enrich_interval_secs, 86400);
    }

    // ── Phase 20 config structs (POL-01, POL-02, POL-03) ──────────────────────

    /// Test 1: EnricherConfig::default() deserialises from empty JSON without panicking.
    /// POL-01: electoral_commission.enabled defaults true.
    /// POL-02: council_registers.enabled defaults false (empty lpa_codes).
    /// POL-03: opencorporates.enabled defaults false.
    #[test]
    fn test_enricher_config_phase20_defaults_from_empty_json() {
        let enricher: EnricherConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        // Electoral Commission: enabled by default (OGL v3 open data)
        assert!(
            enricher.electoral_commission.enabled,
            "electoral_commission.enabled should default to true"
        );
        // Council Registers: disabled by default (requires lpa_codes)
        assert!(
            !enricher.council_registers.enabled,
            "council_registers.enabled should default to false"
        );
        assert!(
            enricher.council_registers.lpa_codes.is_empty(),
            "council_registers.lpa_codes should default to empty"
        );
        // OpenCorporates: disabled by default (requires api_key + oc_open_data_confirmed)
        assert!(
            !enricher.opencorporates.enabled,
            "opencorporates.enabled should default to false"
        );
    }

    /// Test 2: OpenCorporatesConfig::default() has monthly_limit == 200 (Pitfall 4 — not 500).
    #[test]
    fn test_opencorporates_config_default_monthly_limit_is_200() {
        let cfg = OpenCorporatesConfig::default();
        assert_eq!(
            cfg.monthly_limit, 200,
            "monthly_limit must default to 200 (free tier — Pitfall 4)"
        );
    }

    /// Test 3: ElectoralCommissionConfig fields — enabled (default true), cache_dir (None).
    #[test]
    fn test_electoral_commission_config_fields() {
        let cfg = ElectoralCommissionConfig::default();
        assert!(
            cfg.enabled,
            "ElectoralCommissionConfig.enabled should default to true"
        );
        assert!(
            cfg.cache_dir.is_none(),
            "ElectoralCommissionConfig.cache_dir should default to None"
        );

        // Verify deserialisation from JSON
        let from_json: ElectoralCommissionConfig =
            serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(from_json.enabled);
        assert!(from_json.cache_dir.is_none());

        // Verify cache_dir can be set
        let with_cache: ElectoralCommissionConfig =
            serde_json::from_value(serde_json::json!({"cache_dir": "data/electoral"})).unwrap();
        assert_eq!(with_cache.cache_dir.as_deref(), Some("data/electoral"));
    }

    /// Test 4: CouncilRegistersConfig fields — enabled (default false), lpa_codes (default empty).
    #[test]
    fn test_council_registers_config_fields() {
        let cfg = CouncilRegistersConfig::default();
        assert!(
            !cfg.enabled,
            "CouncilRegistersConfig.enabled should default to false"
        );
        assert!(
            cfg.lpa_codes.is_empty(),
            "CouncilRegistersConfig.lpa_codes should default to empty"
        );

        // Verify lpa_codes can be populated
        let with_codes: CouncilRegistersConfig = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "lpa_codes": ["E09000006", "E09000032"]
        }))
        .unwrap();
        assert!(with_codes.enabled);
        assert_eq!(with_codes.lpa_codes.len(), 2);
        assert_eq!(with_codes.lpa_codes[0], "E09000006");
    }

    /// Test 5: OpenCorporatesConfig fields — enabled (false), api_key (None),
    /// oc_open_data_confirmed (false), monthly_limit (200).
    #[test]
    fn test_opencorporates_config_fields() {
        let cfg = OpenCorporatesConfig::default();
        assert!(
            !cfg.enabled,
            "OpenCorporatesConfig.enabled should default to false"
        );
        assert!(
            cfg.api_key.is_none(),
            "OpenCorporatesConfig.api_key should default to None"
        );
        assert!(
            !cfg.oc_open_data_confirmed,
            "OpenCorporatesConfig.oc_open_data_confirmed should default to false"
        );
        assert_eq!(cfg.monthly_limit, 200);

        // Verify full deserialisation
        let full: OpenCorporatesConfig = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "api_key": "oc-testkey",
            "oc_open_data_confirmed": true,
            "monthly_limit": 500
        }))
        .unwrap();
        assert!(full.enabled);
        assert_eq!(full.api_key.as_deref(), Some("oc-testkey"));
        assert!(full.oc_open_data_confirmed);
        assert_eq!(full.monthly_limit, 500);
    }

    // -----------------------------------------------------------------------
    // Jurisdiction derivation (see the jurisdiction-gate spec)
    // -----------------------------------------------------------------------

    fn cfg_with_wards(wards: &[&str]) -> AppConfig {
        let json = serde_json::json!({
            "surreal": { "ws_url": "ws://localhost:8000", "namespace": "urban_integrity", "database": "main" },
            "graph": {
                "checkpoint_events_n": 1000, "checkpoint_interval_secs": 60,
                "checkpoint_path": "data/checkpoints", "wal_channel_capacity": 10000,
                "wal_batch_size": 100, "wal_batch_window_ms": 100, "shard_count": 64,
                "snapshot_every_n_events": 1000, "snapshot_interval_secs": 30
            },
            "ingest": { "cf_poll_interval_secs": 900, "scrapling_url": "http://localhost", "scrapling_port": 8765 },
            "detector": { "phoenix_window_days": 180, "phoenix_min_value": 10000.0 }
        });
        let mut cfg: AppConfig = serde_json::from_value(json).expect("minimal config parses");
        cfg.ingest.planning_portals = wards
            .iter()
            .map(|w| PlanningPortalConfig {
                lpa_code: None,
                ward_code: (*w).to_string(),
                portal_type: "arcus".to_string(),
                url: "https://example.invalid".to_string(),
            })
            .collect();
        cfg
    }

    #[test]
    fn analysis_nation_derives_from_configured_wards() {
        let cfg = cfg_with_wards(&["W05001492", "W05001503"]);
        assert_eq!(cfg.analysis_nation().unwrap(), Some(Nation::Wales));

        let cfg = cfg_with_wards(&["E05001234"]);
        assert_eq!(cfg.analysis_nation().unwrap(), Some(Nation::England));
    }

    /// A single deprivation index cannot be correct for two nations, and the
    /// England-and-Wales registries would be right for only part of the corpus. Refuse
    /// rather than warn.
    #[test]
    fn mixed_nation_wards_are_refused() {
        let cfg = cfg_with_wards(&["W05001492", "S13002522"]);
        match cfg.analysis_nation() {
            Err(JurisdictionError::MixedNations(nations)) => {
                assert_eq!(nations, vec![Nation::Wales, Nation::Scotland]);
            }
            other => panic!("expected MixedNations, got {other:?}"),
        }
    }

    #[test]
    fn mixed_nations_can_be_allowed_deliberately() {
        let mut cfg = cfg_with_wards(&["W05001492", "S13002522"]);
        cfg.jurisdiction.allow_mixed_nations = true;
        assert_eq!(
            cfg.analysis_nation().unwrap(),
            None,
            "a deliberately mixed analysis has no single nation to gate on"
        );
    }

    #[test]
    fn an_unparseable_ward_code_is_an_error_not_a_guess() {
        let cfg = cfg_with_wards(&["W05001492", "NOTAWARD"]);
        match cfg.analysis_nation() {
            Err(JurisdictionError::UnrecognisedWardCode(code)) => assert_eq!(code, "NOTAWARD"),
            other => panic!("expected UnrecognisedWardCode, got {other:?}"),
        }
    }

    /// An analysis with no wards has no nation to check against — not an error, but the
    /// preflight says so out loud.
    #[test]
    fn no_wards_means_no_nation() {
        let cfg = cfg_with_wards(&[]);
        assert_eq!(cfg.analysis_nation().unwrap(), None);
    }

    #[test]
    fn an_explicit_nation_covers_an_analysis_with_no_wards() {
        let mut cfg = cfg_with_wards(&[]);
        cfg.jurisdiction.nation = Some("scotland".to_string());
        assert_eq!(cfg.analysis_nation().unwrap(), Some(Nation::Scotland));
    }

    /// An override that contradicts the wards is the exact mistake this gate exists to
    /// catch, so it fails loudly instead of silently winning.
    #[test]
    fn an_explicit_nation_that_contradicts_the_wards_is_refused() {
        let mut cfg = cfg_with_wards(&["W05001492"]);
        cfg.jurisdiction.nation = Some("Scotland".to_string());
        match cfg.analysis_nation() {
            Err(JurisdictionError::OverrideDisagreesWithWards { declared, derived }) => {
                assert_eq!(declared, Nation::Scotland);
                assert_eq!(derived, Nation::Wales);
            }
            other => panic!("expected OverrideDisagreesWithWards, got {other:?}"),
        }
    }

    #[test]
    fn an_unparseable_nation_name_is_an_error() {
        let mut cfg = cfg_with_wards(&[]);
        cfg.jurisdiction.nation = Some("Cymru".to_string());
        assert!(matches!(
            cfg.analysis_nation(),
            Err(JurisdictionError::UnrecognisedNation(_))
        ));
    }
}
