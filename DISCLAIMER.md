# Disclaimer — read before acting on anything this software produces

UrbanIntegrity Engine is a **work in progress**. It is research software that joins public
records and flags patterns worth a closer look. **It makes mistakes.** A flag means
"someone qualified should look at this". It does not mean "something wrong happened".

## Do not accuse anyone on the strength of this software's output

Never treat an alert, motif, score, probe verdict, report, graph or workbench view from this
application as evidence that any person or organisation has done anything wrong. Do not
publish, report, repeat or act on such a claim unless **both** of these have happened first:

1. **A qualified fraud investigator is involved.** For example, an accredited counter-fraud
   specialist, a forensic accountant, or the investigating function of the relevant public
   body. They must check the underlying source records independently, not just this
   software's view of them.
2. **You have taken legal advice up front**, before any allegation is made in public or in
   private, or passed to an employer, a public body, a journalist or a regulator.
   Allegations of fraud or corruption can be defamatory and can cause serious harm to the
   people named. Processing personal data in connection with alleged wrongdoing also
   carries obligations under UK GDPR and the Data Protection Act 2018. See
   [`docs/gdpr-basis.md`](docs/gdpr-basis.md).

## Why the output can be wrong

- **Source data is incomplete, late or wrong.** Public registers contain errors, duplicates,
  stale addresses and missing filings. The engine inherits all of them.
- **Identity matching is hard.** Different people share names. Companies House officer
  records can merge two people into one, or split one person into several. A link between
  two records is a hypothesis, not a fact.
- **Patterns are not proof.** A company that dissolves and is followed by a similar one, a
  shared registered address, or a relative on a planning committee all have ordinary,
  lawful explanations far more often than corrupt ones.
- **Coverage is partial.** The absence of a flag says nothing about whether wrongdoing
  occurred, and a "Clear" verdict only describes the data the engine actually saw.
- **The software has bugs.** It is under active development and its detectors, scoring and
  verdicts change between versions. During development a flag raised by this engine
  against a real person was later shown to be a false positive caused by a data-matching
  error.

## No warranty

The software is provided "as is", without warranty of any kind, as set out in the
[LICENSE](LICENSE). The authors accept no liability for decisions made, or statements
published, on the basis of its output.

This disclaimer is not legal advice.
