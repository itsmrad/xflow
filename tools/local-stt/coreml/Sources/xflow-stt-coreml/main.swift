import CoreML
import FluidAudio
import Foundation

private let helperVersion = "0.2.1-beta"
private let maxHeader = 65_536
private let maxPayload = 41_943_040
private let maxSamples = 19_200_000
private let pcmRate = 16_000
private let requiredFiles = [
    "Preprocessor.mlmodelc", "Encoder.mlmodelc", "Decoder.mlmodelc",
    "JointDecisionv3.mlmodelc", "parakeet_vocab.json",
]

private let input = FileHandle.standardInput
private let output = FileHandle.standardOutput
// AsrModels.load reads <parent>/<repo folder>. Removed when serve() returns.
nonisolated(unsafe) private var scratchURL: URL?

private enum Read {
    case eof, short, word(UInt32), bytes(Data)
}

private struct FrameError: Error {}

private func log(_ message: String) {
    try? FileHandle.standardError.write(contentsOf: Data((message + "\n").utf8))
}

private func readExact(_ count: Int) throws -> Read {
    var data = Data()
    data.reserveCapacity(count)
    while data.count < count {
        guard let chunk = try input.read(upToCount: count - data.count), !chunk.isEmpty else {
            return data.isEmpty ? .eof : .short
        }
        data.append(chunk)
    }
    return .bytes(data)
}

private func readU32() throws -> Read {
    switch try readExact(4) {
    case .bytes(let data):
        let value = data.withUnsafeBytes { raw -> UInt32 in
            let b = raw.bindMemory(to: UInt8.self)
            return UInt32(b[0]) | (UInt32(b[1]) << 8) | (UInt32(b[2]) << 16) | (UInt32(b[3]) << 24)
        }
        return .word(value)
    case .eof:
        return .eof
    case .short:
        return .short
    case .word(_):
        return .short
    }
}

private func writeFrame(_ object: [String: Any]) throws {
    let header = try JSONSerialization.data(withJSONObject: object)
    var headerLen = UInt32(header.count).littleEndian
    var payloadLen = UInt32(0).littleEndian
    try output.write(contentsOf: Data(bytes: &headerLen, count: 4))
    try output.write(contentsOf: header)
    try output.write(contentsOf: Data(bytes: &payloadLen, count: 4))
    // No synchronize(): fsync on a pipe fails with EINVAL, and pipe writes are unbuffered.
}

private func whole(_ value: Any?) -> Double? {
    guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
    let double = number.doubleValue
    return double.rounded() == double ? double : nil
}

private func jsonInt(_ value: Any?) -> Int? {
    guard let double = whole(value), double >= Double(Int.min), double <= Double(Int.max) else { return nil }
    return Int(double)
}

private func jsonUInt(_ value: Any?) -> UInt64? {
    guard let double = whole(value), double >= 0, double <= Double(UInt64.max) else { return nil }
    return UInt64(double)
}

private func reject(_ status: Int32, _ code: String, _ message: String, id: UInt64? = nil) -> Int32 {
    var object: [String: Any] = ["op": "error", "code": code, "message": message]
    if let id { object["id"] = NSNumber(value: id) }
    try? writeFrame(object)
    log(code)
    return status
}

private func floats(_ payload: Data) -> [Float] {
    let count = payload.count / 2
    var samples = [Float](repeating: 0, count: count)
    payload.withUnsafeBytes { raw in
        samples.withUnsafeMutableBufferPointer { dst in
            for index in 0..<count {
                let lo = UInt16(raw[index * 2])
                let hi = UInt16(raw[index * 2 + 1]) << 8
                dst[index] = Float(Int16(bitPattern: lo | hi)) / 32768
            }
        }
    }
    return samples
}

private func modelDirectory(_ modelDir: String) -> URL {
    URL(fileURLWithPath: modelDir, isDirectory: true).standardizedFileURL
}

private func missingFile(_ modelDir: String) -> String? {
    var isDirectory: ObjCBool = false
    let url = modelDirectory(modelDir)
    guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory), isDirectory.boolValue else {
        return "The model directory does not exist."
    }
    for name in requiredFiles where !FileManager.default.fileExists(atPath: url.appendingPathComponent(name).path) {
        return "The model directory is missing \(name)."
    }
    return nil
}

private func loadURL(_ modelDir: String) throws -> URL {
    let direct = modelDirectory(modelDir)
    let folder = Repo.parakeetV3.folderName
    if direct.lastPathComponent == folder { return direct }
    let scratch = FileManager.default.temporaryDirectory
        .appendingPathComponent("xflow-stt-\(ProcessInfo.processInfo.processIdentifier)", isDirectory: true)
    try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
    scratchURL = scratch
    let link = scratch.appendingPathComponent(folder, isDirectory: true)
    try FileManager.default.createSymbolicLink(at: link, withDestinationURL: direct)
    return link
}

private func loadFailure(_ error: Error) -> (String, String) {
    if let error = error as? DownloadError, case .modelMissing = error {
        return ("model_missing", "The model directory is missing required files.")
    }
    if let error = error as? AsrModelsError {
        switch error {
        case .modelNotFound:
            return ("model_missing", "A required model file is missing.")
        case .modelCompilationFailed:
            return ("model_invalid", "The model files are not a usable Core ML model.")
        default:
            break
        }
    }
    return ("load_failed", "The model could not be loaded.")
}

