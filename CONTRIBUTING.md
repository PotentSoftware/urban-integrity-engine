# Contributing to UrbanIntegrity

Thanks for your interest. Please read this before opening a pull request — the
licensing section in particular, because we cannot merge a contribution that has
not been made under the terms below.

## Licence of the project

UrbanIntegrity is licensed under the **GNU Affero General Public License, version
3 or later** (`AGPL-3.0-or-later`). See [`LICENSE`](LICENSE).

The `urban-integrity-schema` crate is additionally available under
**Apache-2.0** (`Apache-2.0 OR AGPL-3.0-or-later`), so that the entity model,
identifier conventions and `SCHEMA_DEF` can be adopted by other tools without
copyleft obligations. Interoperability on the data model is more valuable to this
project than exclusivity over it.

The AGPL's network clause (section 13) is deliberate. If you run a modified
version of this software as a network service, you must offer its complete source
to the users of that service.

## Contributor Licence Agreement (CLA)

**Every contribution requires a signed CLA before it can be merged.** No
exceptions, including for single-line changes.

The project maintains the option to offer the software under separate commercial
terms alongside the AGPL. That option only exists if a single party holds
sufficient rights in the whole codebase. Once a contribution is merged under the
bare AGPL with no additional grant, that option is permanently lost for the
project as a whole — it cannot be recovered later without tracking down and
re-obtaining permission from every past contributor. This is why the CLA is
required at the point of contribution rather than requested afterwards.

### What the CLA does and does not do

It **does**:

- grant the project a perpetual, worldwide, irrevocable licence to use,
  reproduce, modify and distribute your contribution, **including under licences
  other than the AGPL**;
- ask you to confirm you have the right to make the contribution (that you wrote
  it, or are authorised to submit it — check your employment contract if the work
  is related to your job);
- ask you to confirm the contribution is your original work and does not
  knowingly infringe anyone's rights.

It **does not**:

- take your copyright away — you retain full ownership of your contribution and
  may use it however you like elsewhere;
- allow the project to make your contribution proprietary *to you* — the AGPL
  release stands permanently, and nothing can retract the public version.

The agreement used is the **Harmony Individual Contributor Licence Agreement
1.0, Licence variant** (inbound `AGPL-3.0-or-later`, outbound `Any`), or an
equivalent Apache-style ICLA. A Developer Certificate of Origin sign-off is
**not** sufficient on its own: the DCO certifies where code came from, it does not
grant relicensing rights.

### How to sign

1. Open your pull request as normal.
2. A maintainer will link you to the CLA document on first contribution.
3. Sign it once. It covers all your future contributions to the project.

If you are contributing on behalf of an employer, you will need the Corporate CLA
signed by someone authorised to bind the organisation.

If you would rather not sign a CLA, that is entirely reasonable — please open an
issue describing the change instead. A well-specified bug report is genuinely
useful and carries no licensing question at all.

## What this project will and will not accept

This is an evidence tool. Its only real asset is whether its output is believed
by people who are motivated to disbelieve it — auditors, defence solicitors,
regulators, editors. Several contribution categories are refused on that basis,
not on ideological grounds:

- **No fuzzy or phonetic identity matching.** The project enforces an
  exact-identifier-only join policy. Probabilistic name matching has previously
  produced tens of thousands of false links here, each of which is a false
  accusation against a real, named, living person. Recall is deliberately traded
  away for precision. The history is in the project's planning notes.
- **No target selection by party, person, or organisation.** Detectors run
  blind across the whole corpus or they do not run. A detector that is pointed is
  not evidence, it is an allegation with a build system.
- **No contribution that embeds real personal data** — no real names, dates of
  birth, addresses, company officer records or Land Registry title numbers in
  test fixtures, documentation, example output or commit messages. Use the
  synthetic planted fixture (`crates/test-support/src/planted.rs`). This is
  enforced in review; see `DATA-SOURCES.md`.
- **No source whose licence forbids the use you are making of it.** New adapters
  must arrive with the source's licence terms documented in `DATA-SOURCES.md`.

## Development

Setting up from a fresh clone (OS packages, SurrealDB, Trunk, the zero-key planted
fixture): [`docs/GETTING-STARTED.md`](docs/GETTING-STARTED.md), Part A. Day-to-day
work: [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) and
[`docs/TESTING.md`](docs/TESTING.md). In short:

```bash
cargo build --workspace
cargo test --workspace                     # DB-gated tests skip
URBAN_DB_TESTS=1 cargo test --workspace    # requires SurrealDB on ws://localhost:8000
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

The workspace is clippy-clean and CI fails on any new warning. The toolchain is
pinned in `rust-toolchain.toml`; do not track `stable`.

**Run one `cargo` invocation at a time.** Concurrent builds have repeatedly
exhausted memory on developer machines here — `rustc` peaks around 3GB per
process on this workspace.

DB-gated tests must take their namespace from `urban-integrity-test-support`
(`TestNs` / `NsGuard`), never a hand-rolled `format!("test_{uuid}")`. Hand-rolled
namespaces leak and do not get cleaned up on drop.

## Reporting a security or data-protection issue

Do **not** open a public issue. Email the maintainers directly. This includes:

- anything that could expose the contents of a licensed dataset;
- any path by which personal data could leak into a public artifact;
- server-side request forgery or injection in the Scrapling sidecar.

## Reporting a suspected false positive

If you believe the engine has produced an incorrect finding about a real person
or organisation, say so in an issue and it will be treated as a priority defect,
not a tuning request. Please do not include the personal data itself in the
issue — reference the identifier and contact the maintainers directly with
specifics.
