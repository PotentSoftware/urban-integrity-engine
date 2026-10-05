//! Debug/analysis shim exposing `applicant_slug` — the canonical `director:{slug}` key
//! function — to out-of-process tooling.
//!
//! Exists so migration and analysis scripts key entities with the SHIPPED function rather than
//! a re-implementation. Two near-identical slug functions already exist in this codebase
//! (`naming::slugify` joins with `_`, this one with `-`), and picking the wrong one silently
//! produces a disjoint keyspace that joins nothing — which is the very defect being repaired
//! here (planning open item 17). A third re-implementation in Python would be the same
//! mistake again.
//!
//! Reads one name per line on stdin; writes one slug per line on stdout, in order.

use std::io::{self, BufRead, Write};
use urban_integrity_ingestor::planning::mapper::applicant_slug;

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut out = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        writeln!(out, "{}", applicant_slug(&line?))?;
    }
    out.flush()
}
