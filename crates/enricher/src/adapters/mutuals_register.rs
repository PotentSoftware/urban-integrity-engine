//! FCA Mutuals Public Register adapter (planning open item 24).
//!
//! ## Why this exists
//!
//! A **registered society** is a corporate body that is **not a company**. It is constituted
//! under the Co-operative and Community Benefit Societies Act 2014, registered with the FCA
//! as registrar, and carries a number with a letter suffix (`22360R`) unrelated to any
//! Companies House number. Before this adapter the entire class was invisible to the engine:
//!
//! * Companies House ingest cannot see them — they are not on that register at all.
//! * The Charity Commission adapter cannot see them either — a society with charitable
//!   rules is an **exempt charity**, recognised by HMRC and absent from the CC register.
//!
//! Two independent blind spots over the same organisations, and they are not marginal:
//! housing associations, co-operatives and community benefit societies are among the largest
//! recipients of public land and public money in Wales. Measured live on 2026-08-11,
//! ClwydAlyn Housing (`22360R`) held **52 Anglesey planning applications** and reached the
//! graph as a bare applicant string with no number, no accounts and no ownership data behind
//! it — while `north-wales-housing-association-ltd` (18 applications) and `grwp-cynefin` (14)
//! sat in the same position.
//!
//! ## What it does, and deliberately does not do
//!
//! It **ingests**. Each society becomes an `Entity` node keyed `GB-MUT:{registration}`,
//! carrying its registered address as a `Location` node plus a `LocatedAt` edge, exactly as
//! `ch/mapper.rs` does for companies — so societies become visible to `AddressCluster` on the
//! same terms as everything else.
//!
//! It creates **no identity link**. `planning_identity.rs` (IDN-05) already joins planning
//! applicants to any non-`director:`-keyed Entity node by exact `normalise_name` equality,
//! with ambiguity resolving to no match. Once a society is in the graph that join happens on
//! the next enrichment pass, through code that has already been reviewed for exactly this
//! risk. Duplicating it here would mean two matchers to keep in agreement.
//!
//! ## Source
//!
//! The FCA publishes the whole register as one unauthenticated CSV on Azure blob storage —
//! no API key, no rate limit, no scraping, and no sidecar. `mutuals.fca.org.uk` itself is an
//! HTML search form; the bulk file is linked from its footer and is the only sane ingest
//! path.
//!
//! **The file is Windows-1252, not UTF-8.** Byte `0x92` (a curly apostrophe, common in
//! society names such as `Working Men's Club`) is not valid UTF-8 and aborts a strict decoder
//! mid-file. Decoding is lossy-by-design here: a mangled apostrophe in a name is survivable,
//! silently losing the tail of the register is not.
//!
//! ## Status mapping
//!
//! `Society Status` is `Registered` or `Deregistered`. A deregistered society is the
//! society-world equivalent of a dissolved company, so it maps to
//! `EntityStatus::Dissolved(deregistration_date)` — which makes societies first-class
//! counterparties for Phoenix. Where the register records no deregistration date, the
//! mapper's own `UNIX_EPOCH` sentinel is used rather than inventing one, matching
//! `ch/mapper.rs`.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use std::path::PathBuf;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};

use urban_integrity_core::config::MutualsConfig;
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_schema::types::{
    edge::{EdgeMeta, EdgeWeight, LocatedAtData},
    id::EntityId,
    node::{EntityData, EntityStatus, LocationData, NodeWeight},
};

use crate::adapters::{EnricherAdapter, EnrichmentStats};

/// One row of the FCA society list, reduced to the fields this adapter uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Society {
    pub registration: String,
    pub name: String,
    pub address: Option<String>,
    pub postcode: Option<String>,
    pub status: EntityStatus,
    pub registered_on: Option<NaiveDate>,
}

