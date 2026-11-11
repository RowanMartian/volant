// Launches a browser and waits for its first real window to appear on screen.
// Prints the milliseconds it took. Usage: launch <window owner name> <executable> [args...]
// Only window owner names and bounds are read, which needs no screen-recording permission.
import CoreGraphics
import Foundation

let args = CommandLine.arguments
guard args.count >= 3 else {
    FileHandle.standardError.write("usage: launch <owner> <executable> [args...]\n".data(using: .utf8)!)
    exit(2)
}
let owner = args[1]
let process = Process()
process.executableURL = URL(fileURLWithPath: args[2])
process.arguments = Array(args.dropFirst(3))
process.standardOutput = FileHandle.nullDevice
process.standardError = FileHandle.nullDevice

func hasWindow() -> Bool {
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] else { return false }
    return list.contains { w in
        guard (w[kCGWindowOwnerName as String] as? String) == owner, (w[kCGWindowLayer as String] as? Int) == 0 else { return false }
        let b = w[kCGWindowBounds as String] as? [String: Any]
        return ((b?["Height"] as? Double) ?? 0) > 300 && ((b?["Width"] as? Double) ?? 0) > 300
    }
}

let start = DispatchTime.now()
do { try process.run() } catch {
    print("failed: \(error)")
    exit(1)
}
while !hasWindow() {
    if DispatchTime.now().uptimeNanoseconds - start.uptimeNanoseconds > 30_000_000_000 {
        print("timeout")
        exit(1)
    }
    usleep(2_000)
}
print(String(format: "%.0f", Double(DispatchTime.now().uptimeNanoseconds - start.uptimeNanoseconds) / 1_000_000))
