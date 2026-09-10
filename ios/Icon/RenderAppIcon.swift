// The app icon, drawn rather than stored as artwork nobody can change.
//
// One 1024×1024 PNG is the whole icon: the asset catalog compiler derives
// every size iOS and the App Store ask for from it. Keeping the drawing as
// code means the mark can be adjusted — a hue, a stroke, the spacing of the
// strands — without a design tool, and that the file in the catalog is
// always exactly what this file says.
//
// Run it with `wt run icon`, which writes
// ios/Amux/Assets.xcassets/AppIcon.appiconset/AppIcon.png.
//
// What it draws: amux multiplexes. Many agent sessions, on many machines,
// reached from one place. So the mark is a merge — two channels curving into
// a bright spine that carries them out the other side. The ground is the
// app's own near-black and the strands its accent teal, so the icon on the
// home screen is the same two colours as the first screen it opens.

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let side = 1024.0

// The app's palette, from AmuxDesign: the dark end of the neutral ramp and
// the dark-appearance accent. An icon is always seen against a wallpaper
// rather than a page, so it uses the dark appearance in both.
let groundTop = (0x1A, 0x21, 0x2B)
let groundBottom = (0x06, 0x08, 0x0B)
let spineLeft = (0x35, 0x9E, 0xB6)
let spineRight = (0x7A, 0xDD, 0xEE)
let strand = (0x2E, 0x7C, 0x90)
let glow = (0x4F, 0xBA, 0xCD)

func components(_ colour: (Int, Int, Int), _ alpha: Double = 1) -> [CGFloat] {
    [CGFloat(colour.0) / 255, CGFloat(colour.1) / 255, CGFloat(colour.2) / 255,
     CGFloat(alpha)]
}

let space = CGColorSpaceCreateDeviceRGB()

// No alpha channel: the App Store rejects an icon that has one, and a
// transparent pixel in a home screen icon has nothing to show through to
// anyway. `noneSkipLast` is what makes the PNG come out as plain RGB.
guard let canvas = CGContext(
    data: nil, width: Int(side), height: Int(side),
    bitsPerComponent: 8, bytesPerRow: 0, space: space,
    bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else {
    FileHandle.standardError.write(Data("could not open a canvas\n".utf8))
    exit(1)
}

func gradient(_ from: (Int, Int, Int), _ to: (Int, Int, Int)) -> CGGradient {
    CGGradient(colorSpace: space,
               colorComponents: components(from) + components(to),
               locations: [0, 1], count: 2)!
}

// The ground. Barely a gradient — enough that the icon has a top and a
// bottom under a glossy wallpaper, not enough to read as a colour of its own.
canvas.drawLinearGradient(gradient(groundTop, groundBottom),
                          start: CGPoint(x: 0, y: side),
                          end: CGPoint(x: 0, y: 0), options: [])

let middle = side / 2
let inset = 236.0            // where the channels enter
let outlet = side - inset    // where the spine leaves
let junction = 576.0         // where the curves have finished merging
let offset = 192.0           // how far the outer channels start from centre

// A wash of accent behind the junction, so the merge has somewhere to happen.
// It is well under the ground's own contrast: at icon sizes it is felt as
// depth rather than seen as a shape.
canvas.saveGState()
canvas.drawRadialGradient(
    CGGradient(colorSpace: space,
               colorComponents: components(glow, 0.20) + components(glow, 0),
               locations: [0, 1], count: 2)!,
    startCenter: CGPoint(x: junction, y: middle), startRadius: 0,
    endCenter: CGPoint(x: junction, y: middle), endRadius: 380,
    options: [])
canvas.restoreGState()

canvas.setLineCap(.round)
canvas.setLineJoin(.round)

// The three channels coming in. Two curve; the middle one runs straight, and
// all three stop inside the outgoing spine so the spine covers their ends and
// the merge has no seam in it.
canvas.setStrokeColor(CGColor(colorSpace: space,
                              components: components(strand))!)
canvas.setLineWidth(88)
for direction in [1.0, 0.0, -1.0] {
    let entry = middle + direction * offset
    let path = CGMutablePath()
    path.move(to: CGPoint(x: inset, y: entry))
    if direction == 0 {
        path.addLine(to: CGPoint(x: junction, y: middle))
    } else {
        path.addLine(to: CGPoint(x: inset + 164, y: entry))
        path.addCurve(to: CGPoint(x: junction, y: middle),
                      control1: CGPoint(x: inset + 280, y: entry),
                      control2: CGPoint(x: junction - 72, y: middle))
    }
    canvas.addPath(path)
    canvas.strokePath()
}

// The one channel out: thicker than what feeds it, brightening as it goes,
// and starting left of the junction so it covers the merge. This is the whole
// idea of the icon — three ways in, one way to reach them.
let thickness = 112.0
let spine = CGRect(x: junction - 112, y: middle - thickness / 2,
                   width: outlet + thickness / 2 - (junction - 112),
                   height: thickness)
canvas.saveGState()
canvas.addPath(CGPath(roundedRect: spine, cornerWidth: thickness / 2,
                      cornerHeight: thickness / 2, transform: nil))
canvas.clip()
canvas.drawLinearGradient(gradient(spineLeft, spineRight),
                          start: CGPoint(x: spine.minX, y: 0),
                          end: CGPoint(x: spine.maxX, y: 0), options: [])
canvas.restoreGState()

let destination = CommandLine.arguments.count > 1
    ? CommandLine.arguments[1] : "AppIcon.png"
guard let image = canvas.makeImage(),
      let file = CGImageDestinationCreateWithURL(
        URL(fileURLWithPath: destination) as CFURL,
        UTType.png.identifier as CFString, 1, nil)
else {
    FileHandle.standardError.write(Data("could not write \(destination)\n".utf8))
    exit(1)
}
CGImageDestinationAddImage(file, image, nil)
guard CGImageDestinationFinalize(file) else {
    FileHandle.standardError.write(Data("could not finish \(destination)\n".utf8))
    exit(1)
}
print("wrote \(destination)")
