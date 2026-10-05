#!/usr/bin/env python3
"""Remove isolated (degree-zero) nodes from a GraphML file.

WHY. `scripts/export_graphml.sh` dumps the whole graph, including every node that no edge
touches — placeholder entities, unlinked property records, officers whose only edge lives in
a different analysis. Those dominate the node count in a layout tool and push large exports
past what most viewers will open at all.

STREAMING, IN TWO PASSES. The exports run to hundreds of megabytes (252 MB / ~872k elements
at the time of writing), so this never builds a DOM. Pass 1 reads only `<edge>` elements and
records which node ids are referenced. Pass 2 re-reads the file and copies it through,
dropping `<node>` elements whose id was never referenced, freeing each element as it goes.
Peak memory is the id set, not the document.

FIDELITY. The output is the input minus the dropped nodes: the same `<key>` declarations, the
same `<graph>` attributes, the same edges in the same order, and every `<data>` child
preserved verbatim. Namespace declarations on the root are reproduced, so the result loads in
Gephi / yEd / networkx exactly as the original does.

SELF-LOOPS. A node whose only edge points at itself is not connected to anything else, but
dropping it would leave that edge dangling and make the file invalid. Default is therefore to
KEEP such nodes; `--prune-self-loops` drops them together with their self-loop edges.

DANGLING EDGES. An edge referencing an id that no `<node>` declares is reported, and dropped
only if you ask (`--drop-dangling-edges`). Dropping them can isolate further nodes, so the
run tells you whether a second pass would remove more rather than quietly iterating.

Usage:
  python3 scripts/prune_isolated_nodes.py graph.graphml                  # -> graph-connected.graphml
  python3 scripts/prune_isolated_nodes.py graph.graphml -o out.graphml
  python3 scripts/prune_isolated_nodes.py graph.graphml --dry-run        # count only, write nothing
  python3 scripts/prune_isolated_nodes.py graph.graphml --prune-self-loops
"""
import argparse
import os
import sys
import xml.etree.ElementTree as ET
from xml.sax.saxutils import escape, quoteattr

GRAPHML_NS = "http://graphml.graphdrawing.org/xmlns"


def localname(tag):
    """`{uri}node` -> `node`. ElementTree expands every name; we write local names and let
    the root's default namespace cover them."""
    return tag.rpartition("}")[2]


def prefixed(name, prefixes):
    """Render an expanded attribute name back as `prefix:local` for output."""
    if not name.startswith("{"):
        return name
    uri, _, local = name[1:].partition("}")
    prefix = prefixes.get(uri)
    # An attribute in the default namespace still needs no prefix; an unknown URI keeps its
    # expanded form rather than being silently mangled into a wrong name.
    if prefix is None:
        return f"{{{uri}}}{local}"
    return local if prefix == "" else f"{prefix}:{local}"


def write_element(out, elem, prefixes, indent):
    """Serialise one element and its subtree. Children are `<data>` nodes in practice."""
    tag = localname(elem.tag)
    parts = [f"{indent}<{tag}"]
    for k, v in elem.attrib.items():
        parts.append(f" {prefixed(k, prefixes)}={quoteattr(v)}")
    children = list(elem)
    text = (elem.text or "").strip()
    if not children and not text:
        parts.append("/>\n")
        out.write("".join(parts))
        return
    parts.append(">")
    if children:
        out.write("".join(parts) + "\n")
        for child in children:
            write_element(out, child, prefixes, indent + "  ")
        out.write(f"{indent}</{tag}>\n")
    else:
        out.write("".join(parts) + escape(text) + f"</{tag}>\n")


def collect_referenced(path, verbose=True):
    """Pass 1 — ids that any edge touches, ids that a `<node>` declares, and self-loop
    bookkeeping.

    Declared ids are collected here rather than during the copy because "dangling" must be
    judged against what the document DECLARES, not against what its edges mention: an id is
    dangling precisely when an edge names it and no node defines it, so testing against the
    referenced set would compare a set to itself and never fire. GraphML also does not
    require nodes to precede edges, so the answer has to exist before the copy starts.
    """
    referenced, declared, self_looped = set(), set(), set()
    edges = hyperedges = 0
    non_self = set()
    for _event, elem in ET.iterparse(path, events=("end",)):
        tag = localname(elem.tag)
        if tag == "node":
            declared.add(elem.get("id"))
            elem.clear()
        elif tag == "edge":
            src, dst = elem.get("source"), elem.get("target")
            edges += 1
            if src is not None and dst is not None:
                referenced.add(src)
                referenced.add(dst)
                if src == dst:
                    self_looped.add(src)
                else:
                    non_self.add(src)
                    non_self.add(dst)
            elem.clear()
            if verbose and edges % 200_000 == 0:
                print(f"  pass 1: {edges:,} edges", flush=True)
        elif tag == "hyperedge":
            hyperedges += 1
            elem.clear()
    # Nodes kept alive ONLY by a self-loop, i.e. connected to nothing else.
    self_only = self_looped - non_self
    return referenced, declared, self_only, edges, hyperedges


