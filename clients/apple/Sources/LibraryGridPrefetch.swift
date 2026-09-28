import Foundation

/// A-03: two measured adaptive-grid rows beyond the zero-based visible card.
/// The pager consumes an exclusive item count, not a final item index.
enum LibraryGridPrefetch {
    static func columns(width: Double, minimumWidth: Double, spacing: Double) -> Int {
        guard width.isFinite, minimumWidth.isFinite, spacing.isFinite,
              width > 0, minimumWidth > 0, spacing >= 0 else { return 1 }
        let count = floor((width + spacing) / (minimumWidth + spacing))
        return Int(min(Double(Int.max / 2), max(1, count)))
    }

    static func exclusiveCount(lastVisibleIndex: Int, columns: Int) -> Int {
        guard lastVisibleIndex >= 0, columns > 0 else { return 0 }
        let (base, indexOverflow) = lastVisibleIndex.addingReportingOverflow(1)
        let (rows, rowOverflow) = columns.multipliedReportingOverflow(by: 2)
        let (result, sumOverflow) = base.addingReportingOverflow(rows)
        return indexOverflow || rowOverflow || sumOverflow ? Int.max : result
    }
}
