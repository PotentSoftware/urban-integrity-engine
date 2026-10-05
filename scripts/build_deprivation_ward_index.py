#!/usr/bin/env python3
"""Build a ward-level deprivation index the detector can join to, for any UK nation.

WHY. `P_c` is documented as IMD-weighted and was inert: `data/imd_2025.csv` never existed,
so `serve` logged "IMD index load failed" and every alert carried `m_env = 1.0`.

THE UK HAS FOUR DEPRIVATION INDICES AND THEY ARE NOT INTERCHANGEABLE. This is the fact the
whole script is shaped around:

  nation            index        geography ranked        denominator   ward codes
  England           IMD 2019     32,844 LSOAs (E01…)     32844         E05…
  Wales             WIMD 2019     1,909 LSOAs (W01…)      1909         W05…
  Scotland          SIMD 2020v2   6,976 data zones (S01…) 6976         S13…
  Northern Ireland  NIMDM 2017      890 SOAs / 462 wards   462         N08…

All four rank 1 = most deprived. None is comparable with another — different indicators,
different weights, different denominators — and every publisher says so explicitly. A rank
of 500 is the 26th percentile in Wales and the 1.5th in England. So **one nation per file**,
and the emitted `IMD Denominator` column is what lets the loader enforce it: it rejects a
file whose rows disagree about the denominator, and one whose ranks exceed it.

Supplying the wrong nation's index is not a loud failure — it loads cleanly, matches zero
wards, and leaves the weighting inert while appearing configured. That is exactly what would
have happened had the English index been dropped in for the Anglesey analysis.

NORTHERN IRELAND IS STRUCTURALLY DIFFERENT, and it is the easy one. NISRA publishes NIMDM
**at ward level directly**, with official `N08…` codes and its own 1..462 ranking, so there
is no small-area aggregation and no postcode bridge — the ranks are already the thing we
need. Its denominator is therefore a WARD count, not a small-area count, which is precisely
why the denominator lives in the file rather than being assumed.

THE BRIDGE, for the other three. Their indices rank small areas, while alerts carry current
electoral wards — and ONS's published small-area-to-ward lookups stop at 2020 wards, so they
do not contain (for example) Anglesey's 2022 boundaries. The ONS Postcode Directory carries
both the small-area code and the current ward on every postcode, so each ward's small areas
are the distinct small-area codes of its live postcodes. Terminated postcodes are excluded:
they can sit in areas that no longer reflect where anyone lives.

AGGREGATION — median, deliberately, not mean. A deprivation rank is ORDINAL: the gap between
ranks 100 and 200 is not the gap between 1,700 and 1,800, and the publishers' guidance warns
against averaging ranks. A median is a rank-preserving summary; a mean is not. The per-ward
spread is written out (`Rank Min`/`Rank Max`) because a ward covering ranks 168..1325 is one
number hiding two very different places.

  ⚠ This is NOT the official method for summarising to larger areas. MHCLG's own district
    summaries use rank-of-average-SCORE (scores are cardinal, ranks are not) alongside
    "proportion of small areas in the most deprived 10%". Doing that here would require the
    scores files and a complete national extract to re-rank against, and would make the
    denominator a ward count. Recorded as a known deviation rather than hidden — see
    planning open item 34.

HOW COARSE THE RESULT IS DEPENDS ENTIRELY ON THE NATION, and Scotland is the outlier.
Measured by building all four (2026-08-16), small areas per ward and the ward's rank spread
as a share of the whole national index:

  Northern Ireland   1 area/ward     0% spread   NISRA ranks wards directly — exact
  England            2 areas/ward   30% spread   usable
  Wales              4 areas/ward   35% spread   usable
  Scotland          42 areas/ward   88% spread   NOT usable at ward level

Scotland's wards are multi-member and enormous — Edinburgh has 17 for half a million people,
against 43 wards for two London boroughs — so a Scottish ward's median rank summarises data
zones spanning 88% of the national distribution. The number is arithmetically fine and
substantively meaningless. A Scottish analysis needs to join at data-zone or intermediate-
zone level instead, which means the alert payload would have to carry one, not just a ward.
The script still builds Scotland so the gap is measurable rather than assumed.

WHAT THIS IS NOT. Ward-level deprivation is a coarse proxy. The maintainers' reference council's
14 wards cover 53 LSOAs, so this compresses a 1,909-point scale onto 14 values. It is the resolution the alert
payload can join to, not the resolution the data has.

Usage:
  python3 scripts/build_deprivation_ward_index.py --nation wales --all
  python3 scripts/build_deprivation_ward_index.py --nation wales --lad <W06... code>
  python3 scripts/build_deprivation_ward_index.py --nation england --lad <E06/E07/E08/E09... code> <...>
  python3 scripts/build_deprivation_ward_index.py --nation scotland --lad <S12... code>
  python3 scripts/build_deprivation_ward_index.py --nation northern-ireland --all
"""
from __future__ import annotations

