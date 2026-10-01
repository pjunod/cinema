//! Exact display-fit calculations for normalized square-pixel candidates.
//! Coded decoder dimensions are deliberately separate from displayed aspect.

use super::candidate::PresentationTarget;

/// Upright displayed aspect, after applying source SAR and rotation once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayAspect {
    width: u64,
    height: u64,
}

impl DisplayAspect {
    /// Missing or unsupported geometry remains unknown. Rotation follows FFprobe display-matrix degrees.
    pub fn from_source(
        coded_width: u32,
        coded_height: u32,
        sar: Option<(u32, u32)>,
        rotation_degrees: Option<i32>,
    ) -> Option<Self> {
        let (sar_num, sar_den) = sar?;
        if coded_width == 0 || coded_height == 0 || sar_num == 0 || sar_den == 0 {
            return None;
        }
        let mut width = u64::from(coded_width) * u64::from(sar_num);
        let mut height = u64::from(coded_height) * u64::from(sar_den);
        match rotation_degrees?.rem_euclid(360) {
            0 | 180 => {}
            90 | 270 => std::mem::swap(&mut width, &mut height),
            _ => return None,
        }
        Some(Self { width, height })
    }

    /// Both fitted axes must require at most the agreed 1.10 enlargement.
    /// Cross-products preserve the boundary without rounding the fit down.
    pub fn covered_by(self, target: PresentationTarget, output: (u32, u32)) -> Option<bool> {
        let (container_width, container_height) = target.rectangle()?;
        let (output_width, output_height) = output;
        if output_width == 0 || output_height == 0 {
            return None;
        }
        let aspect_width = u128::from(self.width);
        let aspect_height = u128::from(self.height);
        let cw = u128::from(container_width);
        let ch = u128::from(container_height);
        let ow = u128::from(output_width);
        let oh = u128::from(output_height);
        Some(if cw * aspect_height <= ch * aspect_width {
            cw * 10 <= ow * 11 && cw * aspect_height * 10 <= oh * aspect_width * 11
        } else {
            ch * aspect_width * 10 <= ow * aspect_height * 11 && ch * 10 <= oh * 11
        })
    }

    /// Production scale dimensions: even square-pixel width for an even height.
    /// This never grants coverage; callers check the actual returned dimensions.
    pub fn output_at_height(self, height: u32) -> Option<(u32, u32)> {
        let height = height / 2 * 2;
        if height == 0 {
            return None;
        }
        let width = u128::from(height) * u128::from(self.width) / u128::from(self.height);
        let width = u32::try_from(width).ok()? / 2 * 2;
        (width > 0).then_some((width, height))
    }

    /// Fit an even square-pixel raster within the source's upright square-pixel
    /// presentation ceiling, rather than incorrectly using anamorphic coded axes.
    pub fn output_within(self, max_width: u32, max_height: u32) -> Option<(u32, u32)> {
        let (width, height) = self.output_at_height(max_height)?;
        if width <= max_width {
            return Some((width, height));
        }
        let width = max_width / 2 * 2;
        let height =
            u32::try_from(u128::from(width) * u128::from(self.height) / u128::from(self.width))
                .ok()?
                / 2
                * 2;
        (width > 0 && height > 0).then_some((width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(width: i64, height: i64) -> PresentationTarget {
        PresentationTarget {
            width_px: width,
            height_px: height,
            revision: 1,
        }
    }

    #[test]
    fn exact_ten_percent_boundary_does_not_expand_after_rounding() {
        let aspect =
            DisplayAspect::from_source(3840, 2160, Some((1, 1)), Some(0)).expect("known source");
        assert_eq!(
            aspect.covered_by(target(2112, 1600), (1920, 1080)),
            Some(true)
        );
        assert_eq!(
            aspect.covered_by(target(2113, 1600), (1920, 1080)),
            Some(false)
        );
        assert_eq!(
            aspect.covered_by(target(2400, 1600), (1920, 1080)),
            Some(false)
        );
        assert_eq!(
            aspect.covered_by(target(2400, 1600), (2560, 1440)),
            Some(true)
        );
        assert_eq!(
            aspect.covered_by(target(2000, 1200), (1920, 1080)),
            Some(true)
        );
        assert_eq!(
            aspect.covered_by(target(1080, 2340), (1280, 720)),
            Some(true)
        );
    }

    #[test]
    fn displayed_aspect_uses_sar_and_rotation_without_changing_coded_dimensions() {
        let anamorphic =
            DisplayAspect::from_source(1440, 1080, Some((4, 3)), Some(0)).expect("known SAR");
        assert_eq!(anamorphic.output_at_height(1080), Some((1920, 1080)));
        let rotated =
            DisplayAspect::from_source(1920, 1080, Some((1, 1)), Some(90)).expect("known rotation");
        assert_eq!(rotated.output_at_height(1920), Some((1080, 1920)));
        let scope =
            DisplayAspect::from_source(3840, 1600, Some((1, 1)), Some(0)).expect("scope source");
        assert_eq!(scope.output_at_height(1001), Some((2400, 1000)));
        assert!(DisplayAspect::from_source(1920, 1080, None, Some(0)).is_none());
        assert!(DisplayAspect::from_source(1920, 1080, Some((1, 1)), None).is_none());
        assert!(DisplayAspect::from_source(1920, 1080, Some((1, 1)), Some(45)).is_none());
        assert_eq!(scope.covered_by(target(i64::MAX, 1), (2400, 1000)), None);
    }
}
