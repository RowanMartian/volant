// Saves Volant's icon exactly as macOS draws it (Liquid Glass and all) to a PNG.
// Usage: appicon <path to Volant.app> <output.png> [size]
import AppKit

let args = CommandLine.arguments
let size = args.count > 3 ? Double(args[3]) ?? 512 : 512
let icon = NSWorkspace.shared.icon(forFile: args[1])
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size), bitsPerSample: 8,
                           samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
icon.draw(in: NSRect(x: 0, y: 0, width: size, height: size))
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: args[2]))
print("wrote \(args[2])")
