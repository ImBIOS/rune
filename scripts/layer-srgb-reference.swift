// Headless AppKit source-over in an explicitly sRGB bitmap. Run through HiveMind.
import AppKit

let cases: [(String, [Double], [Double])] = [
    ("white-6%-dark", [1, 1, 1, 0.06], [0.08, 0.08, 0.08, 1]),
    ("white-20%-dark", [1, 1, 1, 0.2], [0.08, 0.08, 0.08, 1]),
    ("white-50%-black", [1, 1, 1, 0.5], [0, 0, 0, 1]),
    ("black-20%-light", [0, 0, 0, 0.2], [0.9, 0.9, 0.9, 1]),
    ("grey-35%-dark", [0.6, 0.6, 0.6, 0.35], [0.12, 0.12, 0.12, 1]),
    ("white-over-blue", [1, 1, 1, 0.2], [0.04, 0.2, 0.6, 1]),
    ("orange-over-blue", [1, 0.25, 0.04, 0.4], [0.04, 0.2, 0.6, 1]),
    ("cyan-over-red", [0.04, 0.9, 0.8, 0.4], [0.7, 0.1, 0.15, 1]),
]
var output: [[String: Any]] = []
for (name, fg, bg) in cases {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let bitmap = CGContext(data: nil, width: 4, height: 4, bitsPerComponent: 8, bytesPerRow: 16,
        space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(cgContext: bitmap, flipped: false)
    for c in [bg, fg] {
        NSColor(srgbRed: c[0], green: c[1], blue: c[2], alpha: c[3]).setFill()
        NSBezierPath(rect: NSRect(x: 0, y: 0, width: 4, height: 4)).fill()
    }
    NSGraphicsContext.restoreGraphicsState()
    bitmap.flush()
    let p = bitmap.data!.assumingMemoryBound(to: UInt8.self)
    output.append(["name": name, "fg": fg, "bg": bg, "rgba": (0..<4).map { Int(p[$0]) }])
}
let data = try JSONSerialization.data(withJSONObject: output, options: [.sortedKeys])
FileHandle.standardOutput.write(data)
