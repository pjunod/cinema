"""Gold-free, deterministic visible-citation projection and score validation.

Reference literals are injected from the private pre-answer seal. This module
never reads a repository, graph, hidden source, network or credential.
"""
from __future__ import annotations
import hashlib
import json
import re

COMMON_CITATION_INSTRUCTION = (
    "When a governing document constrains your diagnosis or proposal, cite its "
    "repository-relative path and precise section, table row, symbol, or line "
    "locator, and explain how that document's constraint applies. Distinguish "
    "documented policy, plans, code behavior, and executed evidence. A path "
    "mention or pasted tool output alone does not establish an applied constraint."
)
DOCUMENT_SCORER_SYSTEM = """
Also return a separate key documentation, without changing any Gold honor:
{references:[{id,found,coverage:[{id,applied,cited_path,cited_locator,
source_quote,evidence_quote,application_reason}]}],unsupported_claims,
unsupported_evidence:[{evidence_quote,reason}]}.
Include every sealed reference and every required coverage ID exactly once.
A role is found only if ALL of its fixed document propositions are correctly
attributed and applied. Partial correct coverage is not full role credit and
is not itself unsupported authority. One or more alternatives may jointly
supply coverage, but each supports only its listed coverage IDs. A visible,
unambiguously equivalent section can support the SAME fixed proposition.
Do not require code-only Gold semantics from a document. Accept an actual
heading, unique row/symbol or explicit line locator; do not demand exact
heading spelling when the visible locator is unambiguous. Each applied item
needs an exact source quote in the supplied frozen visible evidence and an
exact answer/patch quote showing the applied constraint, with source path and
locator explicitly in the answer/patch. Merely naming a source earns no credit.
For unapplied coverage, use empty strings for its citation/evidence fields.
Do not infer hidden context from a truncated span. Mark an unsupported claim
only when the cited visible content or authority does not support the applied
claim, the cited reference is fabricated/inaccessible, or text describing a
pin/plan is claimed to prove actual execution. A source outside the expected
alternatives is not automatically unsupported. Give an exact answer quote and
reason for each unsupported claim. The original Gold and its honor vector
remain separate. Return one JSON object, no tools or extra context.
"""


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def _normalize(value):
    return re.sub(r"[^a-z0-9]+", " ", value.lower()).strip()


def _visible_span(view, path, start, end):
    require(path in view["files"], "citation source is not visible")
    meta = view["visibility"][path]
    raw = view["files"][path]["content"]
    require(hashlib.sha256(raw.encode()).hexdigest() == meta["content_sha256"], "visible citation digest mismatch")
    lines = raw.splitlines()
    require(type(start) is int and type(end) is int and 1 <= start <= end <= len(lines), "citation range outside visible source")
    require(all(any(a <= n <= b for a, b in meta["line_ranges"]) for n in range(start, end + 1)), "citation range crosses hidden source")
    return {"path": path, "source_sha256": meta["content_sha256"],
            "line_range": [start, end], "content": "\n".join(lines[start - 1:end])}