import argparse
import collections
import io
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DATA = os.path.join(ROOT, "data")

ONSPD = (
    "https://services1.arcgis.com/ESMARspQHYMw9BZ9/ArcGIS/rest/services/"
    "ONS_Postcode_Directory_(May_2026)_for_the_United_Kingdom_(Hosted_Table)"
    "/FeatureServer/0/query"
)
WARD_NAMES = (
    "https://services1.arcgis.com/ESMARspQHYMw9BZ9/ArcGIS/rest/services/"
    "WD_DEC_2023_UK_NC/FeatureServer/0/query"
)

#: Per-nation source of truth. `denominator` is a property of the PUBLISHED index, hardcoded
#: on purpose — deriving it from the rows we happen to load is the exact bug the column
#: exists to prevent.
NATIONS = {
    "wales": {
        "index": "WIMD 2019",
        "denominator": 1909,
        "out": "wimd_2019_wards.csv",
        "cache": "wimd-2019-ranks-by-lsoa.ods",
        "url": "https://www.gov.wales/sites/default/files/statistics-and-research/2022-02/"
               "welsh-index-multiple-deprivation-2019-index-and-domain-ranks-by-small-area.ods",
        "format": "ods",
        "sheet": "WIMD_2019_ranks",
        "code_col": 0,
        "rank_col": 3,
        "code_prefix": "W01",
        "ward_prefix": "W05",
    },
    "england": {
        "index": "IMD 2019",
        "denominator": 32844,
        "out": "imd_2019_wards.csv",
        "cache": "imd-2019-ranks-by-lsoa.xlsx",
        "url": "https://assets.publishing.service.gov.uk/media/5d8b3abded915d0373d3540f/"
               "File_1_-_IMD2019_Index_of_Multiple_Deprivation.xlsx",
        "format": "xlsx",
        "sheet": "IMD2019",
        "code_col": "A",
        "rank_col": "E",
        "code_prefix": "E01",
        "ward_prefix": "E05",
    },
    "scotland": {
        "index": "SIMD 2020v2",
        "denominator": 6976,
        "out": "simd_2020_wards.csv",
        "cache": "simd-2020v2-ranks-by-datazone.xlsx",
        "url": "https://www.gov.scot/binaries/content/documents/govscot/publications/statistics/"
               "2020/01/scottish-index-of-multiple-deprivation-2020-ranks-and-domain-ranks/"
               "documents/scottish-index-of-multiple-deprivation-2020-ranks-and-domain-ranks/"
               "scottish-index-of-multiple-deprivation-2020-ranks-and-domain-ranks/"
               "govscot%3Adocument/SIMD%2B2020v2%2B-%2Branks.xlsx",
        "format": "xlsx",
        "sheet": "SIMD 2020v2 ranks",
        "code_col": "A",
        "rank_col": "F",
        "code_prefix": "S01",
        "ward_prefix": "S13",
    },
    "northern-ireland": {
        "index": "NIMDM 2017 (Ward2014)",
        # NISRA ranks the 462 WARDS directly, so this denominator is a ward count while the
        # other three are small-area counts. Both are correct for their own file; the column
        # is what keeps them straight.
        "denominator": 462,
        "out": "nimdm_2017_wards.csv",
        "cache": "nimdm-2017-ward2014.xls",
        "url": "https://www.nisra.gov.uk/files/nisra/publications/NIMDM17_Ward2014.xls",
        "format": "xls-ward-direct",
        "sheet": "NIMDM 2017",
        "code_col": 2,
        "rank_col": 4,
        "ward_prefix": "N08",
    },
}

