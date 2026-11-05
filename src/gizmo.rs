use eframe::egui::Vec2;

/// Convert a pointer displacement to world units along a projected axis.
pub fn axis_amount(drag: Vec2, pixels_per_unit: Vec2) -> f32 {
    drag.dot(pixels_per_unit) / pixels_per_unit.length_sq().max(0.0001)
}
pub fn segment_distance(point: Vec2, end: Vec2) -> f32 {
    let t = (point.dot(end) / end.length_sq().max(0.0001)).clamp(0.0, 1.0);
    (point - end * t).length()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::V, render::Camera};
    #[test]
    fn dragging_a_projected_axis_tracks_world_units_at_different_views() {
        for yaw in [0.0, 0.55, 1.2] {
            for zoom in [0.5, 1.0, 3.0] {
                let c = Camera {
                    yaw,
                    zoom,
                    pitch: 0.3,
                    ..Default::default()
                };
                let anchor = V(15.0, 10.0, -50.0);
                let a = c.project(anchor, 800, 700);
                for axis in [V(1.0, 0.0, 0.0), V(0.0, 1.0, 0.0), V(0.0, 0.0, 1.0)] {
                    let b = c.project(anchor.add(axis), 800, 700);
                    let per_unit = Vec2::new(b[0] - a[0], b[1] - a[1]);
                    assert!((axis_amount(per_unit * 7.0, per_unit) - 7.0).abs() < 0.0001);
                    let perpendicular = Vec2::new(-per_unit.y, per_unit.x) * 5.0;
                    assert!(
                        (axis_amount(per_unit * 7.0 + perpendicular, per_unit) - 7.0).abs()
                            < 0.0001
                    );
                }
            }
        }
    }
    #[test]
    fn world_axes_foreshorten_instead_of_becoming_screen_diagonals() {
        let anchor = V(0.0, -5.0, 0.0);
        let front = Camera::default();
        let side = Camera {
            yaw: std::f32::consts::FRAC_PI_2,
            ..Default::default()
        };
        let length = |c: &Camera, axis: V| {
            let a = c.project(anchor, 800, 700);
            let b = c.project(anchor.add(axis.mul(30.0)), 800, 700);
            Vec2::new(b[0] - a[0], b[1] - a[1]).length()
        };
        assert!(length(&front, V(0.0, 0.0, 1.0)) < 0.001);
        assert!(length(&front, V(1.0, 0.0, 0.0)) > 50.0);
        assert!(length(&side, V(1.0, 0.0, 0.0)) < 0.001);
        assert!(length(&side, V(0.0, 0.0, 1.0)) > 50.0);
    }
    #[test]
    fn camera_aligned_axes_are_a_separate_view_relative_basis() {
        for yaw in [-1.0, 0.0, 1.0] {
            for pitch in [-0.7, 0.0, 0.7] {
                let c = Camera {
                    yaw,
                    pitch,
                    ..Default::default()
                };
                for a in [V(1.0, 0.0, 0.0), V(0.0, 1.0, 0.0), V(0.0, 0.0, 1.0)] {
                    assert!(c.rotate(c.unrotate(a)).sub(a).len() < 0.000001);
                }
            }
        }
    }
}
