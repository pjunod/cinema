"""The facts Cinema's quality pills read, and nothing else.

The web player paints resolution, dynamic range (source → delivered), the
Dolby Vision enhancement pills (FEL / RPU / DYN) and the audio format above
its picture (crates/plurxd/src/web/detail/dynamic-range.js
`playerFactBadges`). Cinema draws the same pills inside mpv. The Lua overlay
observes what mpv decodes and renders; this module hands it the four facts
only the launcher holds, whitelisted so no URL, path or credential reaches
mpv's user-data:

* ``source``: the decision's SourceSummary (crates/plurxd/src/http/stream.rs),
  i.e. what the file *is*;
* ``enhancement``: the HLS session's server-computed PlaybackEnhancement
  (crates/plurx-core/src/playback/enhancement.rs), read the way the web's
  `playerEnhancement` reads it: a processed delivery's pills hold only while
  its current, generation-matched effective-processing report does, and that
  report supersedes the provisional snapshot. ``processed`` names the
  processing ("HDR10-E", "SDR-E", "P81") the report proves, for the range pill.
  Direct play has no session and so no server enhancement;
* ``provenance``: the server's "Dolby Vision metadata" and "Display target"
  rows (``dv_metadata`` / ``display_target``), selected and validated as the
  web's `playerDvRows` does, with the contract's sub-row labels;
* ``renderer``: which Dolby Vision mapping the private mpv build reported
  for the current file (playback.py `on_log`), which is how the overlay knows
  the RPU and dynamic metadata were applied on this machine.
"""
import re

ENHANCEMENTS = ("FEL", "RPU", "DYN")
# Sub-row labels of the server's two Playback info rows. The packaged player
# cannot read tests/playback/playback-info-fields.json, so test_badges.py pins
# these to that contract's `sub_rows`.
DV_METADATA_LABELS = ("In the file", "Applied by Cinema", "Left for the display")
DISPLAY_TARGET_LABELS = ("Reported peak", "Mapped for", "Title peak")
NATIVE_MAPPER = "shared native Dolby renderer"
_RPU_NAMES = {"PolynomialReshape": "polynomial reshaping", "MmrReshape": "MMR reshaping",
              "RpuColorConversion": "RPU color conversion"}
_GENERATION = re.compile(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}")
_NUMBER = r"[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?"
_CLOSED_FORM_TARGET = re.compile(rf"\btarget=(?:{_NUMBER})-(?P<peak>{_NUMBER}) nits\b")


def _text(value, limit):
    if not isinstance(value, str) or not value:
        return None
    return value.replace("\n", " ").replace("\r", " ")[:limit]


def _count(value, high):
    return value if type(value) is int and 0 < value <= high else None


def source_facts(decision):
    source = (decision or {}).get("source")
    source = source if isinstance(source, dict) else {}
    facts = {"hdr": _text(source.get("hdr"), 32), "hdr_format": _text(source.get("hdr_format"), 80),
             "video_codec": _text(source.get("video_codec"), 32),
             "dv_profile": _count(source.get("dv_profile"), 20),
             "bit_depth": _count(source.get("bit_depth"), 32),
             "dv_el_present": source.get("dv_el_present") if type(source.get("dv_el_present")) is bool else None}
    return {key: value for key, value in facts.items() if value is not None}