private func audioFailure(_ error: Error) -> (String, String) {
    if let error = error as? ASRError, case .invalidAudioData = error {
        return ("bad_audio", "The audio is shorter than the 300 ms the model accepts.")
    }
    return ("inference_failed", "Transcription failed.")
}

private func readFrame() throws -> (op: String, object: [String: Any], payload: Data)? {
    switch try readU32() {
    case .eof:
        return nil
    case .word(let headerLen) where headerLen >= 1 && headerLen <= UInt32(maxHeader):
        guard case .bytes(let header) = try readExact(Int(headerLen)) else { throw FrameError() }
        guard case .word(let payloadLen) = try readU32(), payloadLen <= UInt32(maxPayload) else {
            throw FrameError()
        }
        let payload: Data
        if payloadLen == 0 {
            payload = Data()
        } else {
            guard case .bytes(let body) = try readExact(Int(payloadLen)) else { throw FrameError() }
            payload = body
        }
        let json = try? JSONSerialization.jsonObject(with: header)
        guard let object = json as? [String: Any], let op = object["op"] as? String else {
            throw FrameError()
        }
        return (op, object, payload)
    default:
        throw FrameError()
    }
}

private func transcribe(_ object: [String: Any], _ payload: Data, _ manager: AsrManager) async -> Int32 {
    guard let id = jsonUInt(object["id"]) else {
        return reject(2, "protocol", "transcribe requires an unsigned id")
    }
    guard jsonInt(object["sample_rate"]) == pcmRate else {
        return reject(0, "bad_audio", "sample_rate must be 16000", id: id)
    }
    guard !payload.isEmpty, payload.count.isMultiple(of: 2) else {
        return reject(0, "bad_audio", "Audio must be non-empty 16-bit PCM.", id: id)
    }
    guard payload.count / 2 <= maxSamples else {
        return reject(0, "bad_audio", "Audio is longer than 20 minutes.", id: id)
    }
    let samples = floats(payload)
    var state = TdtDecoderState.make()
    let started = Date()
    do {
        let result = try await manager.transcribe(samples, decoderState: &state)
        try writeFrame([
            "op": "result",
            "id": NSNumber(value: id),
            "text": result.text,
            "audio_ms": samples.count * 1000 / pcmRate,
            "compute_ms": Int(Date().timeIntervalSince(started) * 1000),
        ])
        return 0
    } catch {
        log("transcription failed")
        let (code, message) = audioFailure(error)
        return reject(0, code, message, id: id)
    }
}

private func requests(_ manager: AsrManager) async -> Int32 {
    while true {
        let frame: (op: String, object: [String: Any], payload: Data)
        do {
            guard let next = try readFrame() else { return 0 }
            frame = next
        } catch {
            return reject(2, "protocol", "The frame is not valid protocol.")
        }
        switch frame.op {
        case "shutdown":
            guard frame.payload.isEmpty else {
                return reject(2, "protocol", "shutdown payload must be empty")
            }
            return 0
        case "transcribe":
            let status = await transcribe(frame.object, frame.payload, manager)
            if status != 0 { return status }
        default:
            return reject(2, "protocol", "Unknown op.")
        }
    }
}

private func hello(_ object: [String: Any], _ payload: Data) async -> Int32 {
    guard payload.isEmpty else { return reject(1, "protocol", "hello payload must be empty") }
    guard jsonInt(object["protocol"]) == 1 else { return reject(1, "protocol", "protocol must be 1") }
    guard let modelDir = object["model_dir"] as? String, modelDir.hasPrefix("/") else {
        return reject(1, "protocol", "model_dir must be an absolute path")
    }
    if let missing = missingFile(modelDir) { return reject(1, "model_missing", missing) }
    do {
        try writeFrame(["op": "progress", "stage": "loading"])
        let started = Date()
        let models = try await AsrModels.load(
            from: loadURL(modelDir),
            configuration: AsrModels.defaultConfiguration(),
            version: .v3,
            encoderPrecision: .int8,
            encoderComputeUnits: .cpuAndNeuralEngine
        )
        let manager = AsrManager()
        try await manager.loadModels(models)
        let loadMs = Int(Date().timeIntervalSince(started) * 1000)
        try writeFrame(["op": "ready", "protocol": 1, "helper": helperVersion, "load_ms": loadMs])
        return await requests(manager)
    } catch {
        log("load failed")
        let (code, message) = loadFailure(error)
        return reject(1, code, message)
    }
}

private func serve() async -> Int32 {
    defer {
        if let scratchURL { try? FileManager.default.removeItem(at: scratchURL) }
    }
    let frame: (op: String, object: [String: Any], payload: Data)
    do {
        guard let next = try readFrame() else { return 0 }
        frame = next
    } catch {
        return reject(2, "protocol", "The frame is not valid protocol.")
    }
    switch frame.op {
    case "hello":
        return await hello(frame.object, frame.payload)
    case "shutdown":
        guard frame.payload.isEmpty else {
            return reject(2, "protocol", "shutdown payload must be empty")
        }
        return 0
    default:
        return reject(2, "protocol", "The first request must be hello.")
    }
}

ModelHub.offlineMode = true
let arguments = Array(CommandLine.arguments.dropFirst())
if arguments == ["--version"] {
    let line = "{\"helper\":\"\(helperVersion)\",\"protocol\":1}\n"
    try? output.write(contentsOf: Data(line.utf8))
    try? output.synchronize()
    exit(0)
}
if !arguments.isEmpty {
    log("unknown argument")
    exit(2)
}
let status = await serve()
if status != 0 { exit(status) }