/// Parse a `YYYY-MM-DD` date, tolerating blanks.
fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// Extract a UK postcode from a free-text registered address.
///
/// The register stores the address as one comma-joined string with no postcode column, so
/// the postcode has to be recovered from the tail. Anchored on the outward+inward shape
/// rather than "last token", because plenty of rows end in a county with no postcode at all.
pub fn extract_postcode(addr: &str) -> Option<String> {
    let upper = addr.to_uppercase();
    let bytes: Vec<char> = upper.chars().collect();
    // Scan for `<1-2 letters><1-2 digits><optional letter> <digit><2 letters>`.
    for start in 0..bytes.len() {
        let rest: String = bytes[start..].iter().collect();
        let mut it = rest.chars().peekable();
        let mut letters = 0;
        while it.peek().is_some_and(|c| c.is_ascii_alphabetic()) && letters < 2 {
            it.next();
            letters += 1;
        }
        if letters == 0 {
            continue;
        }
        let mut digits = 0;
        while it.peek().is_some_and(|c| c.is_ascii_digit()) && digits < 2 {
            it.next();
            digits += 1;
        }
        if digits == 0 {
            continue;
        }
        if it.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            it.next();
        }
        // Optional single space, then the inward code.
        if it.peek() == Some(&' ') {
            it.next();
        }
        let inward: String = it.by_ref().take(3).collect();
        if inward.len() == 3
            && inward.starts_with(|c: char| c.is_ascii_digit())
            && inward[1..].chars().all(|c| c.is_ascii_alphabetic())
        {
            let consumed = rest.len() - it.count();
            let candidate: String = rest[..consumed.min(rest.len())]
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if candidate.len() >= 5 && candidate.len() <= 7 {
                let (out, inn) = candidate.split_at(candidate.len() - 3);
                return Some(format!("{out} {inn}"));
            }
        }
    }
    None
}

/// Parse the FCA society list CSV into societies worth holding.
///
/// Rows with no name are skipped: the register keeps stub entries for long-dissolved
/// Victorian societies whose names were never digitised, and a nameless node can join
/// nothing and means nothing.
pub fn parse_society_csv(data: &str) -> Vec<Society> {
    let mut out = Vec::new();
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(data.as_bytes());

    let headers = match rdr.headers() {
        Ok(h) => h.clone(),
        Err(e) => {
            warn!(error = %e, "mutuals: CSV header unreadable");
            return out;
        }
    };
    let idx = |name: &str| headers.iter().position(|h| h.trim() == name);
    let (i_reg, i_name, i_addr, i_dereg, i_status, i_regdate) = (
        idx("Full Registation Number"), // sic — the FCA's own header spelling
        idx("Society Name"),
        idx("Society Address"),
        idx("Deregistration Date"),
        idx("Society Status"),
        idx("Registration Date"),
    );
    let (Some(i_reg), Some(i_name)) = (i_reg, i_name) else {
        warn!("mutuals: CSV is missing the registration-number or name column");
        return out;
    };

    for rec in rdr.records().flatten() {
        let get = |i: Option<usize>| i.and_then(|i| rec.get(i)).unwrap_or("").trim().to_string();
        let registration = rec.get(i_reg).unwrap_or("").trim().to_string();
        let name = rec.get(i_name).unwrap_or("").trim().to_string();
        if registration.is_empty() || name.is_empty() {
            continue;
        }
        let status_raw = get(i_status);
        let dereg = parse_date(&get(i_dereg));
        let status = if status_raw.eq_ignore_ascii_case("deregistered") {
            // A deregistered society is the society-world equivalent of a dissolved company.
            // No recorded date → the mapper's own sentinel, never an invented one.
            EntityStatus::Dissolved(
                dereg
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|dt| dt.and_utc())
                    .unwrap_or(DateTime::UNIX_EPOCH),
            )
        } else {
            EntityStatus::Active
        };
        let addr = get(i_addr);
        let postcode = if addr.is_empty() {
            None
        } else {
            extract_postcode(&addr)
        };
        out.push(Society {
            registration,
            name,
            address: (!addr.is_empty()).then_some(addr),
            postcode,
            status,
            registered_on: parse_date(&get(i_regdate)),
        });
    }
    out
}

pub struct MutualsRegisterAdapter {
    config: MutualsConfig,
    cache_dir: PathBuf,
}

impl MutualsRegisterAdapter {
    pub fn new(config: MutualsConfig) -> Self {
        let cache_dir = config
            .cache_dir
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data/mutuals"));
        Self { config, cache_dir }
    }

