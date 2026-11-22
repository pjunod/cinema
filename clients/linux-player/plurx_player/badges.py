"""The facts Cinema's quality pills read, and nothing else.

The web player paints resolution, dynamic range (source → delivered), the
Dolby Vision enhancement pills (FEL / RPU / DYN) and the audio format above
its picture (crates/plurxd/src/web/detail/dynamic-range.js
`playerFactBadges`). Cinema draws the same pills inside mpv. The Lua overlay
observes what mpv decodes and renders; this module hands it the three facts
only the launcher holds, whitelisted so no URL, path or credential reaches
mpv's user-data:

* ``source``: the decision's SourceSummary (crates/plurxd/src/http/stream.rs),
  i.e. what the file *is*;
* ``enhancement``: the HLS session's server-computed PlaybackEnhancement
  (crates/plurx-core/src/playback/enhancement.rs). A processed delivery's
  pills hold only while its current effective-processing report does, the
  same rule as the web's `playerEnhancement`. Direct play has no session and
  so no server enhancement;
* ``renderer``: which Dolby Vision mapping the private mpv build reported
  for the current file (playback.py `on_log`), which is how the overlay knows
  the RPU and dynamic metadata were applied on this machine.
"""

ENHANCEMENTS = ("FEL", "RPU", "DYN")
NATIVE_MAPPER = "shared native Dolby renderer"
_RPU_NAMES = {"PolynomialReshape": "polynomial reshaping", "MmrReshape": "MMR reshaping",
              "RpuColorConversion": "RPU color conversion"}


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


def _processed(enhancement, report):
    """The current report's FEL/RPU/DYN, superseding the provisional snapshot."""
    operations = report.get("applied_operations") or []
    rpu = [_RPU_NAMES[op] for op in operations if isinstance(op, str) and op in _RPU_NAMES]
    target = next((op["DisplayTargetMapping"].get("nits") for op in operations
                   if isinstance(op, dict) and isinstance(op.get("DisplayTargetMapping"), dict)), None)
    trims = ("with the colourist's creative trims" if "CreativeTrimApplication" in operations
             else "with the colourist's creative trims on some shots" if report.get("creative_trims_partial") is True
             else "L1 only; creative trims are not applied")
    fel = report.get("fel_contributed") is True
    sdr = "SDR " if report.get("sdr_processed") is True else ""
    current = [
        {"badge": "FEL", "active": fel, "text": f"FEL: full enhancement layer reconstructed into the {sdr}picture"
            if fel else "FEL: no enhancement-layer detail in this window"},
        {"badge": "RPU", "active": bool(rpu), "text": f"RPU: {', '.join(rpu)} applied by the server"
            if rpu else "RPU: no reshaping or colour conversion applied in this window"},
        {"badge": "DYN", "active": type(target) is int,
         "text": f"DYN: per-shot Dolby display mapping for a {target}-nit display by the server ({trims})"
            if type(target) is int else "DYN: no per-shot Dolby display mapping applied in this window"},
    ]
    had_fel = any(d["badge"] == "FEL" for d in enhancement["details"])
    details = [d for d in current if d["badge"] != "FEL" or d["active"] or had_fel]
    details += [d for d in enhancement["details"] if d["badge"] not in ENHANCEMENTS]
    return {"active": [d["badge"] for d in current if d["active"]], "details": details}


def server_enhancement(session, report=None):
    """The session's enhancement as the overlay may show it now; None when absent.

    ``report`` is the current, generation-matching effective-processing report
    (playback.py `effective_tone_mapping`), or None. A delivery that requires
    one shows nothing without it: its claims are only true while the report
    proving them is current.
    """
    raw = (session or {}).get("enhancement")
    if not isinstance(raw, dict) or not isinstance(raw.get("active"), list) \
            or not isinstance(raw.get("details"), list):
        return None
    details = [{"badge": d["badge"][:16], "active": d.get("active") is True, "text": d["text"]}
               for d in raw["details"] if isinstance(d, dict) and isinstance(d.get("badge"), str)
               and isinstance(d.get("text"), str) and 0 < len(d["text"]) <= 240]
    enhancement = {"active": [b for b in ENHANCEMENTS if b in raw["active"]], "details": details}
    if raw.get("requires_effective_processing") is True:
        return _processed(enhancement, report) if isinstance(report, dict) else None
    return enhancement


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
        try:
            target_nits = float(mapper.split("target=", 1)[1].split("-", 1)[1].split(" ", 1)[0])
        except (IndexError, ValueError):
            target_nits = None
    nits = round(target_nits) if isinstance(target_nits, (int, float)) and 0 < target_nits <= 10000 else None
    return {"kind": kind, "label": mapper, "target_nits": nits}


def video_facts(decision, session, mapper, target_nits=None, report=None):
    return {"source": source_facts(decision), "enhancement": server_enhancement(session, report),
            "renderer": renderer_facts(mapper, target_nits)}