def current_report(session):
    """(report, kind) for the session's current effective-processing report, else (None, None).

    dynamic-range.js `effectiveHdrProcessing`: the report must name this
    session's generation and a delivery it can describe — server-processed
    HDR10 ("hdr10"), Dolby Vision–processed SDR ("sdr") or a reconstructed
    Profile 8.1 picture ("p81") — with a bounded, well-formed operation list.
    """
    session = session if isinstance(session, dict) else {}
    report = session.get("effective_processing")
    control = session.get("control")
    generation = control.get("generation") if isinstance(control, dict) else None
    if not isinstance(report, dict) or not isinstance(generation, str) \
            or _GENERATION.fullmatch(generation) is None or report.get("generation") != generation \
            or type(report.get("fel_contributed")) is not bool:
        return None, None
    delivered = session.get("delivered_dynamic_range")
    if delivered == "sdr" and report.get("sdr_processed") is True and report.get("hdr10_enhanced") is False:
        kind = "sdr"
    elif delivered == "dolby_vision" and report.get("profile81_reconstructed") is True \
            and report.get("hdr10_enhanced") is False and report.get("sdr_processed") is not True \
            and report["fel_contributed"] is True:
        kind = "p81"
    elif delivered == "hdr10" and report.get("hdr10_enhanced") is True and report.get("sdr_processed") is not True:
        kind = "hdr10"
    else:
        return None, None
    operations = report.get("applied_operations")
    if not isinstance(operations, list) or not 0 < len(operations) <= 16:
        return None, None
    targets = (100,) if kind == "sdr" else (400, 600, 1000, 1600, 2000, 4000)
    for op in operations:
        if isinstance(op, str) and 0 < len(op) <= 64:
            continue
        mapping = op.get("DisplayTargetMapping") if isinstance(op, dict) and len(op) == 1 else None
        if not isinstance(mapping, dict) or len(mapping) != 1 or mapping.get("nits") not in targets \
                or type(mapping.get("nits")) is not int:
            return None, None
    mapped = [op for op in operations if isinstance(op, dict)]
    if kind == "sdr" and ("Bt709Gamma24Conversion" not in operations
                          or not any(op["DisplayTargetMapping"]["nits"] == 100 for op in mapped)):
        return None, None
    if kind == "p81" and ("LinearNlqResidual" not in operations or mapped):
        return None, None
    return report, kind


def _skip_reason(session):
    """A late optional-upgrade refusal for this generation (`processingSkipFromWire`)."""
    skip = session.get("processing_skip")
    control = session.get("control")
    generation = control.get("generation") if isinstance(control, dict) else None
    if not isinstance(skip, dict) or not isinstance(generation, str) or not generation \
            or skip.get("generation") != generation or not isinstance(skip.get("reason"), str):
        return None
    reason = skip["reason"]
    if not reason.strip() or len(reason) > 512 or re.search(r"[\x00-\x1f\x7f-\x9f]", reason):
        return None
    return reason


def _processed(enhancement, report):
    """The current report's FEL/RPU/DYN, superseding the provisional snapshot."""
    operations = report["applied_operations"]
    rpu = [_RPU_NAMES[op] for op in operations if isinstance(op, str) and op in _RPU_NAMES]
    target = next((op["DisplayTargetMapping"]["nits"] for op in operations if isinstance(op, dict)), None)
    trims = ("with the colourist's creative trims" if "CreativeTrimApplication" in operations
             else "with the colourist's creative trims on some shots" if report.get("creative_trims_partial") is True
             else "L1 only; creative trims are not applied")
    fel = report["fel_contributed"]
    sdr = "SDR " if report.get("sdr_processed") is True else ""
    current = [
        {"badge": "FEL", "active": fel, "text": f"FEL: full enhancement layer reconstructed into the {sdr}picture"
            if fel else "FEL: no enhancement-layer detail in this window"},
        {"badge": "RPU", "active": bool(rpu), "text": f"RPU: {', '.join(rpu)} applied by the server"
            if rpu else "RPU: no reshaping or colour conversion applied in this window"},
        {"badge": "DYN", "active": target is not None,
         "text": f"DYN: per-shot Dolby display mapping for a {target}-nit display by the server ({trims})"
            if target is not None else "DYN: no per-shot Dolby display mapping applied in this window"},
    ]
    had_fel = any(d["badge"] == "FEL" for d in enhancement["details"])
    details = [d for d in current if d["badge"] != "FEL" or d["active"] or had_fel]
    details += [d for d in enhancement["details"] if d["badge"] not in ENHANCEMENTS]
    return {"active": [d["badge"] for d in current if d["active"]], "details": details}


