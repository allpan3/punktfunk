// The tail both codec repackers share: allocate one CMBlockBuffer, let the caller write its own
// per-unit layout into it, and wrap the result as a CMSampleBuffer.
//
// AnnexB (length-prefixed NALs) and AV1 (header + leb128-sized OBUs) differ only in that write
// loop; everything around it — the block create, the destination-length check, the timing, the
// sample create — was duplicated line for line, so a fix to either had to be made twice.

import CoreMedia
import Foundation

enum SamplePack {
    /// One sample of exactly `total` bytes, filled by `write`.
    ///
    /// `write` receives the block's base pointer and must fill all `total` bytes — the caller
    /// computed that size in its own first pass over the same access unit, so a mismatch is a bug
    /// in that pass rather than a runtime condition.
    static func sample(
        total: Int, ptsNs: UInt64, format: CMVideoFormatDescription,
        write: (UnsafeMutableRawPointer) -> Void
    ) -> CMSampleBuffer? {
        guard total > 0 else { return nil }
        var blockBuffer: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil,
            blockLength: total, blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil, offsetToData: 0, dataLength: total,
            flags: kCMBlockBufferAssureMemoryNowFlag, blockBufferOut: &blockBuffer) == noErr,
            let block = blockBuffer
        else { return nil }

        var dstLen = 0
        var dstPtr: UnsafeMutablePointer<CChar>?
        guard CMBlockBufferGetDataPointer(
            block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &dstLen,
            dataPointerOut: &dstPtr) == noErr,
            dstLen == total, let dstPtr
        else { return nil }
        write(UnsafeMutableRawPointer(dstPtr))

        var timing = CMSampleTimingInfo(
            duration: .invalid,
            presentationTimeStamp: CMTime(value: Int64(ptsNs), timescale: 1_000_000_000),
            decodeTimeStamp: .invalid)
        var sampleSize = total
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreate(
            allocator: kCFAllocatorDefault, dataBuffer: block, dataReady: true,
            makeDataReadyCallback: nil, refcon: nil, formatDescription: format,
            sampleCount: 1, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
            sampleSizeEntryCount: 1, sampleSizeArray: &sampleSize,
            sampleBufferOut: &sample) == noErr,
            let sample
        else { return nil }

        // For the stage-1 AVSampleBufferDisplayLayer path: render on arrival rather than against
        // a clock. Inert on the stage-2 decode path, which paces frames itself.
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                dict,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }
}