def main():
    ap = argparse.ArgumentParser(description="Remove nodes with no edges from a GraphML file.")
    ap.add_argument("input")
    ap.add_argument("-o", "--output", help="default: <input stem>-connected.graphml")
    ap.add_argument("--dry-run", action="store_true", help="report counts, write nothing")
    ap.add_argument("--prune-self-loops", action="store_true",
                    help="also drop nodes whose only edge is a self-loop (and that edge)")
    ap.add_argument("--drop-dangling-edges", action="store_true",
                    help="drop edges pointing at ids no <node> declares")
    ap.add_argument("-q", "--quiet", action="store_true")
    args = ap.parse_args()

    if not os.path.isfile(args.input):
        sys.exit(f"not a file: {args.input}")
    out_path = args.output
    if not out_path:
        stem = args.input[:-8] if args.input.endswith(".graphml") else args.input
        out_path = f"{stem}-connected.graphml"
    if os.path.abspath(out_path) == os.path.abspath(args.input):
        sys.exit("refusing to overwrite the input file — pass a different --output")

    verbose = not args.quiet
    size_mb = os.path.getsize(args.input) / 1e6
    if verbose:
        print(f"reading {args.input} ({size_mb:.1f} MB)")

    referenced, declared, self_only, edge_count, hyperedges = collect_referenced(
        args.input, verbose)
    if hyperedges:
        print(f"  WARNING: {hyperedges} <hyperedge> element(s) — their endpoints are NOT "
              f"counted as connections, so nodes held only by one may be dropped.",
              file=sys.stderr)

    keep = referenced - self_only if args.prune_self_loops else referenced
    if verbose:
        print(f"  pass 1 done: {edge_count:,} edges, {len(declared):,} node(s) declared, "
              f"{len(referenced):,} referenced id(s), {len(self_only):,} self-loop-only")

    prefixes, root_written, graph_el = {}, False, None
    kept = dropped = total_nodes = edges_written = edges_dropped = 0

    out = open(os.devnull if args.dry_run else out_path, "w", encoding="utf-8")
    try:
        out.write('<?xml version="1.0" encoding="UTF-8"?>\n')
        context = ET.iterparse(args.input, events=("start-ns", "start", "end"))
        for event, elem in context:
            if event == "start-ns":
                prefix, uri = elem
                prefixes[uri] = prefix
                continue

            tag = localname(elem.tag) if not isinstance(elem, tuple) else ""

            if event == "start":
                if not root_written and tag == "graphml":
                    attrs = "".join(
                        f' {prefixed(k, prefixes)}={quoteattr(v)}' for k, v in elem.attrib.items())
                    ns = "".join(
                        f' xmlns={quoteattr(u)}' if p == "" else f' xmlns:{p}={quoteattr(u)}'
                        for u, p in prefixes.items())
                    out.write(f"<graphml{ns}{attrs}>\n")
                    root_written = True
                elif tag == "graph":
                    graph_el = elem
                    attrs = "".join(
                        f' {prefixed(k, prefixes)}={quoteattr(v)}' for k, v in elem.attrib.items())
                    out.write(f"  <graph{attrs}>\n")
                continue

            # event == "end"
            if tag == "key":
                write_element(out, elem, prefixes, "  ")
                elem.clear()
            elif tag == "node":
                total_nodes += 1
                node_id = elem.get("id")
                if node_id in keep:
                    write_element(out, elem, prefixes, "    ")
                    kept += 1
                else:
                    dropped += 1
                elem.clear()
                if graph_el is not None:
                    del graph_el[:]          # release the shell, keep <graph>'s attributes
                if verbose and total_nodes % 200_000 == 0:
                    print(f"  pass 2: {total_nodes:,} nodes", flush=True)
            elif tag == "edge":
                src, dst = elem.get("source"), elem.get("target")
                drop = args.prune_self_loops and src == dst and src in self_only
                if not drop and args.drop_dangling_edges:
                    # Judged against DECLARED nodes: an edge is dangling when it names an id
                    # no <node> defines. Testing against `keep` (the referenced set) would
                    # compare the set to itself and never drop anything.
                    drop = src not in declared or dst not in declared
                if drop:
                    edges_dropped += 1
                else:
                    write_element(out, elem, prefixes, "    ")
                    edges_written += 1
                elem.clear()
                if graph_el is not None:
                    del graph_el[:]
            elif tag == "graph":
                out.write("  </graph>\n")
            elif tag == "graphml":
                out.write("</graphml>\n")
    finally:
        out.close()

    dangling = referenced - declared
    print(f"\nnodes   : {total_nodes:,} read, {kept:,} kept, {dropped:,} removed "
          f"({100.0 * dropped / max(total_nodes, 1):.1f}% isolated)")
    print(f"edges   : {edge_count:,} read, {edges_written:,} kept, {edges_dropped:,} removed")
    if self_only:
        verb = "removed" if args.prune_self_loops else "kept (use --prune-self-loops to drop)"
        print(f"self-loop-only nodes: {len(self_only):,} — {verb}")
    if dangling:
        print(f"WARNING: {len(dangling):,} id(s) are referenced by an edge but never declared "
              f"as a <node>. The input was already inconsistent.", file=sys.stderr)
        for d in sorted(dangling)[:5]:
            print(f"    {d}", file=sys.stderr)
        if args.drop_dangling_edges:
            print("  Their edges were dropped, which may have isolated further nodes — "
                  "re-run on the output to remove those too.", file=sys.stderr)

    if args.dry_run:
        print("\nDRY RUN — nothing written.")
    else:
        new_mb = os.path.getsize(out_path) / 1e6
        print(f"\nwritten: {out_path} ({new_mb:.1f} MB, "
              f"{100.0 * (1 - new_mb / max(size_mb, 1e-9)):.1f}% smaller)")


if __name__ == "__main__":
    main()
