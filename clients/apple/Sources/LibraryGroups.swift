import Foundation

/// Group the complete filtered snapshot, retaining the pager's order within
/// each group. Stable IDs keep horizontal scroll positions across arrivals.
struct LibraryGroup: Identifiable {
    let id: String
    let label: String
    let order: Int
    var items: [Item]
}

enum LibraryGroups {
    static func make(_ items: [Item], sort: LibrarySort, now: Date = Date(),
                     calendar: Calendar = .current) -> [LibraryGroup] {
        var groups: [String: LibraryGroup] = [:]
        let today = calendar.startOfDay(for: now)
        let week = calendar.date(byAdding: .day, value: -6, to: today)!
        let month = calendar.date(from: calendar.dateComponents([.year, .month], from: now))!
        let formatter = DateFormatter()
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        formatter.setLocalizedDateFormatFromTemplate("MMMM yyyy")
        var monthLabels: [String: String] = [:]
        for item in items {
            if Task.isCancelled { return [] }
            let bucket = bucket(item, sort: sort, now: now, calendar: calendar,
                                today: today, week: week, month: month,
                                formatter: formatter, monthLabels: &monthLabels)
            if groups[bucket.id] == nil { groups[bucket.id] = bucket }
            groups[bucket.id]?.items.append(item)
        }
        return groups.values.sorted { $0.order < $1.order }
    }

    private static func bucket(_ item: Item, sort: LibrarySort, now: Date,
                               calendar: Calendar, today: Date, week: Date, month: Date,
                               formatter: DateFormatter, monthLabels: inout [String: String]) -> LibraryGroup {
        func group(_ id: String, _ label: String, _ order: Int) -> LibraryGroup {
            LibraryGroup(id: id, label: label, order: order, items: [])
        }
        switch sort {
        case .title:
            var key = item.sortTitle ?? item.title.lowercased()
            if item.sortTitle == nil {
                for article in ["the ", "an ", "a "] where key.hasPrefix(article) && key.count > article.count {
                    key = String(key.dropFirst(article.count)); break
                }
            }
            let scalar = key.unicodeScalars.first?.value ?? 0
            let letter = (65...90).contains(scalar) ? scalar : scalar >= 97 && scalar <= 122 ? scalar - 32 : 0
            let label = letter == 0 ? "#" : String(UnicodeScalar(letter)!)
            return group(label, label, Int(letter))
        case .year, .recorded:
            let year = sort == .year ? item.year : item.recordedAt.flatMap { Int($0.prefix(4)) }
            guard let year, year > 0 else {
                return group("unknown", sort == .year ? "Unknown year" : "Unknown recording date", Int.max)
            }
            return group("year-\(year)", String(year), -year)
        case .resolution:
            let height = item.resolution ?? 0
            let thresholds = [1700, 1300, 900, 650, 400, 1]
            let labels = ["4K", "1440p", "1080p", "720p", "480p", "SD", "Unknown resolution"]
            let index = thresholds.firstIndex { height >= $0 } ?? 6
            return group("res-\(index)", labels[index], index)
        case .added:
            guard let seconds = item.addedAt else { return group("unknown", "Unknown date added", Int.max) }
            let date = Date(timeIntervalSince1970: Double(seconds))
            if date > now { return group("future", "Future dates", -1) }
            if date >= today { return group("today", "Today", 0) }
            if date >= week { return group("week", "Previous 6 days", 1) }
            if date >= month { return group("month", "Earlier this month", 2) }
            let parts = calendar.dateComponents([.year, .month], from: date)
            let current = calendar.dateComponents([.year, .month], from: now)
            guard let year = parts.year, let m = parts.month, let currentYear = current.year,
                  let currentMonth = current.month else { return group("unknown", "Unknown date added", Int.max) }
            let key = "added-\(year)-\(m)"
            if monthLabels[key] == nil { monthLabels[key] = formatter.string(from: date) }
            return group(key, monthLabels[key]!, 3 + (currentYear - year) * 12 + currentMonth - m)
        }
    }
}
