// Rasterizes an SVG onto black as big-endian RGB565 for the firmware.
// Usage: swift scripts/logo.swift <in.svg> <size> <out.raw> [preview.png]
import AppKit

let args = CommandLine.arguments
let size = Int(args[2])!
guard let image = NSImage(contentsOfFile: args[1]) else { fatalError("can't read \(args[1])") }
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
                           bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                           colorSpaceName: .deviceRGB, bytesPerRow: size * 4, bitsPerPixel: 32)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSColor.black.setFill()
NSRect(x: 0, y: 0, width: size, height: size).fill()
image.draw(in: NSRect(x: 0, y: 0, width: size, height: size))
NSGraphicsContext.restoreGraphicsState()

var out = Data()
let px = rep.bitmapData!
for i in 0..<(size * size) {
    let (r, g, b) = (UInt16(px[i * 4]), UInt16(px[i * 4 + 1]), UInt16(px[i * 4 + 2]))
    let v = (r >> 3) << 11 | (g >> 2) << 5 | (b >> 3)
    out.append(contentsOf: [UInt8(v >> 8), UInt8(v & 0xff)])
}
try! out.write(to: URL(fileURLWithPath: args[3]))
if args.count > 4 { try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: args[4])) }
