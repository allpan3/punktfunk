import CoreGraphics

extension Comparable {
    /// Clamp into `range`. One definition for the whole module: the absolute-cursor mapping, the
    /// pen's pressure and tilt, the gamepad wire's axis ranges and the settings sliders all want
    /// it, and four private copies is four places for a future edge-case fix to miss.
    func clamped(to range: ClosedRange<Self>) -> Self {
        Swift.min(Swift.max(self, range.lowerBound), range.upperBound)
    }
}