def server_enhancement(session):
    """The session's enhancement as the overlay may show it now; None when absent."""
    session = session if isinstance(session, dict) else {}
    raw = session.get("enhancement")
    if not isinstance(raw, dict) or not isinstance(raw.get("active"), list) \
            or not isinstance(raw.get("details"), list):
        return None
    details = [{"badge": d["badge"][:16], "active": d.get("active") is True, "text": d["text"]}
               for d in raw["details"] if isinstance(d, dict) and isinstance(d.get("badge"), str)
               and isinstance(d.get("text"), str) and 0 < len(d["text"]) <= 240]
    reason = _skip_reason(session)
    if reason:
        details = [dict(d, text=f"{d['badge']}: not used for this play — {reason}")
                   if not d["active"] and d["badge"] in ("FEL", "RPU") and d["text"].startswith(f"{d['badge']}: not used")
                   else d for d in details]
    enhancement = {"active": [b for b in ENHANCEMENTS if b in raw["active"]], "details": details, "processed": None}
    report, kind = current_report(session)
    if kind == "p81":
        return {"active": ["FEL"], "processed": "P81", "details": [
            {"badge": "FEL", "active": True,
             "text": "FEL: full enhancement layer reconstructed into the Dolby Vision Profile 8.1 picture"}]
            + [d for d in details if d["badge"] not in ENHANCEMENTS]}
    if raw.get("requires_effective_processing") is not True:
        return enhancement
    if report is None:
        return None
    return dict(_processed(enhancement, report), processed="SDR-E" if kind == "sdr" else "HDR10-E")


def _row_text(value):
    """dynamic-range.js `dvRowText`: a non-blank, bounded, control-free string."""
    if isinstance(value, str) and value.strip() and len(value) <= 512 \
            and not re.search(r"[\x00-\x1f\x7f-\x9f]", value):
        return value
    return None


def _provenance_row(labels, headline, texts):
    """{"value", "rows"} when every text is valid, else None (the whole row is dropped)."""
    texts = [_row_text(t) for t in texts]
    if any(t is None for t in texts) or _row_text(headline) is None:
        return None
    return {"value": headline, "rows": [{"label": l, "text": t} for l, t in zip(labels, texts)]}


def provenance_facts(session):
    """The server's Dolby Vision metadata and display target rows, as the web's `playerDvRows` reads them.

    A processed delivery's rows come only from its current, generation-matched
    report; otherwise the metadata comes from the enhancement and there is no
    display target. Each row is dropped whole when any of its texts is invalid.
    """
    session = session if isinstance(session, dict) else {}
    enhancement = session.get("enhancement")
    if not isinstance(enhancement, dict):
        return {}
    if enhancement.get("requires_effective_processing") is True:
        report, _ = current_report(session)
        metadata = report.get("dv_metadata") if report else None
        target = report.get("display_target") if report else None
    else:
        metadata, target = enhancement.get("dv_metadata"), None

    def part(value, key):
        value = value.get(key) if isinstance(value, dict) else None
        return value if isinstance(value, dict) else {}

    facts = {}
    if isinstance(metadata, dict):
        applied = part(metadata, "applied").get("text")
        row = _provenance_row(DV_METADATA_LABELS, applied, [
            part(metadata, "in_file").get("text"), applied, part(metadata, "left_for_display").get("text")])
        if row:
            facts["dv_metadata"] = row
    if isinstance(target, dict):
        mapped = target.get("mapped_for_text")
        row = _provenance_row(DISPLAY_TARGET_LABELS, mapped, [
            target.get("reported_peak_text"), mapped, target.get("title_peak_text")])
        if row:
            facts["display_target"] = row
    return facts


def renderer_facts(mapper, target_nits=None):
    """Classify the private build's mapper line (mpv patch 0003, or the shared native renderer).

    native / closed_form: plurx's own Dolby Vision display management mapped
    the frame; libplacebo: the RPU was read but libplacebo's built-in mapping
    ran (every other "none (...)" line says so); none: no RPU. A bare libplacebo
    "CM vX" line is libplacebo's own mapping. None: no line, as with a
    system mpv, so the overlay claims nothing beyond what mpv itself reports.
    """
    mapper = _text(mapper, 80)
    if mapper is None:
        return None
    if mapper == NATIVE_MAPPER:
        kind = "native"
    elif mapper.startswith("closed-form"):
        kind = "closed_form"
    elif mapper.startswith("none (no Dolby Vision RPU"):
        kind = "none"
    else:
        kind = "libplacebo"
    if target_nits is None and kind == "closed_form":
        match = _CLOSED_FORM_TARGET.search(mapper)
        target_nits = float(match["peak"]) if match else None
    nits = round(target_nits) if isinstance(target_nits, (int, float)) and 0 < target_nits <= 10000 else None
    return {"kind": kind, "label": mapper, "target_nits": nits}


def video_facts(decision, session, mapper, target_nits=None):
    return {"source": source_facts(decision), "enhancement": server_enhancement(session),
            "renderer": renderer_facts(mapper, target_nits), "provenance": provenance_facts(session)}
