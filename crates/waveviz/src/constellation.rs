//! Constellation renderer — IQ scatter plot with persistence.
//!
//! Renders IQ sample pairs as a scatter plot, showing modulation
//! characteristics. Supports persistence (alpha-blended trails of
//! previous frames) for visualizing signal dynamics.
//!
//! GPU rendering uses a vertex buffer of IQ pairs. The vertex shader
//! maps IQ coordinates to clip space within a symmetric range.
//! The fragment shader renders colored circles with optional
//! persistence via alpha blending.

/// Configuration for the constellation renderer.
#[derive(Debug, Clone)]
pub struct ConstellationConfig {
    /// Symmetric axis range: display covers [-range, +range].
    pub range: f32,
    /// Point size in pixels.
    pub point_size: f32,
    /// Enable persistence (alpha-blended trails).
    pub persistence: bool,
    /// Alpha decay per frame (0.0 = instant, 1.0 = permanent).
    pub decay: f32,
}

impl Default for ConstellationConfig {
    fn default() -> Self {
        Self {
            range: 1.0,
            point_size: 2.0,
            persistence: true,
            decay: 0.05,
        }
    }
}

/// CPU-side constellation renderer state.
pub struct ConstellationRenderer {
    config: ConstellationConfig,
    /// Current IQ points.
    points: Vec<(f32, f32)>,
}

impl ConstellationRenderer {
    /// Create a new constellation renderer.
    pub fn new(config: ConstellationConfig) -> Self {
        Self {
            config,
            points: Vec::new(),
        }
    }

    /// Update the constellation points.
    pub fn update(&mut self, points: &[(f32, f32)]) {
        self.points = points.to_vec();
    }

    /// Number of current points.
    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// Get current points.
    pub fn points(&self) -> &[(f32, f32)] {
        &self.points
    }

    /// Get the configuration.
    pub fn config(&self) -> &ConstellationConfig {
        &self.config
    }

    /// Set the display range.
    pub fn set_range(&mut self, range: f32) {
        self.config.range = range.max(0.001);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constellation_config_default() {
        let config = ConstellationConfig::default();
        assert!(config.persistence);
        assert_eq!(config.range, 1.0);
    }

    #[test]
    fn constellation_update_and_count() {
        let mut renderer = ConstellationRenderer::new(ConstellationConfig::default());
        assert_eq!(renderer.point_count(), 0);

        let points = vec![(0.5, 0.3), (-0.2, 0.1), (0.0, -0.5)];
        renderer.update(&points);
        assert_eq!(renderer.point_count(), 3);
        assert_eq!(renderer.points()[1], (-0.2, 0.1));
    }

    #[test]
    fn constellation_set_range() {
        let mut renderer = ConstellationRenderer::new(ConstellationConfig::default());
        renderer.set_range(2.0);
        assert_eq!(renderer.config().range, 2.0);

        // Should clamp to minimum
        renderer.set_range(-1.0);
        assert!(renderer.config().range > 0.0);
    }

    #[test]
    fn constellation_replace_points() {
        let mut renderer = ConstellationRenderer::new(ConstellationConfig::default());
        renderer.update(&[(1.0, 0.0)]);
        assert_eq!(renderer.point_count(), 1);

        renderer.update(&[(0.0, 1.0), (1.0, 1.0)]);
        assert_eq!(renderer.point_count(), 2);
        assert_eq!(renderer.points()[0], (0.0, 1.0));
    }
}
