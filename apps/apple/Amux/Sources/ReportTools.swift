import AmuxCore
import AmuxFeatures
import SwiftUI
import UIKit

/// Reporting a problem, over the whole app, in every build.
///
/// Two ways in and no others. The system says a screenshot was taken, and the
/// app freezes its own frame and offers to report it; or somebody goes to Help
/// and asks. Nothing is shaken: a shake is already undo, and a phone in a
/// pocket shakes all day.
struct ReportTools: ViewModifier {
    let composition: Composition

    func body(content: Content) -> some View {
        let reports = composition.reports
        content
            .reportOffer(
                reports.offering,
                take: { reports.accept() },
                dismiss: { reports.dismiss() })
            .overlay {
                if reports.open {
                    ReportScreen(
                        model: reports,
                        signedIn: composition.accounts.selectedAccount?.signedIn == true
                    ) { asked in
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
                reports.offer(composition.freezer)
            }
    }
}

extension Composition {
    func beginReport() {
        reports.begin(freezer)
    }

    func sendReport() {
        // Filed under the account on screen, and only one that is signed in:
        // an account listed with Sign In beside it has no session to send
        // with, and amux.sh would refuse it after the person had pressed Send.
        let account = accounts.selectedAccount.flatMap { $0.signedIn ? $0.id : nil }
        Task {
            await reports.send(
                with: cloud, as: account, build: AppFiles.build,
                gitSHA: AppFiles.gitSHA, log: AppFiles.logTail)
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
