import AmuxDesign

/// One design token, deliberately moved.
///
/// A golden suite nobody has ever seen fail is not evidence that it would.
/// This is how it is shown to: the app is asked to draw a screen with one
/// colour token replaced, the capture is compared with the baseline as
/// usual, and the run has to fail with a difference image. Nothing outside a
/// debug build can ask for it, and nothing inside one asks by itself.
enum Perturbation {
    /// The colour every perturbed token is moved to: a magenta that appears
    /// nowhere in the design, so the check is about whether a difference is
    /// noticed at all rather than about where the tolerance sits.
    static let moved = Ramp(0xFF00AA, 0xFF00AA)

    /// The design with one named colour token moved, or nothing when the
    /// design has no token by that name.
    static func design(_ base: Design, moving token: String) -> Design? {
        guard base.colours.contains(where: { $0.name == token }) else { return nil }
        func ramp(_ name: String, _ original: Ramp) -> Ramp {
            name == token ? moved : original
        }
        return Design(
            name: base.name,
            ground: ramp("ground", base.ground),
            raised: ramp("raised", base.raised),
            sunken: ramp("sunken", base.sunken),
            hairline: ramp("hairline", base.hairline),
            ink: ramp("ink", base.ink),
            inkMuted: ramp("inkMuted", base.inkMuted),
            inkFaint: ramp("inkFaint", base.inkFaint),
            accent: ramp("accent", base.accent),
            onAccent: ramp("onAccent", base.onAccent),
            added: ramp("added", base.added),
            removed: ramp("removed", base.removed),
            faces: base.faces,
            metrics: base.metrics,
            type: base.type,
            surfaces: base.surfaces)
    }
}
