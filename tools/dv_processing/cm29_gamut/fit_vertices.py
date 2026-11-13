"""Fit repeatable effective CM v2.9 lattice vertices from two probe sets.

Usage: fit_vertices.py INPUT_DIR OUTPUT_CSV --peak {600,1000,2000} --heldout DIR
Each input directory contains src.yuv, coords.txt, u.f64 and reference.yuv. This
module is safe to import; it reads inputs and writes the CSV only from main().
"""
from __future__ import annotations

import argparse
import csv
from collections import defaultdict
from pathlib import Path
import sys

import numpy as np

WIDTH = 3840
HEIGHT = 2160
FRAME_BYTES = WIDTH * HEIGHT * 3 * 4
I_MIN, I_MAX = 1, 64
CHROMA_MIN, CHROMA_MAX = -16, 16


def pq_peak_code(peak_nits: int) -> int:
    m1 = 2610.0 / 16384.0
    m2 = 2523.0 / 32.0
    c1 = 3424.0 / 4096.0
    c2 = 2413.0 / 128.0
    c3 = 2392.0 / 128.0
    y = (peak_nits / 10000.0) ** m1
    pq = ((c1 + c2 * y) / (1.0 + c3 * y)) ** m2
    return round(pq * 65535.0)


def solve_dataset(input_dir: Path) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    """Return integer vertex coords, raw values, amp, residual and direct mask."""
    src_path = input_dir / "src.yuv"
    coords_path = input_dir / "coords.txt"
    u_path = input_dir / "u.f64"
    ref_path = input_dir / "reference.yuv"
    for path in (src_path, coords_path, u_path, ref_path):
        if not path.is_file():
            raise ValueError(f"missing required input: {path}")
    if src_path.stat().st_size != FRAME_BYTES or ref_path.stat().st_size != FRAME_BYTES:
        raise ValueError("src.yuv and reference.yuv must each be a 3840x2160 RGB f32 frame")

    u = np.fromfile(u_path, dtype="<f8")
    if u.size % 3:
        raise ValueError(f"{u_path} byte count is not a multiple of three binary64 coordinates")
    u = u.reshape(-1, 3)
    u = np.where(np.abs(u - np.rint(u)) < 1e-5, np.rint(u), u)
    xy = np.loadtxt(coords_path, dtype=np.int64, ndmin=2)
    if xy.shape != (len(u), 2):
        raise ValueError(f"coords/u sample count mismatch: {len(xy)} coordinates vs {len(u)}")
    if ((xy[:, 0] < 0) | (xy[:, 0] >= WIDTH) | (xy[:, 1] < 0) | (xy[:, 1] >= HEIGHT)).any():
        raise ValueError("coords.txt contains an out-of-frame coordinate")
    ref = np.memmap(ref_path, dtype="<f4", mode="r", shape=(HEIGHT, WIDTH, 3))
    y = np.asarray(ref[xy[:, 1], xy[:, 0]], dtype=np.float64) * 65535.0

    base = np.floor(u).astype(np.int64)
    frac = u - base
    corners = np.array([[(bits >> dim) & 1 for dim in range(3)] for bits in range(8)], dtype=np.int64)
    coords = base[:, None, :] + corners[None, :, :]
    weights = np.prod(np.where(corners[None, :, :].astype(bool), frac[:, None, :], 1.0 - frac[:, None, :]), axis=2)
    keys = (coords[:, :, 0] * 128 + coords[:, :, 1] + 64) * 128 + coords[:, :, 2] + 64
    ids, inverse = np.unique(keys, return_inverse=True)
    inverse = inverse.reshape(weights.shape)
    ijk = np.stack((ids // 16384, (ids // 128) % 128 - 64, ids % 128 - 64), axis=1)
    values = np.full((len(ids), 3), np.nan, dtype=np.float64)
    amplification = np.full(len(ids), np.inf, dtype=np.float64)
    residual = np.full(len(ids), np.inf, dtype=np.float64)
    direct_mask = np.zeros(len(ids), dtype=bool)
    identifiable = np.zeros(len(ids), dtype=bool)

    base_cells: dict[tuple[int, int, int], list[int]] = defaultdict(list)
    for row, key in enumerate(map(tuple, base)):
        base_cells[key].append(row)
    direct: dict[int, list[int]] = {}
    for row in np.flatnonzero(weights.max(axis=1) > 0.99999):
        column = int(inverse[row, np.argmax(weights[row])])
        direct.setdefault(column, []).append(int(row))

    for column, vertex in enumerate(ijk):
        if column in direct:
            rows = direct[column]
            values[column] = np.mean(y[rows], axis=0)
            amplification[column] = 1.0 / np.sqrt(len(rows))
            residual[column] = np.abs(y[rows] - values[column]).max()
            direct_mask[column] = True
            identifiable[column] = True
            continue

        sample_rows = sorted({row for offset in corners for row in base_cells.get(tuple(vertex - offset), ())})
        if len(sample_rows) < 8:
            continue
        uv = u[sample_rows]
        cell_base = np.floor(uv).astype(np.int64)
        cell_frac = uv - cell_base
        relative = cell_base[:, None, :] + corners[None, :, :] - (vertex - 1)
        valid = ((relative >= 0) & (relative <= 2)).all(axis=2)
        matrix = np.zeros((len(sample_rows), 27), dtype=np.float64)
        for bit, corner in enumerate(corners):
            rows = np.flatnonzero(valid[:, bit])
            indexes = (relative[rows, bit, 0] * 3 + relative[rows, bit, 1]) * 3 + relative[rows, bit, 2]
            weights_for_corner = np.prod(
                np.where(corner[None, :].astype(bool), cell_frac[rows], 1.0 - cell_frac[rows]), axis=1
            )
            matrix[rows, indexes] += weights_for_corner
        left, singular, right = np.linalg.svd(matrix, full_matrices=False)
        rank = int((singular > singular[0] * 1e-9).sum()) if singular.size and singular[0] else 0
        if rank == 0 or abs(1.0 - np.dot(right[:rank, 13], right[:rank, 13])) > 1e-7:
            continue
        identifiable[column] = True
        pinv = (right[:rank].T / singular[:rank]) @ left[:, :rank].T
        amplification[column] = np.linalg.norm(pinv[13])
        fitted = matrix @ pinv @ y[sample_rows]
        residual[column] = np.abs(fitted - y[sample_rows]).max()
        values[column] = pinv[13] @ y[sample_rows]

    return ijk, values, amplification, residual, direct_mask, identifiable


def fit_pair(input_dir: Path, heldout_dir: Path) -> dict[str, object]:
    a_ijk, a_values, a_amp, a_resid, a_direct, a_identifiable = solve_dataset(input_dir)
    b_ijk, b_values, b_amp, b_resid, b_direct, b_identifiable = solve_dataset(heldout_dir)
    a_index = {tuple(map(int, coord)): i for i, coord in enumerate(a_ijk)}
    b_index = {tuple(map(int, coord)): i for i, coord in enumerate(b_ijk)}
    common = sorted(a_index.keys() & b_index.keys())
    accepted: dict[tuple[int, int, int], dict[str, object]] = {}
    max_agreement = 0.0
    for coord in common:
        ai, bi = a_index[coord], b_index[coord]
        av, bv = a_values[ai], b_values[bi]
        delta = np.abs(av - bv)
        agreement = float(delta.max()) if np.isfinite(delta).all() else float("inf")
        if np.isfinite(agreement):
            max_agreement = max(max_agreement, agreement)
        pass_a = (
            np.isfinite(av).all() and I_MIN <= a_ijk[ai, 0] <= I_MAX
            and CHROMA_MIN <= a_ijk[ai, 1] <= CHROMA_MAX
            and CHROMA_MIN <= a_ijk[ai, 2] <= CHROMA_MAX
            and a_identifiable[ai] and a_amp[ai] < 8.0 and a_resid[ai] < 0.25
            and np.all(av >= -0.5) and np.all(av <= 65535.5)
        )
        pass_b = (
            np.isfinite(bv).all() and I_MIN <= b_ijk[bi, 0] <= I_MAX
            and CHROMA_MIN <= b_ijk[bi, 1] <= CHROMA_MAX
            and CHROMA_MIN <= b_ijk[bi, 2] <= CHROMA_MAX
            and b_identifiable[bi] and b_amp[bi] < 8.0 and b_resid[bi] < 0.25
            and np.all(bv >= -0.5) and np.all(bv <= 65535.5)
        )
        if pass_a and pass_b and agreement < 0.25:
            accepted[coord] = {
                "value": (av + bv) * 0.5,
                "both_direct": bool(a_direct[ai] and b_direct[bi]),
                "agreement": agreement,
            }
    return {
        "rows": accepted,
        "direct_keys": {coord for coord, row in accepted.items() if row["both_direct"]},
        "diagnostics": {
            "original_vertices": len(a_ijk), "heldout_vertices": len(b_ijk),
            "common_vertices": len(common), "accepted_vertices": len(accepted),
            "both_direct_vertices": sum(bool(row["both_direct"]) for row in accepted.values()),
            "max_common_estimate_delta": max_agreement,
        },
    }


def quantize(value: np.ndarray, limit: int) -> tuple[int, int, int]:
    return tuple(map(int, np.clip(np.rint(value), 0, limit).astype(np.int64)))


def write_merged(pairs: list[dict[str, object]], output_csv: Path, peak: int) -> dict[str, object]:
    # Earlier pairs have priority; each supplement only fills coordinates still unknown.
    merged: dict[tuple[int, int, int], dict[str, object]] = {}
    ownership: dict[tuple[int, int, int], int] = {}
    pair_summary = []
    overlap_delta = []
    for pair_index, pair in enumerate(pairs):
        rows = pair["rows"]
        added = 0
        overlap = 0
        max_overlap_code_delta = 0
        for coord, row in rows.items():
            if coord in merged:
                overlap += 1
                old_code = quantize(merged[coord]["value"], pq_peak_code(peak))
                new_code = quantize(row["value"], pq_peak_code(peak))
                max_overlap_code_delta = max(
                    max_overlap_code_delta,
                    max(abs(old_code[k] - new_code[k]) for k in range(3)),
                )
                continue
            merged[coord] = row
            ownership[coord] = pair_index
            added += 1
        pair_summary.append({
            "pair_index": pair_index,
            "accepted_vertices": len(rows),
            "added_vertices": added,
            "overlap_vertices": overlap,
            "max_overlap_quantized_delta_code": max_overlap_code_delta,
            "diagnostics": pair["diagnostics"],
        })
        if pair_index:
            overlap_delta.append(max_overlap_code_delta)

    limit = pq_peak_code(peak)
    table_rows = [(*coord, *quantize(row["value"], limit)) for coord, row in merged.items()]
    table_rows.sort(key=lambda row: row[:3])
    output_csv.parent.mkdir(parents=True, exist_ok=True)
    with output_csv.open("w", newline="", encoding="ascii") as f:
        writer = csv.writer(f, lineterminator="\n")
        writer.writerow(("i", "p", "t", "r", "g", "b"))
        writer.writerows(table_rows)
    plane1 = [(coord, row["value"], row["agreement"]) for coord, row in merged.items()
              if coord[0] == 1 and -1 <= coord[1] <= 1 and -1 <= coord[2] <= 1]
    return {
        "total_vertices": len(table_rows), "peak_code": limit,
        "pairs": pair_summary, "max_overlap_quantized_delta_code": max(overlap_delta, default=0),
        "plane1": sorted(plane1, key=lambda row: row[0]),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input_dir", type=Path)
    parser.add_argument("output_csv", type=Path)
    parser.add_argument("--peak", required=True, type=int, choices=(600, 1000, 2000))
    parser.add_argument("--heldout", required=True, type=Path)
    parser.add_argument("--supplement", nargs=2, action="append", default=[],
                        metavar=("INPUT_DIR", "HELDOUT_DIR"),
                        help="independent supplemental input and held-out pair; may be repeated")
    args = parser.parse_args(argv)
    pair_dirs = [(args.input_dir, args.heldout)] + [
        (Path(input_dir), Path(heldout_dir)) for input_dir, heldout_dir in args.supplement
    ]
    try:
        pairs = [fit_pair(input_dir, heldout_dir) for input_dir, heldout_dir in pair_dirs]
        stats = write_merged(pairs, args.output_csv, args.peak)
    except (OSError, ValueError, np.linalg.LinAlgError) as exc:
        print(f"fit_vertices: {exc}", file=sys.stderr)
        return 1
    for i, pair in enumerate(stats["pairs"]):
        diag = pair["diagnostics"]
        label = "neutral" if i == 0 else f"supplement-{i}"
        print(
            f"{label}: accepted={pair['accepted_vertices']} added={pair['added_vertices']} "
            f"overlap={pair['overlap_vertices']} max_overlap_code_delta={pair['max_overlap_quantized_delta_code']} "
            f"direct_both={diag['both_direct_vertices']} common={diag['common_vertices']}"
        )
    print(f"merged total={stats['total_vertices']} peak_code={stats['peak_code']} "
          f"max_supplement_overlap_delta={stats['max_overlap_quantized_delta_code']}")
    for coord, value, agreement in stats["plane1"]:
        r, g, b = quantize(value, stats["peak_code"])
        print(f"plane1 {coord[1]:+d} {coord[2]:+d}: {r} {g} {b} (delta {agreement:.6g})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