ODS_TABLE = "{urn:oasis:names:tc:opendocument:xmlns:table:1.0}"
ODS_TEXT = "{urn:oasis:names:tc:opendocument:xmlns:text:1.0}"
XL = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
XL_REL = "{http://schemas.openxmlformats.org/officeDocument/2006/relationships}"
PKG_REL = "{http://schemas.openxmlformats.org/package/2006/relationships}"


# ── downloads ──────────────────────────────────────────────────────────────────────────

def fetch(url: str, cache_name: str) -> str:
    path = os.path.join(DATA, cache_name)
    if os.path.exists(path):
        return path
    print(f"downloading {cache_name}…\n  {url}")
    with urllib.request.urlopen(url, timeout=300) as r:
        blob = r.read()
    os.makedirs(DATA, exist_ok=True)
    with open(path, "wb") as f:
        f.write(blob)
    return path


# ── spreadsheet readers (stdlib only: ODS and XLSX are both zip + XML) ─────────────────

def _ods_cells(row) -> list:
    out = []
    for cell in row.findall(ODS_TABLE + "table-cell"):
        repeat = int(cell.get(ODS_TABLE + "number-columns-repeated", "1"))
        text = "".join("".join(p.itertext()) for p in cell.findall(ODS_TEXT + "p"))
        out += [text] * min(repeat, 64)
    return out


def ods_rows(path: str, sheet: str):
    with zipfile.ZipFile(path) as z:
        root = ET.fromstring(z.read("content.xml"))
    for table in root.iter(ODS_TABLE + "table"):
        if table.get(ODS_TABLE + "name") != sheet:
            continue
        for row in table.findall(ODS_TABLE + "table-row"):
            yield _ods_cells(row)
        return
    sys.exit(f"error: sheet {sheet!r} not found in {os.path.basename(path)}")


def xlsx_rows(path: str, sheet: str):
    """Column-letter -> value, per row. No openpyxl: `scripts/` stays stdlib-only."""
    z = zipfile.ZipFile(path)
    shared = []
    if "xl/sharedStrings.xml" in z.namelist():
        for si in ET.fromstring(z.read("xl/sharedStrings.xml")).findall(XL + "si"):
            shared.append("".join(t.text or "" for t in si.iter(XL + "t")))
    workbook = ET.fromstring(z.read("xl/workbook.xml"))
    rels = ET.fromstring(z.read("xl/_rels/workbook.xml.rels"))
    by_id = {r.get("Id"): r.get("Target") for r in rels.findall(PKG_REL + "Relationship")}
    target = None
    for sh in workbook.findall(".//" + XL + "sheet"):
        if sh.get("name") == sheet:
            target = by_id.get(sh.get(XL_REL + "id"))
    if not target:
        sys.exit(f"error: sheet {sheet!r} not found in {os.path.basename(path)}")
    if not target.startswith("xl/"):
        target = "xl/" + target.lstrip("/")
    for row in ET.fromstring(z.read(target)).iter(XL + "row"):
        cells = {}
        for c in row.findall(XL + "c"):
            column = re.match(r"[A-Z]+", c.get("r") or "A").group(0)
            v = c.find(XL + "v")
            if c.get("t") == "inlineStr":
                cells[column] = "".join(t.text or "" for t in c.iter(XL + "t"))
            elif v is None:
                cells[column] = ""
            elif c.get("t") == "s":
                cells[column] = shared[int(v.text)]
            else:
                cells[column] = v.text
        yield cells


def xls_rows(path: str, sheet: str):
    """Legacy .xls, via a LibreOffice conversion.

    NISRA publishes NIMDM only as .xls, which no stdlib module reads. Converting is the
    smallest honest option; the alternative is a binary-format parser for one nation.
    """
    if not (shutil.which("soffice") or shutil.which("libreoffice")):
        sys.exit(
            "error: Northern Ireland's NIMDM is published only as legacy .xls, which needs\n"
            "       LibreOffice to convert. Install it, or convert by hand:\n"
            f"         soffice --headless --convert-to xlsx {path} --outdir {DATA}"
        )
    converted = path[: -len(".xls")] + ".xlsx"
    if not os.path.exists(converted):
        subprocess.run(
            [shutil.which("soffice") or "libreoffice", "--headless", "--convert-to", "xlsx",
             path, "--outdir", DATA],
            check=True, capture_output=True, timeout=300,
        )
    if not os.path.exists(converted):
        sys.exit(f"error: conversion produced no {os.path.basename(converted)}")
    for cells in xlsx_rows(converted, sheet):
        yield [cells.get(chr(ord("A") + i), "") for i in range(12)]


