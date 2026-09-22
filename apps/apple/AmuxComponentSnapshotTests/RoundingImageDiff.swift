import SnapshotTesting
import UIKit

enum RoundingImageDiff {
    /// Native rasterizers can round a border channel differently across hosts.
    /// Permit one 8-bit level per channel, never a percentage of arbitrary
    /// mismatching pixels: even one larger difference must still fail.
    static func allowingChannelRounding(_ exact: Diffing<UIImage>) -> Diffing<UIImage> {
        var result = exact
        result.diffV2 = { reference, actual in
            guard let failure = exact.diffV2(reference, actual) else { return nil }
            return differsOnlyByRounding(reference, actual) ? nil : failure
        }
        return result
    }

    private static func differsOnlyByRounding(_ reference: UIImage, _ actual: UIImage) -> Bool {
        guard let expected = reference.cgImage, let taken = actual.cgImage,
            expected.width > 0, expected.height > 0,
            expected.width == taken.width, expected.height == taken.height,
            let expectedBytes = normalizedBytes(expected),
            let takenBytes = normalizedBytes(taken)
        else { return false }

        var index = 0
        while index < expectedBytes.count {
            if abs(Int(expectedBytes[index]) - Int(takenBytes[index])) > 1 { return false }
            index += 1
        }
        return true
    }

    private static func normalizedBytes(_ image: CGImage) -> [UInt8]? {
        var bytes = [UInt8](repeating: 0, count: image.width * image.height * 4)
        let drawn = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
                let context = CGContext(
                    data: buffer.baseAddress,
                    width: image.width, height: image.height,
                    bitsPerComponent: 8, bytesPerRow: image.width * 4,
                    space: colorSpace,
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            return true
        }
        return drawn ? bytes : nil
    }
}
