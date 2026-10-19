import Foundation
import Darwin

/// The receipt is a small, durable same-directory rename. Neither symlink
/// destinations nor an oversized observation can become a successful receipt.
public enum SmokeReceipt {
    public static let maximumBytes = 64 * 1024
    public enum Failure: Error { case tooLarge, unsafeDestination, io(Int32) }

    public static func write(_ value: [String: Any], to destination: URL) throws {
        let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        guard data.count <= maximumBytes else { throw Failure.tooLarge }
        let parent = destination.deletingLastPathComponent()
        let directory = open(parent.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard directory >= 0 else { throw Failure.io(errno) }
        defer { close(directory) }
        var existing = stat()
        if fstatat(directory, destination.lastPathComponent, &existing, AT_SYMLINK_NOFOLLOW) == 0 {
            guard existing.st_mode & S_IFMT == S_IFREG, existing.st_uid == getuid(), existing.st_nlink == 1 else {
                throw Failure.unsafeDestination
            }
        } else if errno != ENOENT { throw Failure.io(errno) }
        let temporary = ".plurx-smoke-\(UUID().uuidString).json"
        let fd = openat(directory, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw Failure.io(errno) }
        defer { close(fd); unlinkat(directory, temporary, 0) }
        try data.withUnsafeBytes { bytes in
            var offset = 0
            while offset < bytes.count {
                let count = Darwin.write(fd, bytes.baseAddress!.advanced(by: offset), bytes.count - offset)
                if count < 0, errno == EINTR { continue }
                guard count > 0 else { throw Failure.io(errno) }
                offset += count
            }
        }
        guard fsync(fd) == 0 else { throw Failure.io(errno) }
        guard renameat(directory, temporary, directory, destination.lastPathComponent) == 0 else {
            throw Failure.io(errno)
        }
        guard fsync(directory) == 0 else { throw Failure.io(errno) }
    }
}