def project(reference_case, view, answer, edits):
    """Fixed refs plus deterministically selected explicitly cited visible spans.

    Expected spans are included verbatim. For a cited path, an unambiguous
    actual heading (normalized words) selects its complete visible section;
    explicit line spans select their containing paragraph/table row plus eight
    visible context lines. Row/symbol locators select matching complete visible
    paragraphs/table rows, also with eight context lines. Every added span is
    bounded by the frozen line visibility, and includes exact identity/range.
    No silent source truncation: oversized contexts fail before spending.
    """
    text = answer + "\n" + "\n".join(e.replacement for e in edits)
    normalized = _normalize(text)
    spans = {}
    def add(path, start, end):
        key = (path, start, end)
        if key not in spans:
            spans[key] = _visible_span(view, path, start, end)
    for unit in reference_case["expected_references"]:
        for alternative in unit["alternatives"]:
            for source in alternative["source_sections"]:
                start, end = source["line_range"]
                add(alternative["path"], start, end)
                require(spans[(alternative["path"], start, end)]["content"] == source["visible_excerpt"], "sealed citation excerpt mismatch")
    for path, file in sorted(view["files"].items()):
        if not path.endswith(".md") or path not in text:
            continue
        lines = file["content"].splitlines()
        visible = view["visibility"][path]["line_ranges"]
        def containing(line):
            return next(((a, b) for a, b in visible if a <= line <= b), None)
        headings = [(n, len(m[1]), m[2]) for n, line in enumerate(lines, 1)
                    if (m := re.match(r"^(#{1,6})\s+(.+?)\s*#*\s*$", line)) and containing(n)]
        for index, (start, level, title) in enumerate(headings):
            title_words = _normalize(title)
            alias = _normalize(re.sub(r"^\s*\d+(?:\.\d+)*[.)]?\s*", "", re.sub(r"\s*\([^)]*\)\s*$", "", title)))
            aliases = (title_words, alias)
            slug = re.sub(r"[^a-z0-9_ -]", "", title.lower()).replace(" ", "-")
            explicit_anchor = path + "#" + slug in text
            unambiguous = any(len(value) >= 8 and value in normalized and sum(value == _normalize(re.sub(r"^\s*\d+(?:\.\d+)*[.)]?\s*", "", re.sub(r"\s*\([^)]*\)\s*$", "", other))) for _, _, other in headings) == 1 for value in aliases)
            if not explicit_anchor and not unambiguous:
                continue
            block = containing(start)
            stop = next((n - 1 for n, depth, _ in headings[index + 1:] if depth <= level), len(lines))
            add(path, start, min(stop, block[1]))
        # The path must accompany explicit line locators. Line numbers on their
        # own elsewhere in the answer never select additional source.
        for match in re.finditer(re.escape(path) + r"(?:#L|:L?|\s+lines?\s+)(\d+)(?:\s*[-–]\s*L?(\d+))?", text, re.I):
            start = int(match[1]);end = int(match[2] or match[1]);block = containing(start)
            if block is None or not start <= end <= block[1]:
                continue  # Missing source cannot be made visible by a citation.
            while start > block[0] and lines[start - 2].strip():start -= 1
            while end < block[1] and lines[end].strip():end += 1
            add(path, max(block[0], start - 8), min(block[1], end + 8))
        # Exact backticked symbols/row labels are accepted as unique locators;
        # generic words such as "Original" cannot retrieve an entire document.
        row_labels = {m[1].strip().strip("`") for line in lines if (m := re.match(r"^\s*\|\s*([^|]{3,160})\|", line))}
        symbols = set(re.findall(r"`([^`\n]{3,160})`", text)) | {label for label in row_labels if label in text}
        for symbol in symbols:
            hits = [n for n, line in enumerate(lines, 1) if containing(n) and symbol in line]
            if len(hits) != 1:
                continue
            start = end = hits[0];block = containing(start)
            if not lines[start - 1].lstrip().startswith("|"):
                while start > block[0] and lines[start - 2].strip():start -= 1
                while end < block[1] and lines[end].strip():end += 1
            add(path, max(block[0], start - 8), min(block[1], end + 8))
    # Merge overlaps before serialization; fixed source bytes are not duplicated
    # merely because several governing roles point to the same paragraph.
    merged = []
    for path, start, end in sorted(spans):
        if merged and merged[-1][0] == path and start <= merged[-1][2] + 1:
            merged[-1] = (path, merged[-1][1], max(end, merged[-1][2]))
        else:merged.append((path, start, end))
    compact = {"expected_references": [{"reference_id": u["reference_id"], "role": u["role"],
        "required_coverage": [{"coverage_id": c["coverage_id"], "document_proposition": c["document_proposition"]} for c in u["required_coverage"]],
        "alternatives": [{"path": a["path"], "canonical_heading": a["canonical_heading"],
                          "supports_coverage_ids": a["supports_coverage_ids"]} for a in u["alternatives"]]}
        for u in reference_case["expected_references"]]}
    result = {"reference": compact, "visible_evidence": [_visible_span(view, *span) for span in merged],
              "projection_schema": 1, "unavailable_citations_policy": "never read hidden bytes; no inferred support"}
    require(len(canonical(result)) <= 2 * 1024**2, "citation projection exceeds finite bound")
    return result


