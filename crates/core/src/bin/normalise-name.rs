//! Debug/analysis shim exposing `normalise_name` — the canonical name-comparison key — to
//! out-of-process tooling.
//!
//! Exists so identity-resolution scripts compare names with the SHIPPED function rather than
//! a re-implementation. `planning_identity.rs` (IDN-05) decides whether a planning applicant
//! IS a Companies House entity purely on `normalise_name(a) == normalise_name(b)`, so any
//! script that proposes candidates for that adapter must produce byte-identical keys or it
//! proposes joins the adapter will then silently reject.
//!
//! The rule it encodes is not obvious — lowercase, elide apostrophes and hyphens, map other
//! punctuation to space, then strip a TRAILING `limited`/`ltd`/`plc`/`llp`/`llc` and collapse
//! whitespace. A Python copy that dropped the suffix-stripping, or stripped it anywhere but
//! the end, would produce a keyspace that joins nothing — the same defect as the
//! `applicant_slug` vs `slugify` split (planning open item 17), which cost this project a
//! full re-key.
//!
//! Reads one name per line on stdin; writes one normalised key per line on stdout, in order.

use std::io::{self, BufRead, Write};
use urban_integrity_core::stream::normalise_name;

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut out = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        writeln!(out, "{}", normalise_name(&line?))?;
    }
    out.flush()
}
