// Foreground mask of a photo with Apple Vision (on-device, uses the Neural Engine).
//   swift tools/mask.swift photo.jpg mask.png
// Writes a grayscale PNG the size of the photo: 255 on the subject, 0 elsewhere.
import AppKit
import CoreImage
import Vision

let args = CommandLine.arguments
guard args.count == 3 else {
    FileHandle.standardError.write("usage: mask.swift photo.jpg mask.png\n".data(using: .utf8)!)
    exit(2)
}
let input = URL(fileURLWithPath: args[1])
let output = URL(fileURLWithPath: args[2])
guard let image = CIImage(contentsOf: input) else { fatalError("cannot read \(input.path)") }

let request = VNGenerateForegroundInstanceMaskRequest()
let handler = VNImageRequestHandler(ciImage: image)
try handler.perform([request])
guard let result = request.results?.first else { fatalError("no subject found") }
// Merge every instance Vision found; a pet photo has one subject.
let buffer = try result.generateScaledMaskForImage(forInstances: result.allInstances, from: handler)
let mask = CIImage(cvPixelBuffer: buffer)
let ctx = CIContext()
guard let cg = ctx.createCGImage(mask, from: image.extent) else { fatalError("render mask") }
let rep = NSBitmapImageRep(cgImage: cg)
try rep.representation(using: .png, properties: [:])!.write(to: output)
print("\(output.path): \(cg.width)x\(cg.height), \(result.allInstances.count) subject(s)")
