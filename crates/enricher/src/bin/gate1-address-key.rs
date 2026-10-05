//! Debug/analysis shim exposing gate-1's address key to out-of-process tooling.
//!
//! **Why this exists.** `scripts/land_flip_funnel.py` measures the land-flip funnel, gate-1's
//! address match included. It previously computed that match with its own Python arithmetic —
//! a *second* implementation, structurally unable to stay in step with the Rust one the detector
//! ships. When 29-08 reported `candidates_written=0`, the Python re-derivation independently
//! produced zero too, and that agreement was recorded as cross-validation of an "honest
//! structural finding". It was nothing of the kind: both sides shared the same wrong assumption,
//! and the real defect was a parser bug that made full-triple matches impossible for the
//! corpus's dominant address shape. Two implementations agreeing is not independent evidence.
//!
//! So the harness must call the shipped code rather than imitate it, and this binary is the one
//! seam through which it does (Open Item 13, the phase 29 verification report).
//!
//! **Why `normalise_app_address` and not `parse_planning_address`.** Gate-1 compares a
//! `(postcode, paon, street)` triple. Exposing only the parser would still leave the caller
//! re-implementing the postcode half — the `location.postcode`-then-parsed fallback, and
//! `normalise_postcode`'s length and alphanumeric validation — which is the same trap one level
//! down. This emits the whole triple, exactly as `assemble_app_contexts` builds it.
//!
//! **Contract.** One record per input line, `postcode<TAB>address`; a missing or empty first
//! field means "no location postcode, fall back to the parsed one". Writes one JSON object per
//! input line on stdout, in order, so callers can zip results back positionally. `usable` mirrors
//! gate-1's own precondition: all three components non-empty, or the application is skipped.
//!
//! ```text
//! printf '\t1,HIGH STREET,EXAMPLETON,XX1 1AA\n' | gate1-address-key
//! {"paon":"1","postcode":"XX11AA","street":"high street","usable":true}
//! ```

use std::io::{self, BufRead, Write};
use urban_integrity_enricher::land_flip::normalise_app_address;

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut out = io::BufWriter::new(io::stdout().lock());

    for line in stdin.lock().lines() {
        let raw = line?;
        // Split on the FIRST tab only — addresses may legitimately contain further tabs.
        let (postcode, address) = match raw.split_once('\t') {
            Some((p, a)) => (p, a),
            None => ("", raw.as_str()),
        };
        let postcode = (!postcode.trim().is_empty()).then(|| postcode.trim());

        let (postcode_norm, paon_norm, street_norm) = normalise_app_address(postcode, address);
        let usable = !postcode_norm.is_empty() && !paon_norm.is_empty() && !street_norm.is_empty();

        let v = serde_json::json!({
            "postcode": postcode_norm,
            "paon": paon_norm,
            "street": street_norm,
            "usable": usable,
        });
        writeln!(out, "{v}")?;
    }

    out.flush()
}