# ── small-area ranks ───────────────────────────────────────────────────────────────────

def load_small_area_ranks(spec: dict) -> dict:
    path = fetch(spec["url"], spec["cache"])
    ranks = {}
    if spec["format"] == "ods":
        for cells in ods_rows(path, spec["sheet"]):
            if len(cells) <= spec["rank_col"]:
                continue
            code = cells[spec["code_col"]].strip()
            if not code.startswith(spec["code_prefix"]):
                continue
            try:
                ranks[code] = int(float(cells[spec["rank_col"]]))
            except ValueError:
                continue
    else:
        for cells in xlsx_rows(path, spec["sheet"]):
            code = (cells.get(spec["code_col"]) or "").strip()
            if not code.startswith(spec["code_prefix"]):
                continue
            try:
                ranks[code] = int(float(cells.get(spec["rank_col"]) or ""))
            except ValueError:
                continue
    return ranks


def load_ward_ranks_direct(spec: dict) -> dict:
    """Northern Ireland: NISRA already ranks wards, so there is nothing to aggregate."""
    path = fetch(spec["url"], spec["cache"])
    ranks, names = {}, {}
    for cells in xls_rows(path, spec["sheet"]):
        code = str(cells[spec["code_col"]] or "").strip()
        if not code.startswith(spec["ward_prefix"]):
            continue
        try:
            ranks[code] = int(float(cells[spec["rank_col"]]))
        except (ValueError, TypeError):
            continue
        names[code] = str(cells[spec["code_col"] + 1] or "").strip()
    return ranks, names


# ── ONSPD bridge ───────────────────────────────────────────────────────────────────────

def fetch_ward_small_areas(lads: list, ward_prefix: str) -> dict:
    """`ward code -> {small-area code: live postcode count}` for the given local authorities."""
    wards = collections.defaultdict(collections.Counter)
    for lad in lads:
        offset, seen = 0, 0
        while True:
            query = urllib.parse.urlencode({
                "where": f"lad25cd='{lad}'",
                "outFields": "wd25cd,lsoa11cd,doterm",
                "returnGeometry": "false",
                "resultRecordCount": 2000,
                "resultOffset": offset,
                "f": "json",
            })
            with urllib.request.urlopen(f"{ONSPD}?{query}", timeout=300) as r:
                page = json.load(r)
            if "error" in page:
                sys.exit(f"error: ONSPD query failed for {lad}: {page['error']}")
            features = page.get("features", [])
            for feat in features:
                a = feat["attributes"]
                if a.get("doterm"):
                    continue
                ward, area = a.get("wd25cd"), a.get("lsoa11cd")
                if ward and area and ward.startswith(ward_prefix):
                    wards[ward][area] += 1
            seen += len(features)
            if not page.get("exceededTransferLimit") or not features:
                break
            offset += len(features)
        print(f"  {lad}: {seen} postcodes")
    return wards


def fetch_ward_names(wards: list) -> dict:
    """Ward names for the audit column only — never part of any join, never fatal."""
    names = {}
    for i in range(0, len(wards), 100):
        codes = ",".join(f"'{w}'" for w in wards[i : i + 100])
        query = urllib.parse.urlencode({
            "where": f"WD23CD IN ({codes})",
            "outFields": "WD23CD,WD23NM",
            "returnGeometry": "false",
            "resultRecordCount": 2000,
            "f": "json",
        })
        try:
            with urllib.request.urlopen(f"{WARD_NAMES}?{query}", timeout=180) as r:
                page = json.load(r)
            for feat in page.get("features", []):
                a = feat["attributes"]
                if a.get("WD23CD"):
                    names[a["WD23CD"]] = a.get("WD23NM") or ""
        except Exception:
            return names
    return names