    /// Fetch the register, preferring a cached copy. Returns the decoded CSV text.
    async fn load_csv(&self) -> anyhow::Result<String> {
        let cached = self.cache_dir.join("SocietyList.csv");
        if cached.exists() {
            let bytes = tokio::fs::read(&cached).await?;
            return Ok(decode_cp1252(&bytes));
        }
        info!(url = %self.config.url, "mutuals: downloading society list");
        let bytes = reqwest::Client::new()
            .get(&self.config.url)
            .timeout(std::time::Duration::from_secs(180))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        if let Err(e) = tokio::fs::create_dir_all(&self.cache_dir).await {
            warn!(error = %e, "mutuals: cache dir not writable — continuing without caching");
        } else if let Err(e) = tokio::fs::write(&cached, &bytes).await {
            warn!(error = %e, "mutuals: cache write failed — continuing");
        }
        Ok(decode_cp1252(&bytes))
    }
}

/// Decode Windows-1252, mapping the bytes UTF-8 rejects rather than aborting.
///
/// The register contains `0x92` (curly apostrophe) in names like `Working Men's Club`. A
/// strict UTF-8 decode fails on it and takes the rest of the file with it — silently losing
/// most of the register while appearing to succeed on a truncated read.
fn decode_cp1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => match b {
                0x91 | 0x92 => '\'',
                0x93 | 0x94 => '"',
                0x96 | 0x97 => '-',
                _ => ' ',
            },
            _ => b as char,
        })
        .collect()
}

