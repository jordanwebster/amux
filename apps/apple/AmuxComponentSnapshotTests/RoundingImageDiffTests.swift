import SnapshotTesting
import UIKit
import XCTest

final class RoundingImageDiffTests: XCTestCase {
    private let diffing = RoundingImageDiff.allowingChannelRounding(.image(scale: 1))

    func testIdenticalImagesMatch() throws {
        let reference = try image([100, 110, 120, 255])
        XCTAssertNil(diffing.diffV2(reference, reference))
    }

    func testOneLevelRoundingInEveryColorChannelMatches() throws {
        let reference = try image([100, 110, 120, 255, 100, 110, 120, 255])
        let actual = try image([101, 109, 121, 255, 99, 111, 119, 255])
        XCTAssertNotNil(Diffing<UIImage>.image(scale: 1).diffV2(reference, actual))
        XCTAssertNil(diffing.diffV2(reference, actual))
    }

    func testOneLargerChannelDifferenceFailsAndRetainsAttachments() throws {
        let reference = try image([100, 110, 120, 255, 100, 110, 120, 255])
        for channel in 0..<3 {
            var pixels: [UInt8] = [100, 110, 120, 255, 100, 110, 120, 255]
            pixels[channel] += 2
            let failure = try XCTUnwrap(diffing.diffV2(reference, try image(pixels)))
            XCTAssertFalse(failure.1.isEmpty)
        }
    }

    func testLargerAlphaDifferenceFails() throws {
        XCTAssertNotNil(diffing.diffV2(
            try image([0, 0, 0, 255]), try image([0, 0, 0, 253])))
    }

    func testDifferentDimensionsFail() throws {
        XCTAssertNotNil(diffing.diffV2(
            try image([100, 110, 120, 255]),
            try image([100, 110, 120, 255, 100, 110, 120, 255])))
    }

    private func image(_ rgba: [UInt8]) throws -> UIImage {
        let provider = try XCTUnwrap(CGDataProvider(data: Data(rgba) as CFData))
        let colorSpace = try XCTUnwrap(CGColorSpace(name: CGColorSpace.sRGB))
        let cgImage = try XCTUnwrap(CGImage(
            width: rgba.count / 4, height: 1, bitsPerComponent: 8, bitsPerPixel: 32,
            bytesPerRow: rgba.count, space: colorSpace,
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent))
        return UIImage(cgImage: cgImage, scale: 1, orientation: .up)
    }
}
