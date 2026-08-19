use rovr_types::{LayoutKind, Rect, WindowId};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct LayoutRequest<'a> {
    pub area: Rect,
    pub windows: &'a [WindowId],
    pub gap: f64,
    pub padding: f64,
    pub split_ratio: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub window: WindowId,
    pub frame: Rect,
}

#[derive(Debug, Error, PartialEq)]
pub enum LayoutError {
    #[error("gap and padding must be non-negative")]
    NegativeSpacing,
    #[error("split ratio must be between 0.1 and 0.9")]
    InvalidSplitRatio,
    #[error("layout area is too small after padding")]
    AreaTooSmall,
}

pub fn compute(
    kind: LayoutKind,
    request: LayoutRequest<'_>,
) -> Result<Vec<Placement>, LayoutError> {
    validate(&request)?;
    let area = inset(request.area, request.padding)?;
    if request.windows.is_empty() {
        return Ok(vec![]);
    }

    let placements = match kind {
        LayoutKind::Bsp => bsp(request.windows, area, request.gap, true),
        LayoutKind::Stack | LayoutKind::Monocle => overlay(request.windows, area),
        LayoutKind::Master => master(request.windows, area, request.gap, request.split_ratio),
        LayoutKind::Columns => columns(request.windows, area, request.gap),
        LayoutKind::Float => vec![],
    };
    Ok(placements)
}

fn validate(request: &LayoutRequest<'_>) -> Result<(), LayoutError> {
    if request.gap < 0.0 || request.padding < 0.0 {
        return Err(LayoutError::NegativeSpacing);
    }
    if !(0.1..=0.9).contains(&request.split_ratio) {
        return Err(LayoutError::InvalidSplitRatio);
    }
    Ok(())
}

fn inset(area: Rect, padding: f64) -> Result<Rect, LayoutError> {
    let width = area.width - padding * 2.0;
    let height = area.height - padding * 2.0;
    if width <= 0.0 || height <= 0.0 {
        return Err(LayoutError::AreaTooSmall);
    }
    Ok(Rect {
        x: area.x + padding,
        y: area.y + padding,
        width,
        height,
    })
}

fn overlay(windows: &[WindowId], area: Rect) -> Vec<Placement> {
    windows
        .iter()
        .copied()
        .map(|window| Placement {
            window,
            frame: area,
        })
        .collect()
}

fn columns(windows: &[WindowId], area: Rect, gap: f64) -> Vec<Placement> {
    let count = windows.len() as f64;
    let total_gap = gap * (count - 1.0).max(0.0);
    let width = ((area.width - total_gap) / count).max(0.0);

    windows
        .iter()
        .copied()
        .enumerate()
        .map(|(index, window)| Placement {
            window,
            frame: Rect {
                x: area.x + index as f64 * (width + gap),
                y: area.y,
                width,
                height: area.height,
            },
        })
        .collect()
}

fn master(windows: &[WindowId], area: Rect, gap: f64, ratio: f64) -> Vec<Placement> {
    if windows.len() == 1 {
        return overlay(windows, area);
    }

    let master_width = (area.width - gap) * ratio;
    let stack_width = area.width - gap - master_width;
    let mut placements = vec![Placement {
        window: windows[0],
        frame: Rect {
            x: area.x,
            y: area.y,
            width: master_width,
            height: area.height,
        },
    }];

    let stack = &windows[1..];
    let stack_count = stack.len() as f64;
    let total_gap = gap * (stack_count - 1.0).max(0.0);
    let height = ((area.height - total_gap) / stack_count).max(0.0);
    placements.extend(
        stack
            .iter()
            .copied()
            .enumerate()
            .map(|(index, window)| Placement {
                window,
                frame: Rect {
                    x: area.x + master_width + gap,
                    y: area.y + index as f64 * (height + gap),
                    width: stack_width,
                    height,
                },
            }),
    );
    placements
}

fn bsp(windows: &[WindowId], area: Rect, gap: f64, vertical: bool) -> Vec<Placement> {
    match windows {
        [] => vec![],
        [window] => vec![Placement {
            window: *window,
            frame: area,
        }],
        _ => {
            let midpoint = windows.len().div_ceil(2);
            let (first, second) = windows.split_at(midpoint);
            let (first_area, second_area) = split(area, gap, vertical);
            let mut placements = bsp(first, first_area, gap, !vertical);
            placements.extend(bsp(second, second_area, gap, !vertical));
            placements
        }
    }
}

fn split(area: Rect, gap: f64, vertical: bool) -> (Rect, Rect) {
    if vertical {
        let first_width = ((area.width - gap) / 2.0).max(0.0);
        let second_width = (area.width - gap - first_width).max(0.0);
        (
            Rect {
                x: area.x,
                y: area.y,
                width: first_width,
                height: area.height,
            },
            Rect {
                x: area.x + first_width + gap,
                y: area.y,
                width: second_width,
                height: area.height,
            },
        )
    } else {
        let first_height = ((area.height - gap) / 2.0).max(0.0);
        let second_height = (area.height - gap - first_height).max(0.0);
        (
            Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: first_height,
            },
            Rect {
                x: area.x,
                y: area.y + first_height + gap,
                width: area.width,
                height: second_height,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(windows: &'a [WindowId]) -> LayoutRequest<'a> {
        LayoutRequest {
            area: Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 800.0,
            },
            windows,
            gap: 10.0,
            padding: 10.0,
            split_ratio: 0.6,
        }
    }

    #[test]
    fn columns_cover_each_window_once() {
        let windows = [WindowId(1), WindowId(2), WindowId(3)];
        let placements = compute(LayoutKind::Columns, request(&windows)).unwrap();
        assert_eq!(placements.len(), windows.len());
        for id in windows {
            assert_eq!(placements.iter().filter(|p| p.window == id).count(), 1);
        }
    }

    #[test]
    fn bsp_frames_stay_inside_padded_area() {
        let windows = [
            WindowId(1),
            WindowId(2),
            WindowId(3),
            WindowId(4),
            WindowId(5),
        ];
        let placements = compute(LayoutKind::Bsp, request(&windows)).unwrap();
        for placement in placements {
            assert!(placement.frame.x >= 10.0);
            assert!(placement.frame.y >= 10.0);
            assert!(placement.frame.x + placement.frame.width <= 990.0 + f64::EPSILON);
            assert!(placement.frame.y + placement.frame.height <= 790.0 + f64::EPSILON);
            assert!(placement.frame.width >= 0.0);
            assert!(placement.frame.height >= 0.0);
        }
    }

    #[test]
    fn master_assigns_largest_left_area_to_first_window() {
        let windows = [WindowId(1), WindowId(2), WindowId(3)];
        let placements = compute(LayoutKind::Master, request(&windows)).unwrap();
        assert_eq!(placements[0].window, WindowId(1));
        assert!(placements[0].frame.width > placements[1].frame.width);
        assert_eq!(placements[1].frame.width, placements[2].frame.width);
    }

    #[test]
    fn float_does_not_reposition_windows() {
        let windows = [WindowId(1)];
        assert!(compute(LayoutKind::Float, request(&windows))
            .unwrap()
            .is_empty());
    }
}
