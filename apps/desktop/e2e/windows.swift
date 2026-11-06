// What the window server says about one process: no Screen Recording needed, since it reads no
// window names or pixels. `windows <pid>`. CI only, for the hang captures.
import AppKit
import CoreGraphics
import Foundation

let pid = Int32(CommandLine.arguments.dropFirst().first ?? "0") ?? 0
let info = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
let mine = info.filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid }.map { w -> [String: Any] in
  [
    "layer": w[kCGWindowLayer as String] ?? -1,
    "onScreen": w[kCGWindowIsOnscreen as String] ?? false,
    "alpha": w[kCGWindowAlpha as String] ?? -1,
    "bounds": w[kCGWindowBounds as String] ?? [:],
    "number": w[kCGWindowNumber as String] ?? -1,
  ]
}
let session = CGSessionCopyCurrentDictionary() as? [String: Any] ?? [:]
let display = CGMainDisplayID()
let out: [String: Any] = [
  "windows": mine,
  "windowsTotal": info.count,
  "frontmostPid": NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1,
  "screenLocked": session["CGSSessionScreenIsLocked"] ?? false,
  "onConsole": session["kCGSSessionOnConsoleKey"] ?? false,
  "displayAsleep": CGDisplayIsAsleep(display) != 0,
  "displayActive": CGDisplayIsActive(display) != 0,
]
let data = try JSONSerialization.data(withJSONObject: out, options: [.prettyPrinted, .sortedKeys])
print(String(decoding: data, as: UTF8.self))
