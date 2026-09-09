import AmuxCore
import SwiftUI
import UIKit

/// Reporting belongs to the debug app, including the system notification
/// subscription. Shared navigation never links its views or capture models.
struct DebugReports: ViewModifier {
    let composition: Composition

    func body(content: Content) -> some View {
        content
            .reportOffer(
                composition.reports?.offering == true,
                take: { composition.reports?.accept() },
                dismiss: { composition.reports?.dismiss() })
            .overlay {
                if let reports = composition.reports, reports.open {
                    ReportScreen(model: reports) { asked in
                        switch asked {
                        case .cancel: reports.dismiss()
                        case .send: composition.sendReport()
                        }
                    }
                }
            }
            .onReceive(NotificationCenter.default.publisher(
                for: UIApplication.userDidTakeScreenshotNotification)
            ) { _ in
                guard let reports = composition.reports, let freezer = composition.freezer else { return }
                reports.offer(freezer)
            }
    }
}

extension Composition {
    func beginReport() {
        guard let reports, let freezer else { return }
        reports.begin(freezer)
    }

    func sendReport() {
        guard let reports else { return }
        Task {
            await reports.send(
                with: cloud, as: accounts.selected, build: AppFiles.build,
                log: AppFiles.logTail)
        }
    }
}

extension AppFiles {
    /// The tail of this app's own log, or why there is none.
    ///
    /// There is none. The app logs through the system, which keeps its records
    /// in a store no app may read back — not even its own — so there is no
    /// file to take a tail of. The part is declared absent with that reason
    /// rather than left out, because a reader who found no log needs to know
    /// whether it was withheld, lost, or never existed.
    static var logTail: Result<String, PartAbsent> {
        .failure(PartAbsent("this app logs through the system, which keeps no file it can read back"))
    }

}
