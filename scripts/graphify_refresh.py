#!/usr/bin/env python3
from __future__ import annotations

import json
import sys
from pathlib import Path


def rebuild_code_no_viz_fallback(watch_path: Path) -> int:
    watch_path = watch_path.resolve()

    from graphify.analyze import god_nodes, surprising_connections, suggest_questions
    from graphify.build import build_from_json
    from graphify.cluster import cluster, score_all
    from graphify.detect import detect
    from graphify.export import to_html, to_json
    from graphify.extract import extract
    from graphify.report import generate

    detected = detect(watch_path, follow_symlinks=False)
    ignored_parts = {"node_modules", "dist", "target", ".venv-graphifyy", "graphify-out", ".git"}
    code_files = [
        Path(f)
        for f in detected["files"]["code"]
        if ignored_parts.isdisjoint(Path(f).parts)
    ]

    if not code_files:
        print("[graphify refresh] No code files found - nothing to rebuild.")
        return 1

    result = extract(code_files, cache_root=watch_path)

    out = watch_path / "graphify-out"
    existing_graph = out / "graph.json"
    if existing_graph.exists():
        try:
            existing = json.loads(existing_graph.read_text(encoding="utf-8"))
            code_ids = {
                node["id"]
                for node in existing.get("nodes", [])
                if node.get("file_type") == "code"
            }
            sem_nodes = [
                node
                for node in existing.get("nodes", [])
                if node.get("file_type") != "code"
            ]
            sem_edges = [
                edge
                for edge in existing.get("links", existing.get("edges", []))
                if edge.get("confidence") in ("INFERRED", "AMBIGUOUS")
                or (
                    edge.get("source") not in code_ids
                    and edge.get("target") not in code_ids
                )
            ]
            result = {
                "nodes": result["nodes"] + sem_nodes,
                "edges": result["edges"] + sem_edges,
                "hyperedges": existing.get("hyperedges", []),
                "input_tokens": 0,
                "output_tokens": 0,
            }
        except Exception:
            pass

    detection = {
        "files": {
            "code": [str(f) for f in code_files],
            "document": [],
            "paper": [],
            "image": [],
        },
        "total_files": len(code_files),
        "total_words": detected.get("total_words", 0),
    }

    graph = build_from_json(result)
    communities = cluster(graph)
    cohesion = score_all(graph, communities)
    gods = god_nodes(graph)
    surprises = surprising_connections(graph, communities)
    labels = {cid: f"Community {cid}" for cid in communities}
    questions = suggest_questions(graph, communities, labels)

    out.mkdir(exist_ok=True)

    report = generate(
        graph,
        communities,
        cohesion,
        labels,
        gods,
        surprises,
        detection,
        {"input": 0, "output": 0},
        str(watch_path),
        suggested_questions=questions,
    )
    (out / "GRAPH_REPORT.md").write_text(report, encoding="utf-8")
    to_json(graph, communities, str(out / "graph.json"))

    html_written = False
    try:
        to_html(graph, communities, str(out / "graph.html"), community_labels=labels or None)
        html_written = True
    except ValueError as exc:
        if "too large for HTML viz" not in str(exc):
            raise
        stale_html = out / "graph.html"
        if stale_html.exists():
            stale_html.unlink()
        print(
            "[graphify refresh] Skipped graph.html because the corpus is too large for HTML visualization."
        )

    flag = out / "needs_update"
    if flag.exists():
        flag.unlink()

    print(
        f"[graphify refresh] Rebuilt: {graph.number_of_nodes()} nodes, {graph.number_of_edges()} edges, {len(communities)} communities"
    )
    if html_written:
        print(
            f"[graphify refresh] graph.json, graph.html and GRAPH_REPORT.md updated in {out}"
        )
    else:
        print(f"[graphify refresh] graph.json and GRAPH_REPORT.md updated in {out}")
    return 0


def main() -> int:
    watch_path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".")
    if not watch_path.exists():
        print(f"graphify refresh: path not found: {watch_path}", file=sys.stderr)
        return 1
    return rebuild_code_no_viz_fallback(watch_path)


if __name__ == "__main__":
    raise SystemExit(main())
