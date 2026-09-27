import UIKit

/// The controls that present a menu, and the actions their menus hold.
enum MenuSource {
    /// The control whose menu offers an item titled `label`, and that item.
    @MainActor
    static func item(_ label: String, in view: UIView) -> (UIButton, UIAction)? {
        if let button = view as? UIButton, let menu = button.menu,
           let action = action(label, in: menu) {
            return (button, action)
        }
        for subview in view.subviews {
            if let found = item(label, in: subview) { return found }
        }
        return nil
    }

    private static func action(_ label: String, in menu: UIMenu) -> UIAction? {
        for child in menu.children {
            if let action = child as? UIAction, action.title == label { return action }
            if let nested = child as? UIMenu, let found = action(label, in: nested) { return found }
        }
        return nil
    }

    /// Runs an action as the menu would when its row is tapped.
    @MainActor
    static func run(_ action: UIAction, from button: UIButton) {
        let perform = NSSelectorFromString("performWithSender:target:")
        guard action.responds(to: perform) else { return }
        _ = action.perform(perform, with: button, with: nil)
    }
}
