#!/usr/bin/env bash
#
# export_parquet.sh — export the live UrbanIntegrity graph (read from the
# `graph_events` WAL in SurrealDB) to timestamped Parquet files.
#
# Output: 
#   <repo-root>/urban-integrity-<YYYYMMDD-HHMMSS>.graphml
#   <repo-root>/urban-integrity-nodes-<YYYYMMDD-HHMMSS>.parquet
#   <repo-root>/urban-integrity-links-<YYYYMMDD-HHMMSS>.parquet
#
# This uses the `export-graphml` CLI subcommand, and then converts the output
# into two Parquet files suitable for Cosmograph (https://cosmograph.app/).
#
# Usage:
#   scripts/export_parquet.sh            # build (if needed) + export
#   scripts/export_parquet.sh --debug    # use the debug build instead of release
#
# Env:
#   URBAN_CLI   override the CLI binary path (skips cargo build entirely)
#
set -euo pipefail

# ── Resolve repo root from this script's location (robust to CWD) ────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Parse args ───────────────────────────────────────────────────────────────
PROFILE="release"
CARGO_PROFILE_FLAG="--release"
for arg in "$@"; do
  case "$arg" in
    --debug)
      PROFILE="debug"
      CARGO_PROFILE_FLAG=""
      ;;
    -h|--help)
      sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

# ── Timestamped output paths ─────────────────────────────────────────────────
STAMP="$(date +%Y%m%d-%H%M%S)"
GRAPHML_OUT="$REPO_ROOT/urban-integrity-${STAMP}.graphml"
NODES_OUT="$REPO_ROOT/urban-integrity-nodes-${STAMP}.parquet"
LINKS_OUT="$REPO_ROOT/urban-integrity-links-${STAMP}.parquet"

# ── Locate or build the CLI binary ───────────────────────────────────────────
BIN="${URBAN_CLI:-$REPO_ROOT/target/$PROFILE/urban-integrity-cli}"

if [[ -n "${URBAN_CLI:-}" ]]; then
  echo "Using CLI from URBAN_CLI=$BIN"
elif [[ -x "$BIN" ]]; then
  echo "Using existing $PROFILE build: $BIN"
else
  echo "Building urban-integrity-cli ($PROFILE)…"
  # shellcheck disable=SC2086
  cargo build -p urban-integrity-cli $CARGO_PROFILE_FLAG
fi

if [[ ! -x "$BIN" ]]; then
  echo "error: CLI binary not found at $BIN" >&2
  exit 1
fi

# ── Export ───────────────────────────────────────────────────────────────────
echo "Exporting graph_events → GraphML…"
"$BIN" export-graphml --source events --output "$GRAPHML_OUT"

echo "Converting GraphML → Parquet (Nodes & Links)…"
python3 -c "
import sys
import pandas as pd
import xml.etree.ElementTree as ET

def parse_graphml(filepath):
    nodes = []
    edges = []
    
    context = ET.iterparse(filepath, events=('end',))
    
    def strip_ns(tag):
        return tag.split('}')[-1] if '}' in tag else tag
        
    for event, elem in context:
        tag = strip_ns(elem.tag)
        if tag == 'node':
            node_data = {'id': elem.attrib.get('id')}
            for data in elem:
                if strip_ns(data.tag) == 'data':
                    key = data.attrib.get('key')
                    if key and key.startswith('n_'):
                        key = key[2:]
                    node_data[key] = data.text
            nodes.append(node_data)
            elem.clear()
        elif tag == 'edge':
            edge_data = {
                'source': elem.attrib.get('source'),
                'target': elem.attrib.get('target'),
                'id': elem.attrib.get('id')
            }
            for data in elem:
                if strip_ns(data.tag) == 'data':
                    key = data.attrib.get('key')
                    if key and key.startswith('e_'):
                        key = key[2:]
                    edge_data[key] = data.text
            edges.append(edge_data)
            elem.clear()
            
    return nodes, edges

try:
    nodes, edges = parse_graphml('$GRAPHML_OUT')
    
    from collections import defaultdict, deque
    
    # 1. Build adjacency list (undirected for reachability)
    adj = defaultdict(list)
    for e in edges:
        u, v = e.get('source'), e.get('target')
        if u and v:
            adj[u].append(v)
            adj[v].append(u)
            
    try:
        import re
        # config.local.toml replaces config.toml when present (AppConfig::load()'s choice).
        import os
        cfg_path = 'config.local.toml' if os.path.isfile('config.local.toml') else 'config.toml'
        with open(cfg_path, 'r') as f:
            cfg_text = f.read()
        target_codes = set(re.findall(r'(?:lpa_code|ward_code)\s*=\s*\x22([^\x22]+)\x22', cfg_text))
    except Exception:
        target_codes = set()
        
    # 2. Find all nodes matching the target config codes
    # If a node ID contains the lpa_code or ward_code, it is a seed.
    seed_nodes = [n['id'] for n in nodes if 'id' in n and any(c in n['id'] for c in target_codes)]
    
    # 3. BFS to find all nodes in components containing at least one seed
    visited = set(seed_nodes)
    queue = deque(seed_nodes)
    
    while queue:
        curr = queue.popleft()
        for neighbor in adj[curr]:
            if neighbor not in visited:
                visited.add(neighbor)
                queue.append(neighbor)
                
    # 4. Filter nodes and edges
    nodes = [n for n in nodes if n.get('id') in visited]
    edges = [e for e in edges if e.get('source') in visited and e.get('target') in visited]
    
    print(f'Filtered graph to {len(nodes)} nodes and {len(edges)} links connected to config.toml target codes.')
    
    if nodes:
        pd.DataFrame(nodes).to_parquet('$NODES_OUT', engine='pyarrow', index=False)
    else:
        pd.DataFrame(columns=['id']).to_parquet('$NODES_OUT', engine='pyarrow', index=False)
        
    if edges:
        pd.DataFrame(edges).to_parquet('$LINKS_OUT', engine='pyarrow', index=False)
    else:
        pd.DataFrame(columns=['id', 'source', 'target']).to_parquet('$LINKS_OUT', engine='pyarrow', index=False)
        
    print('Parquet conversion succeeded.')
except Exception as e:
    print(f'Error during conversion: {e}', file=sys.stderr)
    sys.exit(1)
"

if [[ -f "$NODES_OUT" ]] && [[ -f "$LINKS_OUT" ]] && [[ -f "$GRAPHML_OUT" ]]; then
  NODES_SIZE="$(du -h "$NODES_OUT" | cut -f1)"
  LINKS_SIZE="$(du -h "$LINKS_OUT" | cut -f1)"
  GRAPHML_SIZE="$(du -h "$GRAPHML_OUT" | cut -f1)"
  echo "✓ Export complete:"
  echo "  GraphML: $GRAPHML_OUT ($GRAPHML_SIZE)"
  echo "  Nodes:   $NODES_OUT ($NODES_SIZE)"
  echo "  Links:   $LINKS_OUT ($LINKS_SIZE)"
else
  echo "error: expected output files were not created." >&2
  exit 1
fi
