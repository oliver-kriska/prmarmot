// window.swift <pid> — print "<window id> <x> <y> <width> <height>" (points,
// top-left origin) of the largest on-screen window owned by <pid>.
//
// Exits 3 when another app's window lies over it: GPUI draws nothing while
// covered, so a capture then would keep a stale frame. Exits 1 when the
// process has no on-screen window yet.
import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 2, let pid = Int32(CommandLine.arguments[1]) else {
    FileHandle.standardError.write("usage: window.swift <pid>\n".data(using: .utf8)!)
    exit(2)
}
// Front to back.
let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] ?? []

func bounds(_ window: [String: Any]) -> CGRect {
    guard let b = window[kCGWindowBounds as String] as? [String: Double] else { return .zero }
    return CGRect(x: b["X"] ?? 0, y: b["Y"] ?? 0, width: b["Width"] ?? 0, height: b["Height"] ?? 0)
}
func owner(_ window: [String: Any]) -> Int32? { window[kCGWindowOwnerPID as String] as? Int32 }
func layer(_ window: [String: Any]) -> Int { window[kCGWindowLayer as String] as? Int ?? 0 }

guard let mine = windows
    .filter({ owner($0) == pid && layer($0) == 0 })
    .max(by: { bounds($0).width * bounds($0).height < bounds($1).width * bounds($1).height }),
    let number = mine[kCGWindowNumber as String] as? Int,
    let position = windows.firstIndex(where: { ($0[kCGWindowNumber as String] as? Int) == number })
else {
    FileHandle.standardError.write("no on-screen window for pid \(pid)\n".data(using: .utf8)!)
    exit(1)
}
let frame = bounds(mine)
// Ordinary windows in front of ours that overlap it. Menu bar, Dock and
// other system layers are not ordinary windows and never cover a capture.
let covering = windows[..<position].filter {
    owner($0) != pid && layer($0) == 0 && bounds($0).intersects(frame)
}
if let first = covering.first {
    let name = first[kCGWindowOwnerName as String] as? String ?? "another app"
    FileHandle.standardError.write("window \(number) is covered by \(name)\n".data(using: .utf8)!)
    exit(3)
}
print(number, Int(frame.minX), Int(frame.minY), Int(frame.width), Int(frame.height))