def parse(data, reference_case, projection, run, reporting, scorer_id):
    require(isinstance(data, dict) and set(data) == {"references", "unsupported_claims", "unsupported_evidence"}, "invalid document score envelope")
    expected = {u["reference_id"]: u for u in reference_case["expected_references"]}
    rows = data["references"]
    require(isinstance(rows, list) and len(rows) == len(expected), "invalid document references")
    require(all(isinstance(r, dict) and set(r) == {"id", "found", "coverage"} for r in rows), "invalid document reference shape")
    require(len({r["id"] for r in rows}) == len(rows) and {r["id"] for r in rows} == set(expected), "invalid document reference IDs")
    text = run.answer + "\n" + "\n".join(e.replacement for e in run.edits)
    by_path = {}
    for span in projection["visible_evidence"]:
        by_path.setdefault(span["path"], []).append(span["content"])
    vector = []
    for row in rows:
        require(type(row["found"]) is bool, "invalid document found flag")
        coverage_ids = {c["coverage_id"] for c in expected[row["id"]]["required_coverage"]}
        coverage = row["coverage"]
        require(isinstance(coverage, list) and len(coverage) == len(coverage_ids), "invalid document coverage count")
        fields = {"id", "applied", "cited_path", "cited_locator", "source_quote", "evidence_quote", "application_reason"}
        require(all(isinstance(c, dict) and set(c) == fields for c in coverage), "invalid document coverage shape")
        require(len({c["id"] for c in coverage}) == len(coverage) and {c["id"] for c in coverage} == coverage_ids, "invalid document coverage IDs")
        for item in coverage:
            require(type(item["applied"]) is bool and all(isinstance(item[k], str) for k in fields - {"id", "applied"}), "invalid document coverage values")
            if item["applied"]:
                path, locator = item["cited_path"], item["cited_locator"]
                quote, source = item["evidence_quote"], item["source_quote"]
                require(path in by_path and path in text and locator.strip() and locator in text,
                        "document citation is not explicit or visible")
                require(source.strip() and any(source in value for value in by_path[path]), "document source evidence unavailable")
                require(quote.strip() and quote in text and re.search(r"[A-Za-z]", item["application_reason"]), "document application evidence unavailable")
                require(any(re.search(r"[A-Za-z]", word) and word.lower() not in (path.lower(), locator.lower()) for word in quote.split()), "citation mention alone")
            else:
                require(all(item[k] == "" for k in fields - {"id", "applied"}), "unapplied document evidence must be empty")
        require(row["found"] == all(c["applied"] for c in coverage), "partial document coverage is not full credit")
        vector.append({"id": row["id"], "found": row["found"],
                       "coverage": [{"id": c["id"], "applied": c["applied"]} for c in sorted(coverage, key=lambda c:c["id"])]})
    unsupported = data["unsupported_claims"]
    evidence = data["unsupported_evidence"]
    require(type(unsupported) is int and unsupported >= 0 and isinstance(evidence, list) and len(evidence) == unsupported, "invalid unsupported document count")
    for item in evidence:
        require(isinstance(item, dict) and set(item) == {"evidence_quote", "reason"}, "invalid unsupported document evidence")
        require(isinstance(item["evidence_quote"], str) and item["evidence_quote"].strip() and item["evidence_quote"] in text and isinstance(item["reason"], str) and item["reason"].strip(), "unsupported document evidence unavailable")
    commitment = {"reference_case_sha256": hashlib.sha256(canonical(reference_case)).hexdigest(),
                  "references": sorted(vector, key=lambda x:x["id"])}
    digest = hashlib.sha256(canonical(commitment)).hexdigest()
    return reporting.DocumentScore(scorer_id, run.spec.id, len(expected), sum(r["found"] for r in rows), unsupported, digest)