# ── main ───────────────────────────────────────────────────────────────────────────────

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--nation", required=True, choices=sorted(NATIONS))
    parser.add_argument("--lad", nargs="*", default=[],
                        help="ONS local-authority codes to cover (W06..., E06-E09... or S12... codes)")
    parser.add_argument("--all", action="store_true",
                        help="every ward the index publishes (Northern Ireland and Wales only "
                             "— England and Scotland would page the whole national postcode "
                             "directory)")
    parser.add_argument("--out")
    args = parser.parse_args()

    spec = NATIONS[args.nation]
    out = args.out or os.path.join(DATA, spec["out"])
    denominator = spec["denominator"]
    print(f"{args.nation}: {spec['index']}, denominator {denominator}")

    if args.nation == "northern-ireland":
        # NISRA publishes ward ranks directly: no bridge, no aggregation, no LAD selection.
        ranks, names = load_ward_ranks_direct(spec)
        if not ranks:
            sys.exit("error: no ward ranks parsed — refusing to write an empty index")
        rows = [(w, names.get(w, ""), r, denominator, 1, r, r) for w, r in sorted(ranks.items())]
        note = "NISRA ranks wards directly; LSOA Count is 1 and the spread is the rank itself"
    else:
        if not args.lad and not args.all:
            remedy = (
                "use --all for every Welsh ward, or --lad <codes> to scope it"
                if args.nation == "wales"
                else "name them with --lad <codes> (--all is not available for this nation, "
                     "since it would page the entire national postcode directory)"
            )
            sys.exit(
                f"error: --nation {args.nation} needs the local authorities to cover: "
                f"{remedy}.\n"
                f"       An analysis is ward-scoped, so there is no default."
            )
        if args.all and args.nation != "wales":
            sys.exit(
                f"error: --all is only supported for wales and northern-ireland. For "
                f"{args.nation}, name the local authorities with --lad."
            )
        ranks = load_small_area_ranks(spec)
        print(f"{spec['index']}: {len(ranks)} small areas, "
              f"ranks {min(ranks.values())}..{max(ranks.values())}")
        if max(ranks.values()) != denominator:
            print(f"warning: the published index tops out at {max(ranks.values())}, not the "
                  f"{denominator} this script declares — check for a new edition", file=sys.stderr)

        lads = args.lad
        if args.all:
            lads = sorted({f"W06{n:06d}" for n in range(1, 25)} - {"W06000007", "W06000017"})
        print(f"ONSPD: resolving wards for {len(lads)} local authority/authorities…")
        wards = fetch_ward_small_areas(lads, spec["ward_prefix"])
        if not wards:
            sys.exit("error: no wards resolved — refusing to write an empty index")
        names = fetch_ward_names(sorted(wards))

        rows, missing = [], []
        for ward in sorted(wards):
            area_ranks = sorted(ranks[a] for a in wards[ward] if a in ranks)
            missing += [a for a in wards[ward] if a not in ranks]
            if not area_ranks:
                continue
            # Median, not mean: a deprivation rank is ordinal (see the module docstring).
            rows.append((ward, names.get(ward, ""), int(statistics.median(area_ranks)),
                         denominator, len(area_ranks), area_ranks[0], area_ranks[-1]))
        note = (f"{len(set(missing))} small area(s) had no rank and were excluded from their "
                f"ward's median") if missing else ""

    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        f.write("Ward Code,Ward Name,IMD Rank,IMD Denominator,LSOA Count,Rank Min,Rank Max\n")
        for ward, name, rank, denom, count, lo, hi in rows:
            f.write(f"{ward},{name.replace(',', ' ')},{rank},{denom},{count},{lo},{hi}\n")

    print(f"\nwrote {len(rows)} wards -> {os.path.relpath(out, ROOT)}")
    if note:
        print(f"note: {note}")
    widest = max(rows, key=lambda r: r[6] - r[5])
    print(f"widest ward spread: {widest[0]} {widest[1]} ranks {widest[5]}..{widest[6]} "
          f"over {widest[4]} area(s) (median {widest[2]})")
    print(f"\nOne nation per file — never merge this with another nation's index; their ranks "
          f"share no scale and the loader rejects it.")
    print(f"Point the detector at it:  [detector] imd_csv_path = \"{os.path.relpath(out, ROOT)}\"")
    return 0


if __name__ == "__main__":
    sys.exit(main())
