// Draws one line of text on a dark bar and saves it as a PNG: the timer
// under the start clip (docs/media/clips.py). ffmpeg here has no text
// filter, and this needs nothing installed on a Mac.
//
//   swift docs/media/caption.swift WIDTH HEIGHT OUT.png TEXT [OUT.png TEXT ...]
import AppKit

let arguments = CommandLine.arguments
guard arguments.count >= 5, let width = Int(arguments[1]), let height = Int(arguments[2]) else {
    FileHandle.standardError.write("usage: caption.swift WIDTH HEIGHT OUT.png TEXT ...\n".data(using: .utf8)!)
    exit(2)
}
var index = 3
while index + 1 < arguments.count {
    let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
    NSColor(calibratedRed: 0.07, green: 0.08, blue: 0.07, alpha: 1).setFill()
    NSRect(x: 0, y: 0, width: width, height: height).fill()
    let size = CGFloat(height) * 0.42
    let text = NSAttributedString(
        string: arguments[index + 1],
        attributes: [
            .font: NSFont.monospacedDigitSystemFont(ofSize: size, weight: .medium),
            .foregroundColor: NSColor.white,
        ])
    let bounds = text.size()
    text.draw(at: NSPoint(x: CGFloat(height) * 0.5, y: (CGFloat(height) - bounds.height) / 2))
    NSGraphicsContext.restoreGraphicsState()
    try! bitmap.representation(using: .png, properties: [:])!
        .write(to: URL(fileURLWithPath: arguments[index]))
    index += 2
}