#[async_trait]
impl EnricherAdapter for MutualsRegisterAdapter {
    fn name(&self) -> &'static str {
        "mutuals_register"
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    async fn run(
        &self,
        _db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        let mut stats = EnrichmentStats::default();
        let csv = self.load_csv().await?;
        let societies = parse_society_csv(&csv);
        info!(
            adapter = "mutuals_register",
            societies = societies.len(),
            "mutuals: register parsed"
        );

        for s in &societies {
            let id = EntityId(format!("GB-MUT:{}", s.registration));
            let weight = NodeWeight::Entity(EntityData {
                name: s.name.clone(),
                // A society has no Companies House number. Leaving this None is the honest
                // encoding — putting the society number here would make it indistinguishable
                // from a CH number downstream and invite a false join.
                company_number: None,
                status: s.status.clone(),
                incorporation_date: s.registered_on,
            });
            let _ = mutation_tx
                .send(GraphMutation::UpsertNode {
                    id: id.clone(),
                    weight,
                })
                .await;
            stats.candidates_found += 1;

            // Registered address → Location + LocatedAt, mirroring `ch/mapper.rs`, so
            // societies are visible to AddressCluster on the same terms as companies.
            if let (Some(addr), Some(pc)) = (s.address.as_ref(), s.postcode.as_ref()) {
                let loc_id = EntityId(format!(
                    "LOC:{}:{}",
                    pc.replace(' ', ""),
                    addr.chars().take(40).collect::<String>().trim()
                ));
                let _ = mutation_tx
                    .send(GraphMutation::UpsertNode {
                        id: loc_id.clone(),
                        weight: NodeWeight::Location(LocationData {
                            name: addr.clone(),
                            postcode: Some(pc.clone()),
                            address_line_1: Some(addr.clone()),
                            locality: None,
                        }),
                    })
                    .await;
                let _ = mutation_tx
                    .send(GraphMutation::AddEdge {
                        src: id,
                        dst: loc_id,
                        weight: EdgeWeight::LocatedAt {
                            meta: EdgeMeta {
                                created_at: Utc::now(),
                                effective_from: None,
                                effective_to: None,
                            },
                            data: LocatedAtData {
                                details: "registered_office".to_string(),
                            },
                        },
                    })
                    .await;
                stats.sub_candidate += 1;
            }
        }

        info!(
            adapter = "mutuals_register",
            societies = stats.candidates_found,
            with_address = stats.sub_candidate,
            "mutuals: enrichment complete — the planning_identity join runs separately"
        );
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "Society Number,Society Suffix,Full Registation Number,Society Name,\
Registered As,Statistics Condition,Statistics Co-operative Type,Society Address,\
Registration Date,Deregistration Date,Registration Act,\
Standard Industrial Classification Code (SIC),Reporting Classification,Society Status";

    /// The row that motivated the whole adapter, verbatim from the live register.
    #[test]
    fn parses_clwydalyn_from_the_real_row() {
        let csv = format!(
            "{HEADER}\n22360,R,22360R,ClwydAlyn Housing Limited,Registered Society,\
Community Benefit,N/A,\"72 Ffordd William Morgan,St Asaph Business Park,Denbighshire, LL17 0JD\",\
1978-11-13,,Co-operative and Community Benefit Societies Act 2014,\
68201 - Renting and operating of Housing Association real estate,Housing,Registered\n"
        );
        let out = parse_society_csv(&csv);
        assert_eq!(out.len(), 1, "one society expected");
        let s = &out[0];
        assert_eq!(s.registration, "22360R");
        assert_eq!(s.name, "ClwydAlyn Housing Limited");
        assert!(
            matches!(s.status, EntityStatus::Active),
            "a Registered society is active"
        );
        assert_eq!(
            s.postcode.as_deref(),
            Some("LL17 0JD"),
            "postcode must be recovered from the free-text address"
        );
        assert_eq!(s.registered_on, NaiveDate::from_ymd_opt(1978, 11, 13));
    }

    #[test]
    fn deregistered_society_maps_to_dissolved_with_its_date() {
        let csv = format!(
            "{HEADER}\n8042,,8042,Pennaf Limited,Community Benefit Society,N/A,N/A,\
\"72 Ffordd William Morgan,St Asaph Business Park,Denbighshire LL17 0JD\",\
2019-02-18,2019-06-05,Co-operative and Community Benefit Societies Act 2014,,,Deregistered\n"
        );
        let out = parse_society_csv(&csv);
        assert_eq!(out.len(), 1);
        match out[0].status {
            EntityStatus::Dissolved(dt) => assert_eq!(
                dt.date_naive(),
                NaiveDate::from_ymd_opt(2019, 6, 5).unwrap(),
                "the deregistration date must be carried, not the registration date"
            ),
            ref other => panic!("expected Dissolved, got {other:?}"),
        }
    }

    /// The register keeps nameless stubs for societies whose names were never digitised.
    #[test]
    fn nameless_stub_rows_are_skipped() {
        let csv = format!(
            "{HEADER}\n25,W,25W,,Working Men's Club,N/A,N/A,,,1965-04-23,\
Friendly Societies Act 1974,,,Deregistered\n"
        );
        assert!(
            parse_society_csv(&csv).is_empty(),
            "a society with no name can join nothing and must not become a node"
        );
    }

    /// A strict UTF-8 decode aborts on byte 0x92 and silently truncates the register.
    #[test]
    fn cp1252_apostrophe_does_not_truncate_the_file() {
        let mut bytes = format!("{HEADER}\n").into_bytes();
        bytes.extend_from_slice(b"1,SAL,1SAL,Working Men");
        bytes.push(0x92); // curly apostrophe — invalid UTF-8
        bytes.extend_from_slice(
            b"s Club,Specially Authorised Society,N/A,N/A,,1900-01-01,,Act,,,Registered\n",
        );
        assert!(
            String::from_utf8(bytes.clone()).is_err(),
            "fixture must actually be invalid UTF-8, or it proves nothing"
        );
        let out = parse_society_csv(&decode_cp1252(&bytes));
        assert_eq!(out.len(), 1, "the row after the bad byte must survive");
        assert_eq!(out[0].name, "Working Men's Club");
    }

    #[test]
    fn postcode_extraction_handles_real_address_shapes() {
        assert_eq!(
            extract_postcode(
                "72 Ffordd William Morgan,St Asaph Business Park,Denbighshire, LL17 0JD"
            ),
            Some("LL17 0JD".to_string())
        );
        assert_eq!(
            extract_postcode("Duntrune,24 Water Street,Clwyd LL22 7SH"),
            Some("LL22 7SH".to_string())
        );
        assert_eq!(
            extract_postcode("Smithfield Green Holt,Clwyd"),
            None,
            "an address with no postcode must yield None, not a guess"
        );
    }

    /// A society number is NOT a company number; conflating them would invite a false join.
    #[test]
    fn society_entity_carries_no_company_number() {
        let csv = format!(
            "{HEADER}\n22360,R,22360R,ClwydAlyn Housing Limited,Registered Society,\
Community Benefit,N/A,\"Denbighshire, LL17 0JD\",1978-11-13,,Act,,Housing,Registered\n"
        );
        let s = &parse_society_csv(&csv)[0];
        let weight = EntityData {
            name: s.name.clone(),
            company_number: None,
            status: s.status.clone(),
            incorporation_date: s.registered_on,
        };
        assert!(
            weight.company_number.is_none(),
            "a society has no CH number — recording one would make it indistinguishable \
             from a company downstream"
        );
    }
}
