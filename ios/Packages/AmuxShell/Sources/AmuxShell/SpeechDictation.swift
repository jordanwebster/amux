import AmuxCore
import AVFoundation
import Speech

/// Owns microphone access for the visible conversation. Audio never falls
/// back to network recognition when a local recognizer is missing.
@MainActor
final class SpeechDictation {
    private var engine: AVAudioEngine?
    private var recognition: SFSpeechRecognitionTask?
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var preparation: Task<Void, Never>?
    private var generation = 0
    private var ownsAudio = false
    private weak var conversation: ConversationStore?

    func toggle(_ store: ConversationStore) {
        if store.dictation.active {
            stop()
            return
        }
        stop()
        conversation = store
        let recognizer = SFSpeechRecognizer()
        let available = recognizer?.isAvailable == true && recognizer?.supportsOnDeviceRecognition == true
        store.dictation.prepare(speech: speechPermission, microphone: microphonePermission, available: available)
        guard store.dictation.active, let recognizer else { return }
        let current = generation
        preparation = Task { [weak self, weak store] in
            guard let self, let store else { return }
            if speechPermission == .notAsked {
                _ = await withCheckedContinuation { continuation in
                    SFSpeechRecognizer.requestAuthorization { status in
                        continuation.resume(returning: status == .authorized)
                    }
                }
            }
            guard current == generation, !Task.isCancelled else { return }
            if speechPermission == .allowed && microphonePermission == .notAsked {
                _ = await AVAudioApplication.requestRecordPermission()
            }
            guard current == generation, !Task.isCancelled else { return }
            store.dictation.prepare(
                speech: speechPermission, microphone: microphonePermission,
                available: recognizer.isAvailable && recognizer.supportsOnDeviceRecognition)
            guard store.dictation.phase == .starting else { return }
            do {
                let session = AVAudioSession.sharedInstance()
                try session.setCategory(.record, mode: .measurement, options: .duckOthers)
                try session.setActive(true)
                ownsAudio = true
                let engine = AVAudioEngine()
                let request = SFSpeechAudioBufferRecognitionRequest()
                request.requiresOnDeviceRecognition = true
                request.shouldReportPartialResults = true
                let input = engine.inputNode
                let format = input.outputFormat(forBus: 0)
                guard format.sampleRate > 0, format.channelCount > 0 else {
                    stop()
                    store.dictation.failed()
                    return
                }
                self.engine = engine
                self.request = request
                input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
                    request.append(buffer)
                }
                engine.prepare()
                try engine.start()
                store.dictation.began(draft: store.draft)
                recognition = recognizer.recognitionTask(with: request) { [weak self, weak store] result, error in
                    let text = result?.bestTranscription.formattedString
                    let final = result?.isFinal == true
                    let failed = error != nil
                    Task { @MainActor in
                        guard let self, let store, current == self.generation else { return }
                        if let text { store.dictation.receive(text, draft: &store.draft) }
                        if final || failed || !store.dictation.active {
                            self.stop()
                            if failed && !final { store.dictation.failed() }
                        }
                    }
                }
            } catch {
                stop()
                store.dictation.failed()
            }
        }
    }

    func stop() {
        generation += 1
        preparation?.cancel()
        preparation = nil
        engine?.stop()
        if let engine { engine.inputNode.removeTap(onBus: 0) }
        engine = nil
        request?.endAudio()
        request = nil
        recognition?.cancel()
        recognition = nil
        if conversation?.dictation.active == true { conversation?.dictation.stop() }
        conversation = nil
        if ownsAudio {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            ownsAudio = false
        }
    }

    private var speechPermission: DictationState.Permission {
        switch SFSpeechRecognizer.authorizationStatus() {
        case .notDetermined: .notAsked
        case .authorized: .allowed
        case .denied, .restricted: .denied
        @unknown default: .denied
        }
    }

    private var microphonePermission: DictationState.Permission {
        switch AVAudioApplication.shared.recordPermission {
        case .undetermined: .notAsked
        case .granted: .allowed
        case .denied: .denied
        @unknown default: .denied
        }
    }
}
